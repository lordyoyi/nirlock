//! `nirlockctl probe`: what cameras this machine has, whether nirlock can
//! use them, and — when it can — the profile to contribute.
//!
//! This exists because the daemon shipped with exactly one hardware profile,
//! compiled in, for the camera it was written on. On any other machine the
//! only thing it could say was "camera not found", which is both true and
//! useless: the camera is right there, nothing claims it. Probing reports
//! what is present, says plainly whether it qualifies, and writes the
//! candidate profile so contributing one is a copy, not a research project.
//!
//! It uses only the ioctls the daemon already uses (`QUERYCAP`, `G_FMT`).
//! Enumerating every format the driver offers would need `VIDIOC_ENUM_FMT`,
//! and widening this crate's audited ioctl surface for a diagnostic is not a
//! trade worth making. The cost is that geometry comes from each node's
//! *default* format, which is what the profile should use anyway.

use std::path::{Path, PathBuf};

use crate::capture::{FrameSource, IrCapture, LabelRules, OpenOptions};
use crate::discover::{UsbCamera, discover_all_in, pin, usb_cameras_in};
use crate::hw::{IR_MAX_DIM, IR_MIN_DIM, ProfileSet};
use crate::sys::Sys;
use crate::v4l2::{
    Request, V4L2_BUF_TYPE_META_CAPTURE, V4L2_BUF_TYPE_VIDEO_CAPTURE, V4L2_CAP_META_CAPTURE,
    V4L2_CAP_VIDEO_CAPTURE, V4L2_META_FMT_UVC_MSXU_1_5, fourcc_str, v4l2_capability, v4l2_format,
};

/// What a node turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Video,
    Meta,
    /// Opened, but it is neither a capture nor a metadata node.
    Other,
    /// Could not be opened or queried; `NodeReport::error` says why.
    Unknown,
}

#[derive(Clone, Debug)]
pub struct NodeReport {
    pub dev: PathBuf,
    pub card: String,
    pub iface: u32,
    pub index: u32,
    pub kind: NodeKind,
    /// FourCC of the node's default format, empty when unknown.
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub error: Option<String>,
}

/// How long to watch the IR node. At this camera's 133 ms lit-frame cadence
/// that is about 15 lit and 15 dark frames, which is plenty to see whether the
/// label alternates; a slower camera gets fewer, and the verdict says how many
/// it actually saw rather than pretending.
const STROBE_SECONDS: f64 = 2.0;

/// What the frames actually showed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrobeCounts {
    pub frames: u32,
    pub lit: u32,
    pub dark: u32,
    pub unlabelled: u32,
    /// Sequences where the label did NOT change from the frame before.
    pub same_label_at: Vec<u32>,
    pub seconds: u32,
}

/// The measured state of the emitter.
///
/// This exists because probe used to write `emitter = "firmware-strobe"` into
/// the generated profile as a literal and tell the user "it will work",
/// without ever opening the stream. On a camera whose emitter stays dark until
/// something writes a vendor control — which is most of them, and which nirlock
/// will never do (ADR-0005) — that sentence was false, and the person found out
/// after a 286 MB download and an install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Strobe {
    /// Lit AND dark frames, with the label alternating. The only outcome that
    /// earns `emitter = "firmware-strobe"`.
    FirmwareStrobe(StrobeCounts),
    /// Labels arrive but the bit never changes. The dangerous case: a check
    /// that only asked "are labels arriving?" would wave this through, yet a
    /// constant bit carries no information, so nothing separates an
    /// emitter-lit frame from an ambient one and the anti-spoofing argument
    /// is gone.
    Steady(StrobeCounts),
    /// Frames arrive carrying no label, so requirement 2 fails at runtime even
    /// though `S_FMT` agreed to UVCM.
    Unlabelled(StrobeCounts),
    /// Nothing was delivered at all, which usually means the emitter is off
    /// and the firmware is not producing frames.
    NoFrames(StrobeCounts),
    /// The measurement could not be taken. **Not a verdict about the camera**:
    /// another process may hold it, or there may be no access to the node.
    /// Conflating this with a dark emitter would turn a working camera away,
    /// which is the same bug as the one being fixed, pointed the other way.
    Unmeasured(String),
}

impl Strobe {
    /// One line for a report or an issue, so the person pasting it does not
    /// have to describe what they saw.
    pub fn summary(&self) -> String {
        match self {
            Strobe::Unmeasured(why) => format!("not measured: {why}"),
            Strobe::FirmwareStrobe(c) => format!(
                "firmware strobe measured: {} frames in {} s, {} lit / {} dark, alternating",
                c.frames, c.seconds, c.lit, c.dark
            ),
            Strobe::Steady(c) => format!(
                "steady emitter: {} frames, {} lit / {} dark, label unchanged at {} sequences",
                c.frames,
                c.lit,
                c.dark,
                c.same_label_at.len()
            ),
            Strobe::Unlabelled(c) => format!(
                "no illumination labels: {} frames, {} unlabelled",
                c.frames, c.unlabelled
            ),
            Strobe::NoFrames(_) => "no frames delivered".into(),
        }
    }
}

/// Streams the IR node of one camera and reports what the labels did.
///
/// Read-only on the control surface: it opens, formats and streams, exactly as
/// the daemon does, and writes no UVC control. (The kernel does write one on
/// its own behalf when the metadata node is opened; see ADR-0005.)
pub fn measure_strobe<S: Sys>(
    root: &Path,
    profile: &crate::hw::HwProfile,
    usb_sysfs: &Path,
    sys: &mut S,
) -> Strobe {
    let cams = match discover_all_in(root, profile) {
        Ok(c) => c,
        Err(e) => return Strobe::Unmeasured(e.to_string()),
    };
    let cam = match pin(cams, Some(usb_sysfs), root, profile) {
        Ok(c) => c,
        Err(e) => return Strobe::Unmeasured(e.to_string()),
    };
    // IR only. The RGB lever is an unlock-latency tool, not part of the
    // question being asked here, and leaving it off keeps the burst short.
    let mut cap = match IrCapture::open(&cam, profile, OpenOptions::default(), sys.clone()) {
        Ok(c) => c,
        Err(e) => return Strobe::Unmeasured(format!("{e} ({})", e.reason())),
    };

    let mut rules = LabelRules::default();
    let t0 = cap.timing().t_streamon_done;
    let mut counts = StrobeCounts::default();
    let mut capture_error: Option<String> = None;
    loop {
        let left = STROBE_SECONDS - (sys.mono_now() - t0);
        if left <= 0.0 {
            break;
        }
        match cap.next(left) {
            Ok(Some(f)) => {
                counts.frames += 1;
                match f.meta.lit {
                    Some(true) => counts.lit += 1,
                    Some(false) => counts.dark += 1,
                    None => counts.unlabelled += 1,
                }
                rules.observe(&f, None);
            }
            // A timeout inside the window is not an error: it is the answer
            // when the firmware delivers nothing.
            Ok(None) => break,
            Err(e) => {
                capture_error = Some(format!("{e} ({})", e.reason()));
                break;
            }
        }
    }
    cap.close();
    counts.same_label_at = rules.same_label_at.clone();
    counts.seconds = STROBE_SECONDS as u32;

    classify(counts, capture_error)
}

/// Turns what was counted into a verdict. Pure, so every branch is testable
/// without a camera: the streaming above is already covered by the capture
/// layer's own tests, and this is the part that is new.
fn classify(counts: StrobeCounts, capture_error: Option<String>) -> Strobe {
    if let Some(e) = capture_error {
        // Frames may have arrived before it broke, but a partial burst cannot
        // tell a dark emitter from an interrupted one.
        return Strobe::Unmeasured(e);
    }
    if counts.frames == 0 {
        return Strobe::NoFrames(counts);
    }
    if counts.lit == 0 && counts.dark == 0 {
        return Strobe::Unlabelled(counts);
    }
    // Both halves must be present AND the label must actually alternate. Only
    // one of those is enough to fool a weaker test: a long lit run with a
    // single dark frame at the end would pass a both-seen check while carrying
    // almost no information.
    if counts.lit > 0 && counts.dark > 0 && counts.same_label_at.is_empty() {
        Strobe::FirmwareStrobe(counts)
    } else {
        Strobe::Steady(counts)
    }
}

/// Whether nirlock can authenticate with this camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A profile already claims it.
    Supported { id: String, source: String },
    /// It qualifies and no profile claims it yet: contribute `candidate`.
    Candidate,
    /// It cannot be used, and this is why.
    Unusable(String),
    /// The descriptors qualify but the strobe could not be measured, so there
    /// is no verdict yet. Kept apart from `Unusable` on purpose: a camera held
    /// by another process, or a node with no access, must not be reported as
    /// hardware that cannot work.
    Inconclusive(String),
}

#[derive(Clone, Debug)]
pub struct DeviceReport {
    pub usb: UsbCamera,
    pub nodes: Vec<NodeReport>,
    pub verdict: Verdict,
    /// A profile ready to drop into `/etc/nirlock/hw/`, when one could be
    /// derived. Present for `Candidate`, and for `Supported` so a shipped
    /// profile can be compared against what the machine actually reports.
    pub candidate: Option<String>,
    /// What streaming the IR node showed. `None` when the descriptors did not
    /// qualify, so there was nothing to stream.
    pub strobe: Option<Strobe>,
}

fn query_node<S: Sys>(sys: &mut S, dev: &Path) -> (NodeKind, String, u32, u32, Option<String>) {
    let fd = match sys.open(dev) {
        Ok(fd) => fd,
        Err(e) => return (NodeKind::Unknown, String::new(), 0, 0, Some(e.to_string())),
    };
    let mut cap = v4l2_capability::default();
    if let Err(e) = sys.ioctl(fd, Request::QueryCap(&mut cap)) {
        sys.close(fd);
        return (NodeKind::Unknown, String::new(), 0, 0, Some(e.to_string()));
    }
    let (kind, buf_type) = if cap.device_caps & V4L2_CAP_VIDEO_CAPTURE != 0 {
        (NodeKind::Video, V4L2_BUF_TYPE_VIDEO_CAPTURE)
    } else if cap.device_caps & V4L2_CAP_META_CAPTURE != 0 {
        (NodeKind::Meta, V4L2_BUF_TYPE_META_CAPTURE)
    } else {
        sys.close(fd);
        return (NodeKind::Other, String::new(), 0, 0, None);
    };
    let mut f = v4l2_format::new(buf_type);
    if let Err(e) = sys.ioctl(fd, Request::GFmt(&mut f)) {
        sys.close(fd);
        return (kind, String::new(), 0, 0, Some(e.to_string()));
    }
    if kind == NodeKind::Video {
        let p = f.pix();
        let (fourcc, w, h) = (fourcc_str(p.pixelformat), p.width, p.height);
        sys.close(fd);
        return (kind, fourcc, w, h, None);
    }

    // Metadata nodes: `G_FMT` reports the format currently selected, not what
    // the node can do. This camera's IR metadata node comes up as `UVCH` and
    // only becomes `UVCM` after an `S_FMT` — see docs/HARDWARE.md. Reading
    // G_FMT alone therefore declared a perfectly good camera unusable on any
    // machine where nirlock had not already run, which is every machine this
    // command exists for. Ask for UVCM and see whether the driver agrees;
    // that is the same negotiation the daemon performs.
    let mut want = v4l2_format::new(V4L2_BUF_TYPE_META_CAPTURE);
    want.meta_mut().dataformat = V4L2_META_FMT_UVC_MSXU_1_5;
    let negotiated = sys
        .ioctl(fd, Request::SFmt(&mut want))
        .is_ok()
        .then(|| fourcc_str(want.meta().dataformat));
    let current = fourcc_str(f.meta().dataformat);
    sys.close(fd);
    // Report what it can be, falling back to what it is.
    (kind, negotiated.unwrap_or(current), 0, 0, None)
}

/// The candidate profile TOML for a device, or `None` when it does not
/// qualify. `reason` receives the explanation in that case.
fn candidate_toml(dev: &DeviceReport, reason: &mut String) -> Option<String> {
    // The IR node: a capture node reporting GREY at a plausible face-camera
    // size. A colour camera has no such node, which is the usual reason a
    // laptop cannot do this at all.
    let ir = dev.nodes.iter().find(|n| {
        n.kind == NodeKind::Video
            && n.format == "GREY"
            && (IR_MIN_DIM..=IR_MAX_DIM).contains(&n.width)
            && (IR_MIN_DIM..=IR_MAX_DIM).contains(&n.height)
    });
    // A camera the firmware does not declare as internal is rejected by
    // `HwProfile::validate` on purpose (THREAT-MODEL T7: an attacker's
    // external camera with a cloned VID:PID reports "removable"). Emitting a
    // candidate for one advertised it as usable and handed over a profile
    // that could never load.
    if dev.usb.removable != "fixed" {
        *reason = format!(
            "this camera reports removable=\"{}\", not \"fixed\". Only a camera the \
             firmware declares as built-in is accepted, so that an external camera with \
             a cloned vendor:product cannot stand in for the internal one.",
            dev.usb.removable
        );
        return None;
    }
    let Some(ir) = ir else {
        *reason = "no infrared node: no capture node reports GREY at a face-camera size. \
                   A plain colour webcam cannot do this — Windows Hello needs the separate \
                   IR sensor."
            .into();
        return None;
    };
    // Its metadata sibling, which is what carries the per-frame illumination
    // label. Without it there is no way to tell a lit frame from an unlit
    // one, and the whole anti-spoofing argument collapses.
    let meta = dev
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Meta && n.iface == ir.iface && n.format == "UVCM");
    let Some(meta) = meta else {
        *reason = format!(
            "the IR node ({}) has no UVCM metadata sibling on interface {}. \
             That needs Linux 6.17 or newer (V4L2_META_FMT_UVC_MSXU_1_5) and a camera \
             exposing the Microsoft extension unit. Running kernel: {}.",
            ir.dev.display(),
            ir.iface,
            kernel_release()
        );
        return None;
    };
    // RGB is a latency lever, not a requirement (ADR-0006): it keeps the
    // colour sensor's auto-exposure running so the IR frame is not captured
    // against a black screen. Fall back to the IR interface if absent.
    let rgb = dev
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Video && n.format == "MJPG");

    let mut t = String::new();
    t.push_str(&format!(
        "# {}\n# Generated by `nirlockctl probe`. Verify it, then send it as a PR.\n",
        dev.usb.label()
    ));
    if rgb.is_none() {
        t.push_str(
            "# No MJPG node was found; the rgb block below is a guess and the\n\
             # daemon should be run without --rgb on this camera.\n",
        );
    }
    t.push_str(&format!(
        "id = \"{}-{}\"\n",
        slug(&dev.usb.name),
        dev.usb.product
    ));
    t.push_str(&format!(
        "match = {{ vendor = \"{}\", product = \"{}\", removable = \"{}\" }}\n",
        dev.usb.vendor, dev.usb.product, dev.usb.removable
    ));
    t.push_str(&format!(
        "ir   = {{ interface = {}, index = {}, format = \"GREY\", width = {}, height = {}, fps = 15, bytes = {} }}\n",
        ir.iface,
        ir.index,
        ir.width,
        ir.height,
        ir.width * ir.height
    ));
    t.push_str(&format!(
        "meta = {{ interface = {}, index = {}, format = \"UVCM\" }}\n",
        meta.iface, meta.index
    ));
    match rgb {
        Some(r) => t.push_str(&format!(
            "rgb  = {{ interface = {}, index = {}, format = \"MJPG\", width = {}, height = {} }}\n",
            r.iface, r.index, r.width, r.height
        )),
        // Interface 0 / index 0 was the placeholder here, and on an IR-only
        // camera whose capture node IS interface 0 index 0 it collided with
        // the IR role. `discover_all_in` tests the RGB coordinates first, so
        // the IR node was claimed as RGB, `ir_cap` stayed empty, and the
        // camera was discarded entirely. Point it at coordinates that cannot
        // exist instead: the node is simply never found, which is the honest
        // representation of "this camera has no colour sensor".
        None => t.push_str(&format!(
            "# No MJPG node: these coordinates intentionally match nothing.\n\
             rgb  = {{ interface = {}, index = 90, format = \"MJPG\", width = 1280, height = 720 }}\n",
            ir.iface
        )),
    }
    t.push_str("emitter = \"firmware-strobe\"\n");
    t.push_str("labeler = \"uvcm-metadata\"\n");
    t.push_str(
        "notes = \"fps is a guess: probe reads each node's default format, not the full list. \
         Check with `v4l2-ctl -d <ir node> --list-formats-ext`.\"\n",
    );

    // The backstop. Everything above is a guess assembled from what the
    // nodes reported, and a candidate that does not survive the daemon's own
    // validation is worse than no candidate: it is advertised as usable and
    // then refuses to load, sending someone to debug a file we generated.
    if let Err(e) = crate::hw::HwProfile::parse(&t) {
        *reason = format!("a profile could be assembled but it does not validate: {e}");
        return None;
    }
    Some(t)
}

/// Records the measurement in the profile's own notes, so a contributed
/// profile carries the evidence for its `emitter` line instead of asserting it.
fn with_measurement(toml: &str, strobe: &Strobe) -> String {
    toml.replace(
        "notes = \"fps is a guess",
        &format!("notes = \"{}. fps is a guess", strobe.summary()),
    )
}

fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "camera".into() } else { out }
}

fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

/// Probes every uvcvideo camera under `root`.
pub fn probe_in<S: Sys>(root: &Path, set: &ProfileSet, sys: &mut S) -> Vec<DeviceReport> {
    let mut out = Vec::new();
    for usb in usb_cameras_in(root) {
        let nodes: Vec<NodeReport> = usb
            .nodes
            .iter()
            .map(|n| {
                let (kind, format, width, height, error) = query_node(sys, &n.dev);
                NodeReport {
                    dev: n.dev.clone(),
                    card: n.card.clone(),
                    iface: n.iface,
                    index: n.index,
                    kind,
                    format,
                    width,
                    height,
                    error,
                }
            })
            .collect();
        let mut dev = DeviceReport {
            usb,
            nodes,
            verdict: Verdict::Candidate,
            candidate: None,
            strobe: None,
        };
        let mut reason = String::new();
        dev.candidate = candidate_toml(&dev, &mut reason);

        // The descriptors can only answer two of the three requirements. The
        // third — does the emitter strobe by itself — is invisible until the
        // node is streamed, and it is the one that most cameras fail.
        if let Some(toml) = dev.candidate.clone() {
            match crate::hw::HwProfile::parse(&toml) {
                Ok(profile) => {
                    let st = measure_strobe(root, &profile, &dev.usb.usb_sysfs, sys);
                    match &st {
                        Strobe::FirmwareStrobe(_) => {
                            dev.candidate = Some(with_measurement(&toml, &st));
                        }
                        Strobe::Unmeasured(why) => {
                            reason = why.clone();
                            dev.candidate = None;
                        }
                        _ => {
                            reason = st.summary();
                            dev.candidate = None;
                        }
                    }
                    dev.strobe = Some(st);
                }
                Err(e) => {
                    reason = e.to_string();
                    dev.candidate = None;
                }
            }
        }

        dev.verdict = match (
            set.find(&dev.usb.vendor, &dev.usb.product),
            dev.candidate.is_some(),
        ) {
            (Some(p), _) => Verdict::Supported {
                id: p.id.clone(),
                source: set
                    .source_of(&dev.usb.vendor, &dev.usb.product)
                    .unwrap_or("unknown")
                    .to_string(),
            },
            (None, true) => Verdict::Candidate,
            (None, false) => match &dev.strobe {
                Some(Strobe::Unmeasured(why)) => Verdict::Inconclusive(why.clone()),
                _ => Verdict::Unusable(reason),
            },
        };
        out.push(dev);
    }
    out
}

/// Probes the live system with the installed profiles.
pub fn probe<S: Sys>(sys: &mut S) -> Vec<DeviceReport> {
    probe_in(Path::new("/sys"), &ProfileSet::load(), sys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Node;

    fn node(dev: &str, iface: u32, index: u32, kind: NodeKind, fmt: &str, w: u32, h: u32) -> NodeReport {
        NodeReport {
            dev: PathBuf::from(dev),
            card: "test".into(),
            iface,
            index,
            kind,
            format: fmt.into(),
            width: w,
            height: h,
            error: None,
        }
    }

    fn device(removable: &str, nodes: Vec<NodeReport>) -> DeviceReport {
        DeviceReport {
            usb: UsbCamera {
                usb_sysfs: PathBuf::from("/sys/devices/x"),
                vendor: "04f2".into(),
                product: "b6d0".into(),
                name: "Integrated Camera".into(),
                removable: removable.into(),
                nodes: Vec::<Node>::new(),
            },
            nodes,
            verdict: Verdict::Candidate,
            candidate: None,
            strobe: None,
        }
    }

    fn hello_nodes() -> Vec<NodeReport> {
        vec![
            node("/dev/video0", 0, 0, NodeKind::Video, "MJPG", 1280, 720),
            node("/dev/video2", 2, 0, NodeKind::Video, "GREY", 640, 480),
            node("/dev/video3", 2, 1, NodeKind::Meta, "UVCM", 0, 0),
        ]
    }

    fn counts(frames: u32, lit: u32, dark: u32, unlabelled: u32, same: &[u32]) -> StrobeCounts {
        StrobeCounts {
            frames,
            lit,
            dark,
            unlabelled,
            same_label_at: same.to_vec(),
            seconds: 2,
        }
    }

    /// The reason this whole measurement exists. Each branch is a camera
    /// somebody actually owns, and getting any of them wrong either turns a
    /// working camera away or repeats the bug being fixed.
    #[test]
    fn the_strobe_verdict_separates_every_case() {
        // A healthy camera: the reference machine measured 28 frames,
        // 14 lit / 14 dark, alternating.
        assert!(matches!(
            classify(counts(28, 14, 14, 0, &[]), None),
            Strobe::FirmwareStrobe(_)
        ));

        // Emitter off, firmware delivering nothing: Bison 5986:118c, where
        // "the stream opens, format negotiates, but the driver never produces
        // a frame".
        assert!(matches!(
            classify(counts(0, 0, 0, 0, &[]), None),
            Strobe::NoFrames(_)
        ));

        // Frames but no labels: S_FMT agreed to UVCM and nothing arrived in
        // it, so requirement 2 fails at runtime.
        assert!(matches!(
            classify(counts(20, 0, 0, 20, &[]), None),
            Strobe::Unlabelled(_)
        ));

        // The dangerous one. Chicony 04f2:b7c0 after its control write: "the
        // light is steady rather than per-frame". Labels arrive, the bit never
        // moves, and a check that only asked "are labels arriving?" would wave
        // it through.
        assert!(matches!(
            classify(counts(30, 30, 0, 0, &[]), None),
            Strobe::Steady(_)
        ));

        // Both halves seen is NOT sufficient: a long lit run with one dark
        // frame at the end carries almost no information, and the
        // same-label-consecutive violations are what expose it.
        assert!(matches!(
            classify(counts(30, 29, 1, 0, &[2, 3, 4, 5]), None),
            Strobe::Steady(_)
        ));

        // Could not measure. This must NEVER collapse into a verdict about
        // the hardware: the camera may simply be held by an unlock in flight.
        assert!(matches!(
            classify(counts(0, 0, 0, 0, &[]), Some("busy".into())),
            Strobe::Unmeasured(_)
        ));
        // Even with frames already counted, an interrupted burst is unmeasured
        // and not a dark emitter.
        assert!(matches!(
            classify(counts(9, 5, 4, 0, &[]), Some("busy".into())),
            Strobe::Unmeasured(_)
        ));
    }

    /// The summary is pasted into issue reports, so it has to say what
    /// happened without the reader having been there.
    #[test]
    fn the_summary_names_what_was_measured() {
        let s = classify(counts(28, 14, 14, 0, &[]), None).summary();
        assert!(s.contains("14 lit"), "{s}");
        assert!(s.contains("alternating"), "{s}");
        let s = classify(counts(0, 0, 0, 0, &[]), Some("camera busy".into())).summary();
        assert!(s.starts_with("not measured"), "{s}");
    }

    /// Every candidate must survive the daemon's own validation. Advertising
    /// a camera as usable and handing over a profile that cannot load sends
    /// someone to debug a file we generated.
    #[test]
    fn a_candidate_always_parses_as_a_profile() {
        let d = device("fixed", hello_nodes());
        let mut why = String::new();
        let toml = super::candidate_toml(&d, &mut why).expect(&why);
        let p = crate::hw::HwProfile::parse(&toml).expect("generated profile must validate");
        assert_eq!((p.ir.width, p.ir.height), (640, 480));
        assert_eq!(p.ir.bytes, 640 * 480);
        assert_eq!(p.match_.vendor, "04f2");
    }

    #[test]
    fn an_external_camera_is_refused_with_the_reason() {
        let d = device("removable", hello_nodes());
        let mut why = String::new();
        assert!(super::candidate_toml(&d, &mut why).is_none());
        assert!(why.contains("removable"), "{why}");
        assert!(why.contains("cloned"), "should say why it matters: {why}");
    }

    /// An IR-only camera whose capture node is interface 0 index 0: the RGB
    /// placeholder used to land on exactly those coordinates, and
    /// `discover_all_in` claims RGB before IR, so the camera vanished.
    #[test]
    fn the_rgb_placeholder_never_collides_with_the_ir_role() {
        let d = device(
            "fixed",
            vec![
                node("/dev/video0", 0, 0, NodeKind::Video, "GREY", 640, 360),
                node("/dev/video1", 0, 1, NodeKind::Meta, "UVCM", 0, 0),
            ],
        );
        let mut why = String::new();
        let toml = super::candidate_toml(&d, &mut why).expect(&why);
        let p = crate::hw::HwProfile::parse(&toml).unwrap();
        assert_ne!(
            (p.rgb.interface, p.rgb.index),
            (p.ir.interface, p.ir.index),
            "RGB must not claim the IR node"
        );
        assert_ne!((p.rgb.interface, p.rgb.index), (p.meta.interface, p.meta.index));
    }

    #[test]
    fn a_colour_only_camera_is_refused_with_the_reason() {
        let d = device(
            "fixed",
            vec![node("/dev/video0", 0, 0, NodeKind::Video, "MJPG", 1280, 720)],
        );
        let mut why = String::new();
        assert!(super::candidate_toml(&d, &mut why).is_none());
        assert!(why.contains("infrared"), "{why}");
    }

    #[test]
    fn an_ir_node_without_a_uvcm_sibling_is_refused_with_the_kernel_named() {
        let d = device(
            "fixed",
            vec![
                node("/dev/video2", 2, 0, NodeKind::Video, "GREY", 640, 360),
                node("/dev/video3", 2, 1, NodeKind::Meta, "UVCH", 0, 0),
            ],
        );
        let mut why = String::new();
        assert!(super::candidate_toml(&d, &mut why).is_none());
        assert!(why.contains("UVCM"), "{why}");
        assert!(why.contains("6.17"), "must name the kernel requirement: {why}");
    }
}
