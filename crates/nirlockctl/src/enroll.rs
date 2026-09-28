//! `nirlockctl enroll`: guided enrolment.
//!
//! Why this exists rather than a plain recording: a template built from one
//! pose only works from that pose. The first live failure of the project was
//! 29 frames in a row rejected as `pose` after opening the lid, where the
//! head sits higher and the screen is at a different angle than when sitting
//! down to type.
//!
//! Why it is one continuous capture rather than a sequence of prompted
//! poses: the prompted version was written first and was bad in two ways
//! that a live trial exposed at once. It threw away every completed phase
//! when a later one timed out, and it had to *name* directions ("tilt your
//! chin down"), which put my reading of the pitch sign in front of the user
//! — it was backwards, so the prompt and the correction contradicted each
//! other. Asking someone to look around the screen and showing which areas
//! are covered needs no direction names at all, so that whole class of
//! mistake is gone.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nirlock_cam::{FrameSource, IrCapture, OpenOptions, RealSys, select};
use nirlock_vision::embed::{Embedder, Kind};
use nirlock_vision::gate::{GateConfig, Reject, quality_gate};
use nirlock_vision::image::GrayImage;
use nirlock_vision::yunet::Detector;

use crate::preview;

const DETECTOR_FLOOR: f32 = 0.30;
/// Frames wanted in each area of the coverage grid.
const PER_AREA: usize = 8;
/// Areas that must be covered before a template may be written. The centre
/// is always one of them; the rest is how the face is actually presented
/// over a day.
const MIN_AREAS: usize = 3;
const MAX_SECONDS: u64 = 90;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Whose face. Defaults to the invoking user (`SUDO_USER` under sudo,
    /// which is the normal case: the template directory belongs to the
    /// daemon, not to the person).
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long, default_value = "/var/lib/nirlock/templates")]
    pub templates: PathBuf,
    #[arg(long, default_value = "/usr/share/nirlock/models")]
    pub models: PathBuf,
    #[arg(long, default_value_t = 4)]
    pub threads: usize,
    /// Replace an existing enrolment.
    #[arg(long)]
    pub force: bool,
}

/// One cell of the coverage grid. The labels describe **where on the screen
/// to look**, which the person can act on directly, rather than naming a
/// head movement — the camera sits above the screen, so looking around it
/// produces exactly the pose spread we need.
struct Area {
    label: &'static str,
    /// `(yaw, pitch)` window. Deliberately generous: the point is spread,
    /// not precision, and the unlock gate applies its own limits anyway.
    yaw: (f64, f64),
    pitch: (f64, f64),
}

const AREAS: &[Area] = &[
    Area {
        label: "the middle of the screen",
        yaw: (-0.13, 0.13),
        pitch: (0.34, 0.78),
    },
    Area {
        label: "the LEFT edge of the screen",
        yaw: (0.10, 0.40),
        pitch: (0.28, 0.88),
    },
    Area {
        label: "the RIGHT edge of the screen",
        yaw: (-0.40, -0.10),
        pitch: (0.28, 0.88),
    },
    Area {
        label: "the TOP of the screen, near the camera",
        yaw: (-0.18, 0.18),
        pitch: (0.28, 0.45),
    },
    Area {
        label: "the BOTTOM of the screen",
        yaw: (-0.18, 0.18),
        pitch: (0.66, 0.90),
    },
];

pub fn run(a: &Args) -> i32 {
    let user = a.user.clone().unwrap_or_else(default_user);
    if user.is_empty() {
        eprintln!("could not tell whose face this is; pass --user");
        return 2;
    }
    let dir = a.templates.join(&user);
    if dir.exists() && !a.force {
        eprintln!("{user} is already enrolled.\nRe-enrol with:  sudo nirlockctl enroll --force");
        return 1;
    }
    if std::fs::create_dir_all(&a.templates).is_err() {
        eprintln!(
            "cannot write to {} — enrolment stores the template where the daemon can read it, so this needs sudo.",
            a.templates.display()
        );
        return 1;
    }

    println!("Enrolling {user}.\n");
    println!("Sit the way you normally use the laptop, with the room lit.");
    println!("Then just look slowly around the screen — the middle, each edge,");
    println!("the top and the bottom — keeping your face towards the laptop.");
    println!("Areas tick off as they are covered. It takes under a minute.\n");
    println!("The infrared light is invisible to you; the camera LED will be on.");
    println!("Press Ctrl+C at any time — nothing is written until the end.\n");
    print!("Ready? press Enter ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    let mut p = match Pipeline::load(&a.models, a.threads) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let frames = match collect(&mut p) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("\n{e}");
            return 1;
        }
    };

    println!("\nBuilding the template from {} frames…", frames.len());
    match write_template(&mut p, &dir, &frames) {
        Ok(n) => {
            println!("\n✓ {user} enrolled: {n} views saved in {}", dir.display());
            println!("  The daemon notices the change by itself — no restart needed.");
            println!("  Lock the screen and look at it.");
            0
        }
        Err(e) => {
            eprintln!("could not write the template: {e}");
            1
        }
    }
}

fn default_user() -> String {
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

struct Pipeline {
    detector: Detector,
    embedder: Embedder,
    gate: GateConfig,
}

impl Pipeline {
    fn load(models: &Path, threads: usize) -> Result<Self, String> {
        let det = Detector::load(
            &models.join("face_detection_yunet_2026may.onnx"),
            1,
            DETECTOR_FLOOR,
        )
        .map_err(|e| format!("models in {}: {e}", models.display()))?;
        let emb = Embedder::load(
            Kind::AuraFace,
            &models.join(Kind::AuraFace.file_name()),
            threads,
        )
        .map_err(|e| format!("models in {}: {e}", models.display()))?;
        Ok(Self {
            detector: det,
            embedder: emb,
            gate: GateConfig::default(),
        })
    }
}

/// Fallback for terminals that cannot show colour: the same information in
/// one line.
fn progress_text(counts: &[usize], hint: &str) {
    let mut s = String::from("  ");
    for (i, area) in AREAS.iter().enumerate() {
        let done = counts[i] >= PER_AREA;
        let name = area.label.split_whitespace().nth(1).unwrap_or("?");
        s.push_str(&format!("{} {name}   ", if done { "✓" } else { "·" }));
    }
    print!("\r{s}  {hint:<40}");
    let _ = std::io::stdout().flush();
}

/// One continuous capture. Keeps every good frame; nothing is ever thrown
/// away because a later part went badly.
fn collect(p: &mut Pipeline) -> Result<Vec<GrayImage>, String> {
    let (cam, profile, _) = select(None).map_err(|e| format!("camera: {e}"))?;
    let mut cap = IrCapture::open(
        &cam,
        &profile,
        OpenOptions {
            rgb: true,
            ..OpenOptions::default()
        },
        RealSys,
    )
    .map_err(|e| format!("camera: {e}"))?;

    let mut kept: Vec<GrayImage> = Vec::new();
    let mut counts = vec![0usize; AREAS.len()];
    let mut hint = String::new();
    let mut hint_since = Instant::now();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(MAX_SECONDS);
    let screen = preview::Screen::probe();
    screen.enter();

    let result = loop {
        if counts.iter().all(|c| *c >= PER_AREA) {
            break Ok(());
        }
        if Instant::now() > deadline {
            break Err(());
        }
        let Some(f) = cap.next(1.0).map_err(|e| e.to_string())? else {
            continue;
        };
        if f.meta.lit != Some(true) || !f.usable() {
            continue;
        }
        let img = GrayImage::from_vec(f.width as usize, f.height as usize, f.pixels)
            .map_err(|e| e.to_string())?;
        let faces = p.detector.detect(&img).map_err(|e| e.to_string())?;
        let g = quality_gate(&faces, &img, &p.gate);
        if !g.pass() {
            // Hints change slowly on purpose: recomputed every frame they
            // flicker between two conditions and read as noise.
            let note = match g.reason {
                Reject::NoFace => "I cannot see a face",
                Reject::MultiFace => "more than one face in view",
                Reject::LowScore => "hold still a moment",
                Reject::SmallBox => "move a little closer",
                Reject::Saturated => "a bit too close or too bright",
                Reject::Underexposed => "the room is too dark",
                Reject::Pose => "keep your face towards the laptop",
                Reject::None => "",
            };
            if note != hint && hint_since.elapsed() > Duration::from_millis(1200) {
                hint = note.to_string();
                hint_since = Instant::now();
            }
            let done: Vec<bool> = counts.iter().map(|c| *c >= PER_AREA).collect();
            if screen.graphical {
                let b = faces.first().map(|f| (f.x, f.y, f.w, f.h));
                preview::draw(&img, b, &done, &hint);
            } else {
                progress_text(&counts, &hint);
            }
            continue;
        }
        if !hint.is_empty() && hint_since.elapsed() > Duration::from_millis(600) {
            hint.clear();
        }
        // A frame can serve more than one area where the windows overlap;
        // count it wherever it is still needed, but store it once.
        let mut used = false;
        for (i, area) in AREAS.iter().enumerate() {
            if counts[i] >= PER_AREA {
                continue;
            }
            if g.pose.yaw >= area.yaw.0
                && g.pose.yaw <= area.yaw.1
                && g.pose.pitch >= area.pitch.0
                && g.pose.pitch <= area.pitch.1
            {
                counts[i] += 1;
                used = true;
            }
        }
        let done: Vec<bool> = counts.iter().map(|c| *c >= PER_AREA).collect();
        if screen.graphical {
            let b = faces.first().map(|f| (f.x, f.y, f.w, f.h));
            preview::draw(&img, b, &done, if used { "good" } else { &hint });
        } else {
            progress_text(&counts, &hint);
        }
        if used {
            kept.push(img);
        }
    };

    cap.close();
    screen.leave();
    println!();
    let covered = counts.iter().filter(|c| **c >= PER_AREA).count();
    if result.is_ok() {
        println!("  all {} areas covered", AREAS.len());
        return Ok(kept);
    }
    // Timed out. Whatever was gathered is still worth keeping if it spans
    // enough of the space — asking someone to start over is the one thing
    // this command must never do.
    if covered >= MIN_AREAS && counts[0] >= PER_AREA {
        println!(
            "  time is up with {covered} of {} areas covered — that is enough to enrol.",
            AREAS.len()
        );
        println!("  Run this again later if the face is ever not recognised from some angle.");
        return Ok(kept);
    }
    let missing: Vec<&str> = AREAS
        .iter()
        .zip(&counts)
        .filter(|(_, c)| **c < PER_AREA)
        .map(|(a, _)| a.label)
        .collect();
    Err(format!(
        "only {covered} of {} areas covered in {MAX_SECONDS} s.\nStill needed: {}.\nNothing was written. Try again with more light, or sitting a little further back.",
        AREAS.len(),
        missing.join("; ")
    ))
}

fn write_template(p: &mut Pipeline, dir: &Path, rows: &[GrayImage]) -> Result<usize, String> {
    let mut embs: Vec<f32> = Vec::new();
    let (mut n, mut dim) = (0usize, 0usize);
    for img in rows {
        let faces = p.detector.detect(img).map_err(|e| e.to_string())?;
        let Some(f) = faces.first() else { continue };
        let crop = nirlock_vision::align::align(img, f).map_err(|e| e.to_string())?;
        let v = p.embedder.embed(&crop).map_err(|e| e.to_string())?;
        dim = v.len();
        embs.extend_from_slice(&v);
        n += 1;
    }
    if n < PER_AREA * MIN_AREAS {
        return Err(format!("only {n} usable frames; not enough to enrol"));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    nirlock_vision::template::write_set(dir, "auraface", "lit", dim, n, &embs)
        .map_err(|e| e.to_string())?;
    hand_over_to_daemon(dir)?;
    Ok(n)
}

/// Give the enrolment to the daemon's user, explicitly.
///
/// Enrolment runs as root (`sudo nirlockctl enroll`) while the daemon runs as
/// `nirlock`, so without this the files keep whatever root's umask produced.
/// It worked only because the usual umask is 022, leaving them world-readable
/// inside a 0700 state directory. Under a hardened umask of 077 the enrolment
/// would report success and every later verification would fail, with nothing
/// to connect the two. Depending on an inherited umask for who can read a
/// biometric template is not a thing to leave to chance in either direction.
fn hand_over_to_daemon(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    // Only meaningful for the packaged install: no `nirlock` user means a
    // development run, where the files already belong to whoever wrote them.
    let Some((uid, gid)) = daemon_ids() else {
        return Ok(());
    };
    let set = |p: &Path, mode: u32| -> Result<(), String> {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", p.display()))?;
        std::os::unix::fs::chown(p, Some(uid), Some(gid))
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    set(dir, 0o700)?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        set(&entry.path(), 0o600)?;
    }
    Ok(())
}

/// The `nirlock` user's uid/gid, via `getent` so this crate stays free of
/// `unsafe` and of a libc dependency.
fn daemon_ids() -> Option<(u32, u32)> {
    let out = std::process::Command::new("getent")
        .args(["passwd", "nirlock"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?;
    let mut f = line.trim_end().split(':').skip(2);
    let uid = f.next()?.parse().ok()?;
    let gid = f.next()?.parse().ok()?;
    Some((uid, gid))
}
