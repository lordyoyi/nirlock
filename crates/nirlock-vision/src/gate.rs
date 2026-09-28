//! Quality gate and pose proxies — a 1:1 port of `estimate_pose`,
//! `quality_gate` and `frame_stats` from the reference implementation
//! (`phase0/pipeline.cpp`). The daemon must accept and reject exactly the
//! frames fuprobe accepted and rejected, because every threshold in
//! `docs/DESIGN.md` §2.5 and every number in the lab's `PHASE0-RESULTS.md`
//! was measured through this logic.

use crate::image::GrayImage;
use crate::yunet::Face;

/// Mean, 99th percentile and saturated fraction of a region.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub mean: f64,
    pub p99: u8,
    /// Fraction of pixels `>= 250`.
    pub sat_frac: f64,
}

/// Statistics over the whole image.
pub fn frame_stats(img: &GrayImage) -> FrameStats {
    stats_of(img.as_slice().iter().copied(), img.as_slice().len())
}

/// Statistics over `(x, y, w, h)` in pixels, clipped to the image. An
/// empty intersection yields the default (all zeros), as `cv::Rect::area()
/// <= 0` did.
pub fn frame_stats_in(img: &GrayImage, x: f32, y: f32, w: f32, h: f32) -> FrameStats {
    // The C++ built a cv::Rect from int() truncation of the float box and
    // intersected it with the image; reproduce both steps.
    let x0 = (x as i64).max(0);
    let y0 = (y as i64).max(0);
    let x1 = (x as i64 + w as i64).min(img.width() as i64);
    let y1 = (y as i64 + h as i64).min(img.height() as i64);
    if x1 <= x0 || y1 <= y0 {
        return FrameStats::default();
    }
    let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
    let stride = img.width();
    let px = img.as_slice();
    let n = (x1 - x0) * (y1 - y0);
    stats_of(
        (y0..y1).flat_map(|row| px[row * stride + x0..row * stride + x1].iter().copied()),
        n,
    )
}

fn stats_of(px: impl Iterator<Item = u8>, total: usize) -> FrameStats {
    let mut hist = [0u64; 256];
    let mut sum = 0u64;
    for v in px {
        hist[v as usize] += 1;
        sum += u64::from(v);
    }
    let mut s = FrameStats::default();
    if total == 0 {
        return s;
    }
    s.mean = sum as f64 / total as f64;
    let mut acc = 0u64;
    for (i, &c) in hist.iter().enumerate() {
        acc += c;
        if acc as f64 >= 0.99 * total as f64 {
            s.p99 = i as u8;
            break;
        }
    }
    s.sat_frac = hist[250..].iter().sum::<u64>() as f64 / total as f64;
    s
}

/// Pose proxies from the five landmarks only (no 3D model), measured in
/// the FACE frame so that yaw and pitch are decoupled from in-plane roll.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    /// Angle of the eye line in the image, degrees.
    pub roll_deg: f64,
    /// `(dL - dR) / (dL + dR)` of the nose-to-eye distances projected on
    /// the eye line. 0 frontal, **not** bounded by ±1.
    pub yaw: f64,
    /// `(nose - eyes)·n / (mouth - eyes)·n`, about 0.5–0.6 frontal.
    pub pitch: f64,
}

pub fn estimate_pose(f: &Face) -> Pose {
    let (re, le, nose, rm, lm) = (f.lm[0], f.lm[1], f.lm[2], f.lm[3], f.lm[4]);
    let (ex, ey) = ((le[0] - re[0]) as f64, (le[1] - re[1]) as f64);
    let ipd = ex.hypot(ey);
    let mut p = Pose {
        roll_deg: ey.atan2(ex).to_degrees(),
        ..Pose::default()
    };
    if ipd < 1e-6 {
        // Degenerate landmarks: report a yaw that fails the gate.
        p.yaw = 1.0;
        return p;
    }
    let (ux, uy) = (ex / ipd, ey / ipd);
    let (nx, ny) = (-uy, ux);
    let along = |x: f64, y: f64| x * ux + y * uy;
    let down = |x: f64, y: f64| x * nx + y * ny;
    let dl = along((nose[0] - re[0]) as f64, (nose[1] - re[1]) as f64);
    let dr = along((le[0] - nose[0]) as f64, (le[1] - nose[1]) as f64);
    let den = dl + dr; // == ipd
    p.yaw = if den.abs() > 1e-6 {
        (dl - dr) / den
    } else {
        1.0
    };
    let (emx, emy) = (0.5 * (re[0] + le[0]) as f64, 0.5 * (re[1] + le[1]) as f64);
    let (mmx, mmy) = (0.5 * (rm[0] + lm[0]) as f64, 0.5 * (rm[1] + lm[1]) as f64);
    let h = down(mmx - emx, mmy - emy);
    p.pitch = if h.abs() > 1e-6 {
        down(nose[0] as f64 - emx, nose[1] as f64 - emy) / h
    } else {
        0.0
    };
    p
}

/// Thresholds of DESIGN §2.5. The defaults are the Phase-0 values every
/// measurement in `PHASE0-RESULTS.md` was taken with; changing one
/// invalidates the impostor thresholds of ADR-0007.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateConfig {
    pub min_score: f32,
    /// `min(w, h)` of the face box, pixels at 640x360.
    pub min_box: f32,
    /// Fraction of box pixels `>= 250` in the LIT frame. 0.15 since
    /// ADR-0016: at 0.05 the dark-room AE regime (a clipped face that never
    /// unclips) cost 4 of 40 unlocks in E4, while clipped genuine frames
    /// still score 0.64-0.84 and clipped impostor frames peak at 0.23, so
    /// the impostor ceiling does not move at all.
    pub max_sat_in_box: f64,
    /// Exposure floor inside the box. 20.0 and not 35: at 8 lux a converged
    /// face box reads 35–39, so 35 flipped good frames between accepted and
    /// underexposed and added 133 ms steps to E4.
    pub min_box_mean: f64,
    pub max_roll_deg: f64,
    pub max_abs_yaw: f64,
    pub min_pitch: f64,
    pub max_pitch: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_score: 0.75,
            min_box: 64.0,
            max_sat_in_box: 0.15,
            min_box_mean: 20.0,
            max_roll_deg: 20.0,
            max_abs_yaw: 0.35,
            min_pitch: 0.30,
            max_pitch: 0.85,
        }
    }
}

/// Why a frame was not accepted. `NoFace` is the default so that an empty
/// detection list needs no special case, as in the C++.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reject {
    None,
    #[default]
    NoFace,
    MultiFace,
    LowScore,
    SmallBox,
    Saturated,
    Underexposed,
    Pose,
}

impl Reject {
    /// The names fuprobe wrote to its CSVs, so replay comparisons are a
    /// string equality.
    pub fn name(self) -> &'static str {
        match self {
            Reject::None => "accepted",
            Reject::NoFace => "no_face",
            Reject::MultiFace => "multi_face",
            Reject::LowScore => "low_score",
            Reject::SmallBox => "small_box",
            Reject::Saturated => "saturated",
            Reject::Underexposed => "underexposed",
            Reject::Pose => "pose",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GateResult {
    pub reason: Reject,
    pub sat_in_box: f64,
    pub box_mean: f64,
    pub pose: Pose,
}

impl GateResult {
    pub fn pass(&self) -> bool {
        self.reason == Reject::None
    }
}

/// `faces` must be sorted by descending score (as `Detector::detect`
/// returns them). `exposure_src` is the image the saturation and exposure
/// are measured on: for the `diff` variant pass the LIT frame, because
/// saturation is a property of the sensor exposure and a clipped lit frame
/// makes the difference meaningless.
pub fn quality_gate(faces: &[Face], exposure_src: &GrayImage, cfg: &GateConfig) -> GateResult {
    let mut g = GateResult::default(); // NoFace
    let Some(f) = faces.first() else {
        return g;
    };
    let s = frame_stats_in(exposure_src, f.x, f.y, f.w, f.h);
    g.sat_in_box = s.sat_frac;
    g.box_mean = s.mean;
    g.pose = estimate_pose(f);
    // Ordered so the recorded reason is the most fundamental one.
    if f.score < cfg.min_score {
        g.reason = Reject::LowScore;
        return g;
    }
    if f.min_side() < cfg.min_box {
        g.reason = Reject::SmallBox;
        return g;
    }
    // A second credible face makes "who is unlocking" ambiguous.
    if faces[1..]
        .iter()
        .any(|o| o.score >= cfg.min_score && o.min_side() >= cfg.min_box)
    {
        g.reason = Reject::MultiFace;
        return g;
    }
    if s.sat_frac > cfg.max_sat_in_box {
        g.reason = Reject::Saturated;
        return g;
    }
    if s.mean < cfg.min_box_mean {
        g.reason = Reject::Underexposed;
        return g;
    }
    if g.pose.roll_deg.abs() > cfg.max_roll_deg
        || g.pose.yaw.abs() > cfg.max_abs_yaw
        || g.pose.pitch < cfg.min_pitch
        || g.pose.pitch > cfg.max_pitch
    {
        g.reason = Reject::Pose;
        return g;
    }
    g.reason = Reject::None;
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frontal face: eyes level, nose centred, mouth below.
    fn frontal() -> Face {
        Face {
            x: 100.0,
            y: 100.0,
            w: 100.0,
            h: 120.0,
            lm: [
                [130.0, 140.0], // right eye
                [170.0, 140.0], // left eye
                [150.0, 165.0], // nose
                [135.0, 190.0], // right mouth
                [165.0, 190.0], // left mouth
            ],
            score: 0.9,
        }
    }

    fn rotate(f: &Face, deg: f64) -> Face {
        let (s, c) = deg.to_radians().sin_cos();
        let (cx, cy) = (150.0f64, 165.0f64);
        let mut out = *f;
        for (i, p) in f.lm.iter().enumerate() {
            let (dx, dy) = (p[0] as f64 - cx, p[1] as f64 - cy);
            out.lm[i] = [(cx + dx * c - dy * s) as f32, (cy + dx * s + dy * c) as f32];
        }
        out
    }

    #[test]
    fn pose_is_frontal_and_independent_of_in_plane_roll() {
        let f = frontal();
        let p = estimate_pose(&f);
        assert!(p.roll_deg.abs() < 1e-6, "{p:?}");
        assert!(p.yaw.abs() < 1e-6, "{p:?}");
        assert!((0.45..=0.55).contains(&p.pitch), "{p:?}");

        // The whole point of measuring in the face frame: rotating the
        // landmarks in the image plane moves roll and nothing else. Using
        // raw image x distances instead would read yaw ~ -1.14*tan(r).
        let base = p;
        for deg in [-30.0, -20.0, -10.0, 10.0, 20.0, 30.0] {
            let q = estimate_pose(&rotate(&f, deg));
            assert!((q.roll_deg - deg).abs() < 1e-4, "roll {deg}: {q:?}");
            assert!(q.yaw.abs() < 1e-5, "yaw must not follow roll {deg}: {q:?}");
            assert!(
                (q.pitch - base.pitch).abs() < 1e-5,
                "pitch must not follow roll {deg}: {q:?}"
            );
        }
    }

    #[test]
    fn yaw_grows_with_the_nose_leaving_the_eye_centre_and_degenerates_safely() {
        let mut f = frontal();
        f.lm[2][0] = 160.0; // nose towards the subject's left eye
        let p = estimate_pose(&f);
        assert!(p.yaw > 0.45 && p.yaw < 0.55, "{p:?}");
        // Past the eye span |yaw| exceeds 1, as the doc comment warns.
        f.lm[2][0] = 190.0;
        assert!(estimate_pose(&f).yaw > 1.0);
        // Both eyes in the same place: no face frame, fails the gate.
        f.lm[1] = f.lm[0];
        assert_eq!(estimate_pose(&f).yaw, 1.0);
    }

    fn img(w: usize, h: usize, v: u8) -> GrayImage {
        GrayImage::from_vec(w, h, vec![v; w * h]).unwrap()
    }

    #[test]
    fn stats_are_measured_inside_the_clipped_box() {
        let mut px = vec![10u8; 40 * 40];
        for y in 10..20 {
            for x in 10..20 {
                px[y * 40 + x] = 255; // a 10x10 saturated patch
            }
        }
        let im = GrayImage::from_vec(40, 40, px).unwrap();
        let s = frame_stats_in(&im, 10.0, 10.0, 10.0, 10.0);
        assert_eq!((s.mean, s.sat_frac, s.p99), (255.0, 1.0, 255));
        let s = frame_stats_in(&im, 0.0, 0.0, 40.0, 40.0);
        assert!((s.sat_frac - 100.0 / 1600.0).abs() < 1e-12);
        // Whole-image helper agrees with a full-size box.
        assert_eq!(frame_stats(&im), s);
        // A box partly outside is clipped; one fully outside is empty.
        assert_eq!(frame_stats_in(&im, 35.0, 35.0, 20.0, 20.0).mean, 10.0);
        assert_eq!(
            frame_stats_in(&im, 40.0, 0.0, 10.0, 10.0),
            FrameStats::default()
        );
        assert_eq!(
            frame_stats_in(&im, -5.0, -5.0, 3.0, 3.0),
            FrameStats::default()
        );
    }

    #[test]
    fn gate_reports_the_most_fundamental_reason_first() {
        let cfg = GateConfig::default();
        let bright = img(640, 360, 120);
        assert_eq!(quality_gate(&[], &bright, &cfg).reason, Reject::NoFace);

        let mut f = frontal();
        f.x = 0.0;
        f.y = 0.0;
        f.w = 100.0;
        f.h = 100.0;
        assert_eq!(quality_gate(&[f], &bright, &cfg).reason, Reject::None);

        // Score is checked before size, size before a second face.
        let mut low = f;
        low.score = 0.5;
        low.w = 10.0;
        low.h = 10.0;
        assert_eq!(quality_gate(&[low], &bright, &cfg).reason, Reject::LowScore);
        let mut small = f;
        small.w = 60.0;
        small.h = 60.0;
        assert_eq!(
            quality_gate(&[small], &bright, &cfg).reason,
            Reject::SmallBox
        );

        // A second credible face is ambiguous; a weak or tiny one is not.
        assert_eq!(
            quality_gate(&[f, f], &bright, &cfg).reason,
            Reject::MultiFace
        );
        assert_eq!(quality_gate(&[f, low], &bright, &cfg).reason, Reject::None);
        assert_eq!(
            quality_gate(&[f, small], &bright, &cfg).reason,
            Reject::None
        );

        // Exposure: saturation first, then the floor.
        let blown = img(640, 360, 255);
        assert_eq!(quality_gate(&[f], &blown, &cfg).reason, Reject::Saturated);
        let dark = img(640, 360, 5);
        assert_eq!(quality_gate(&[f], &dark, &cfg).reason, Reject::Underexposed);

        // Pose is last, and each limit fires on its own.
        let rolled = rotate(&f, 25.0);
        assert_eq!(quality_gate(&[rolled], &bright, &cfg).reason, Reject::Pose);
        let mut yawed = f;
        yawed.lm[2][0] = 160.0;
        assert_eq!(quality_gate(&[yawed], &bright, &cfg).reason, Reject::Pose);
        let mut nodded = f;
        nodded.lm[2][1] = 185.0; // nose almost at the mouth line
        assert_eq!(quality_gate(&[nodded], &bright, &cfg).reason, Reject::Pose);
    }

    /// The Phase-0 values, except `max_sat_in_box`, relaxed by ADR-0016 on
    /// measured evidence. Any change here invalidates the impostor
    /// thresholds of ADR-0007 unless it is argued the same way.
    #[test]
    fn defaults_are_the_phase0_values_every_measurement_was_taken_with() {
        let c = GateConfig::default();
        assert_eq!(
            (c.min_score, c.min_box, c.max_sat_in_box, c.min_box_mean),
            (0.75, 64.0, 0.15, 20.0)
        );
        assert_eq!(
            (c.max_roll_deg, c.max_abs_yaw, c.min_pitch, c.max_pitch),
            (20.0, 0.35, 0.30, 0.85)
        );
        assert_eq!(Reject::default(), Reject::NoFace);
        assert_eq!(Reject::None.name(), "accepted");
    }
}
