//! The request engine: everything between a `verify` and its `result`.
//!
//! One camera, one request in flight (DESIGN §2.8). Models are loaded once
//! and stay resident (ADR-0004: AuraFace takes ~650 ms to load on battery,
//! which cannot hide behind the 253 ms camera start). The per-frame path is
//! the one `nirlock-replay` proved equal to the reference implementation:
//! metadata label → usable check → detect → gate → align → embed →
//! max-cosine over the template → K-of-W window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nirlock_cam::{
    Camera, FrameSource, HwProfile, IrCapture, OpenOptions, RealSys, RgbAssist, select,
    node_paths,
};
use nirlock_vision::embed::{Embedder, Kind};
use nirlock_vision::gate::{GateConfig, quality_gate};
use nirlock_vision::image::GrayImage;
use nirlock_vision::template::Template;
use nirlock_vision::yunet::Detector;

/// Detector floor: deliberately low so near misses are visible to the
/// audit; the gate applies `GateConfig::min_score` on top.
const DETECTOR_FLOOR: f32 = 0.30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Accept,
    NoMatch,
    NoFace,
    Timeout,
}

impl Decision {
    pub fn reason(self) -> &'static str {
        match self {
            Decision::Accept => "match",
            Decision::NoMatch => "no_match",
            Decision::NoFace => "no_face",
            Decision::Timeout => "timeout",
        }
    }
}

/// What one `verify` produced, in the shape the audit log and the bench
/// both want.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub decision: Decision,
    /// Milliseconds from the instant just before `open()` (`t0`), the same
    /// zero as every latency figure in `docs/BENCH.md`.
    pub streamon_ms: f64,
    pub first_frame_ms: f64,
    pub first_gate_pass_ms: f64,
    pub first_embedding_ms: f64,
    pub decision_ms: f64,
    pub best_score: f64,
    pub lit_frames: u32,
    pub scored_frames: u32,
    /// Frames scored strictly below the threshold: the population the
    /// charge-first rule of PROTOCOL §7 persists before answering.
    pub below_threshold: u32,
    pub rejects: Vec<(&'static str, u32)>,
    /// Pose of the last gate-rejected frame. A `pose` rejection is useless
    /// in the journal without the numbers: it cannot say whether the head
    /// was turned, tilted, or just outside a limit by a hair.
    pub last_reject_pose: Option<(f64, f64, f64)>,
    pub rgb_assist: RgbAssist,
}

/// Decision rule of ADR-0008: accept once `k` of the last `window`
/// gate-passed lit frames scored at or above `threshold`, with each hit
/// expiring after `hit_ttl_ms` so that two hits minutes apart never add up.
#[derive(Clone, Copy, Debug)]
pub struct DecisionConfig {
    pub threshold: f64,
    pub k: usize,
    pub window: usize,
    pub hit_ttl_ms: f64,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            threshold: 0.45,
            k: 2,
            window: 4,
            hit_ttl_ms: 800.0,
        }
    }
}

pub struct Engine {
    camera: Camera,
    profile: HwProfile,
    detector: Detector,
    embedder: Embedder,
    /// One enrolment per user, under `<templates>/<user>/`, cached with the
    /// modification time of its manifest. Re-reading the rows on every
    /// unlock would be waste, but caching them forever is worse: after
    /// someone re-enrols, the daemon would go on comparing against the old
    /// face until it happened to be restarted, which is a confusing way to
    /// tell a person their new enrolment does not work.
    templates_dir: PathBuf,
    templates: HashMap<String, (Template, Option<std::time::SystemTime>)>,
    embedder_name: &'static str,
    pub gate: GateConfig,
    pub decision: DecisionConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("camera: {0}")]
    Cam(#[from] nirlock_cam::Error),
    #[error("vision: {0}")]
    Vision(#[from] nirlock_vision::Error),
    #[error("no face enrolled for {0}")]
    NotEnrolled(String),
}

impl Error {
    /// The `unavailable <reason>` token of PROTOCOL.md for this failure.
    ///
    /// The server used to answer a hardcoded `"camera_error"` for every
    /// engine failure, so a camera held by a browser came back as a generic
    /// error while DESIGN §2.3 and §2.9, PROTOCOL.md and the rescue guide all
    /// promised `camera_busy` — and the guide tells the user that token means
    /// another application has the camera, sending them after the wrong
    /// thing. `nirlock_cam::Error` already knows its own token; carry it.
    pub fn reason(&self) -> &'static str {
        match self {
            Error::Cam(e) => e.reason(),
            Error::Vision(_) => "internal",
            Error::NotEnrolled(_) => "not_enrolled",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl Engine {
    /// Discovers and pins the camera, then loads every model. Called once,
    /// at `prewarm`: the whole point is that a later `verify` finds them
    /// warm (ADR-0004).
    pub fn load(
        models: &Path,
        templates_dir: &Path,
        embedder: Kind,
        threads: usize,
    ) -> Result<Self> {
        // Which camera, and which profile claims it. Both come from the
        // installed profile set, so a machine with a different Windows Hello
        // camera works as soon as a profile for it exists.
        let (camera, profile, profile_problems) = select(None)?;
        for problem in &profile_problems {
            eprintln!("nirlockd: ignoring hardware profile {problem}");
        }
        let detector = Detector::load(
            &models.join("face_detection_yunet_2026may.onnx"),
            1,
            DETECTOR_FLOOR,
        )?;
        let mut embedder_session =
            Embedder::load(embedder, &models.join(embedder.file_name()), threads)?;
        // A warm-up forward so the first real frame does not pay the lazy
        // graph initialisation (M0 measured +1-20 ms; on battery more).
        let _ = embedder_session.warm_up();
        Ok(Self {
            camera,
            profile,
            detector,
            embedder: embedder_session,
            templates_dir: templates_dir.to_path_buf(),
            templates: HashMap::new(),
            embedder_name: embedder.name(),
            gate: GateConfig::default(),
            decision: DecisionConfig::default(),
        })
    }

    pub fn camera_paths(&self) -> (PathBuf, PathBuf, PathBuf) {
        node_paths(&self.camera)
    }

    /// Runs one request. `budget_ms` bounds everything, including the
    /// camera open and any EBUSY retries (PROTOCOL §7).
    fn template(&mut self, user: &str) -> Result<&Template> {
        // The user name comes from the peer's uid, not from the wire, so it
        // cannot contain a path; refuse anything odd anyway.
        if user.is_empty() || user.contains(['/', '.']) {
            return Err(Error::NotEnrolled(user.to_string()));
        }
        let dir = self.templates_dir.join(user);
        let stamp = std::fs::metadata(dir.join("manifest.json"))
            .and_then(|m| m.modified())
            .ok();
        let stale = match self.templates.get(user) {
            None => true,
            Some((_, cached)) => *cached != stamp,
        };
        if stale {
            let t = Template::load(&dir).map_err(|_| Error::NotEnrolled(user.to_string()))?;
            self.templates.insert(user.to_string(), (t, stamp));
        }
        self.templates
            .get(user)
            .map(|(t, _)| t)
            .ok_or_else(|| Error::NotEnrolled(user.to_string()))
    }

    pub fn verify(&mut self, user: &str, budget_ms: u32, rgb: bool) -> Result<Verdict> {
        // Fail before touching the camera if there is nothing to compare to.
        self.template(user)?;
        let opts = OpenOptions {
            rgb,
            ..OpenOptions::default()
        };
        let t0 = Instant::now();
        let mut cap = IrCapture::open(&self.camera, &self.profile, opts, RealSys)?;
        let ms = |i: Instant| i.duration_since(t0).as_secs_f64() * 1e3;

        let mut v = Verdict {
            decision: Decision::Timeout,
            streamon_ms: cap.timing().streamon_ms(),
            first_frame_ms: f64::NAN,
            first_gate_pass_ms: f64::NAN,
            first_embedding_ms: f64::NAN,
            decision_ms: f64::NAN,
            best_score: f64::NAN,
            lit_frames: 0,
            scored_frames: 0,
            below_threshold: 0,
            rejects: Vec::new(),
            last_reject_pose: None,
            rgb_assist: RgbAssist::Disabled,
        };
        let mut counts: Vec<(&'static str, u32)> = Vec::new();
        let mut hits: Vec<f64> = Vec::new(); // arrival ms of recent hits
        let mut window: Vec<f64> = Vec::new(); // arrival ms of recent scored frames
        let mut prev_dark: Option<GrayImage> = None;

        let budget = f64::from(budget_ms);
        while ms(Instant::now()) < budget {
            let left = (budget - ms(Instant::now())) / 1e3;
            let Some(f) = cap.next(left.max(0.0))? else {
                break; // timeout
            };
            let arrival = ms(Instant::now());
            if v.first_frame_ms.is_nan() {
                v.first_frame_ms = arrival;
            }
            let Some(lit) = f.meta.lit else {
                continue; // unlabelled frames are never used (DESIGN §2.4)
            };
            if !f.usable() {
                continue;
            }
            let img = GrayImage::from_vec(f.width as usize, f.height as usize, f.pixels)?;
            if !lit {
                prev_dark = Some(img);
                continue;
            }
            v.lit_frames += 1;

            let faces = self.detector.detect(&img)?;
            let g = quality_gate(&faces, &img, &self.gate);
            if !g.pass() {
                bump(&mut counts, g.reason.name());
                v.last_reject_pose = Some((g.pose.roll_deg, g.pose.yaw, g.pose.pitch));
                continue;
            }
            if v.first_gate_pass_ms.is_nan() {
                v.first_gate_pass_ms = arrival;
            }
            let face = faces[0];
            let crop = nirlock_vision::align::align(&img, &face)?;
            let emb = self.embedder.embed(&crop)?;
            let now = ms(Instant::now());
            if v.first_embedding_ms.is_nan() {
                v.first_embedding_ms = now;
            }
            let score = self
                .templates
                .get(user)
                .and_then(|(t, _)| t.find(self.embedder_name, "lit"))
                .and_then(|s| s.max_cosine(&emb))
                .unwrap_or(f64::NAN);
            v.scored_frames += 1;
            if !score.is_nan() && (v.best_score.is_nan() || score > v.best_score) {
                v.best_score = score;
            }
            // `prev_dark` is kept for the `diff` variant of a later
            // version; holding it here documents the pairing the replay
            // used and keeps the dark half from being dropped silently.
            let _ = &prev_dark;

            window.push(now);
            if window.len() > self.decision.window {
                window.remove(0);
            }
            if score >= self.decision.threshold {
                hits.push(now);
            } else {
                // NOT the gate's `low_score`, which is the detector's own
                // confidence: this frame showed a good face that did not
                // match the template well enough.
                v.below_threshold += 1;
                bump(&mut counts, "below_threshold");
            }
            // A hit counts only while it is inside the window (by position)
            // and fresh (by time).
            let oldest = window.first().copied().unwrap_or(now);
            hits.retain(|h| *h >= oldest && now - *h <= self.decision.hit_ttl_ms);
            if hits.len() >= self.decision.k {
                v.decision = Decision::Accept;
                v.decision_ms = now;
                break;
            }
        }

        v.rgb_assist = cap.rgb_assist();
        cap.close();
        v.rejects = counts;
        if v.decision != Decision::Accept {
            v.decision = if v.scored_frames > 0 {
                Decision::NoMatch
            } else if v.lit_frames > 0 {
                Decision::NoFace
            } else {
                Decision::Timeout
            };
            v.decision_ms = ms(Instant::now());
        }
        Ok(v)
    }
}

fn bump(counts: &mut Vec<(&'static str, u32)>, key: &'static str) {
    match counts.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => *n += 1,
        None => counts.push((key, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_defaults_are_adr_0008_and_adr_0007() {
        let d = DecisionConfig::default();
        assert_eq!((d.k, d.window), (2, 4));
        assert_eq!(d.threshold, 0.45); // AuraFace, provisional (ADR-0007)
        assert_eq!(d.hit_ttl_ms, 800.0);
    }

    /// Every token this can produce must be one PROTOCOL.md declares, or a
    /// client sees a word it has no case for.
    #[test]
    fn engine_error_reasons_are_protocol_tokens() {
        const DECLARED: &[&str] = &[
            "stale", "not_enrolled", "template_stale", "disabled", "account_locked",
            "lid_closed", "camera_busy", "camera_missing", "camera_mismatch",
            "camera_format", "camera_ambiguous", "metadata", "models_unavailable",
            "rate_limited", "budget", "busy", "no_session", "suspending", "internal",
        ];
        // The case that cost us: a camera held by another application must
        // say so, because the rescue guide tells the user what that token
        // means and sends them to `fuser`.
        let busy = Error::Cam(nirlock_cam::Error::Busy {
            dev: "/dev/video2".into(),
        });
        assert_eq!(busy.reason(), "camera_busy");
        for e in [
            busy,
            Error::Cam(nirlock_cam::Error::NoMetaNode {
                usb_sysfs: "/sys/x".into(),
            }),
            Error::NotEnrolled("rodrigo".into()),
        ] {
            assert!(DECLARED.contains(&e.reason()), "{e} -> {}", e.reason());
        }
    }

    #[test]
    fn reasons_are_the_protocol_strings() {
        assert_eq!(Decision::Accept.reason(), "match");
        assert_eq!(Decision::NoMatch.reason(), "no_match");
        assert_eq!(Decision::NoFace.reason(), "no_face");
        assert_eq!(Decision::Timeout.reason(), "timeout");
        assert_eq!(nirlock_vision::gate::Reject::Saturated.name(), "saturated");
    }

    #[test]
    fn bump_counts_per_key() {
        let mut c = Vec::new();
        bump(&mut c, "pose");
        bump(&mut c, "saturated");
        bump(&mut c, "pose");
        assert_eq!(c, vec![("pose", 2), ("saturated", 1)]);
    }
}
