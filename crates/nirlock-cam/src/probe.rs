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

use crate::discover::{UsbCamera, usb_cameras_in};
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

/// Whether nirlock can authenticate with this camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A profile already claims it.
    Supported { id: String, source: String },
    /// It qualifies and no profile claims it yet: contribute `candidate`.
    Candidate,
    /// It cannot be used, and this is why.
    Unusable(String),
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
        };
        let mut reason = String::new();
        dev.candidate = candidate_toml(&dev, &mut reason);
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
            (None, false) => Verdict::Unusable(reason),
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
        }
    }

    fn hello_nodes() -> Vec<NodeReport> {
        vec![
            node("/dev/video0", 0, 0, NodeKind::Video, "MJPG", 1280, 720),
            node("/dev/video2", 2, 0, NodeKind::Video, "GREY", 640, 480),
            node("/dev/video3", 2, 1, NodeKind::Meta, "UVCM", 0, 0),
        ]
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
