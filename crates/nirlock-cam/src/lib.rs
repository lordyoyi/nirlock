//! nirlock-cam: the camera layer of `nirlockd` (DESIGN §2.3–2.4), a
//! faithful port of `phase0/v4l2cap.*`.
//!
//! - [`hw`]: the hardware profile (`hw/3277-0055.toml`).
//! - [`discover`]: sysfs discovery, pinning to a USB path, ambiguity and
//!   v4l2loopback refusal (no device access).
//! - [`v4l2`]: the ABI structs and the **single** ioctl entry point over a
//!   closed enum of the nine allowed requests.
//! - [`sys`]: the syscall seam (`open`/`ioctl`/`mmap`/`poll`/clock) that
//!   tests replace with a scripted fake.
//! - [`stream`]: one node with MMAP buffers, RAII teardown.
//! - [`uvcm`]: `parse_uvcm`, 1:1.
//! - [`capture`]: `IrCapture` (exact open order, video↔meta pairing with
//!   the 25 ms grace, pruning), the RGB lever thread, the per-request label
//!   rules and the `FrameSource` trait.
//!
//! Device-safety contract (ADR-0005): towards `/dev/video*` this crate can
//! only issue `QUERYCAP`, `G_FMT`, `S_FMT`, `REQBUFS`, `QUERYBUF`, `QBUF`,
//! `DQBUF`, `STREAMON`, `STREAMOFF` (plus `poll`/`mmap`). There is no code
//! path for any control query or write, parameter write, enumeration,
//! extension unit or DFU interface. Enforced by the compiler and by
//! `tests::unsafe_and_ioctl_surface_is_the_declared_one`:
//! - the crate denies `unsafe_code`; only the functions listed in that
//!   test (per file, exact count) carry `#[allow(unsafe_code)]`, so no new
//!   FFI call, `extern` block or raw syscall can appear unnoticed;
//! - `libc` is named only in `v4l2.rs` and `sys.rs`, never imported with
//!   `use`, and `ioctl(` is called raw at exactly one site (`v4l2::ioctl`,
//!   over the closed `Request` enum of nine variants);
//! - every `VIDIOC_*` token in the crate is one of the nine allowed names
//!   and no control-query, extension-unit, usbdevfs or firmware-update
//!   token exists (the names are assembled at run time in the test);
//! - the dependency set is exactly `libc`, `serde`, `thiserror`, `toml`
//!   (no `nix`/`rustix`/`v4l` that could issue ioctls) and there is no
//!   build script.

#![deny(unsafe_code)]

pub mod capture;
pub mod discover;
#[cfg(test)]
pub(crate) mod fake;
pub mod hw;
pub mod probe;
pub mod stream;
pub mod sys;
pub mod uvcm;
pub mod v4l2;

pub use capture::{
    FrameSource, IrCapture, IrFrame, LabelRules, OpenOptions, RgbAssist, RgbStats, RgbThread,
    Timing, Violation, node_paths,
};
pub use discover::{
    Camera, Node, UsbCamera, discover, discover_all, discover_all_in, discover_in, pin, select,
    select_in, usb_cameras, usb_cameras_in,
};
pub use hw::{HwProfile, ProfileSet};
pub use stream::{Identity, Kind, RawFrame, Stream};
pub use sys::{RealSys, Sys, mono_now};
pub use uvcm::{MetaInfo, parse_uvcm};

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "camera not found: no uvcvideo node for USB {vid}:{pid} (removable=fixed) interface 02 index 0 under {root}"
    )]
    NotFound {
        vid: String,
        pid: String,
        root: String,
    },
    #[error("sysfs {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("hardware profile: {0}")]
    Profile(String),
    #[error(
        "no hardware profile for any camera on this machine.\n  cameras found: {}\n  profiles known: {}\n  Run `nirlockctl probe` to generate a profile for yours.",
        if devices.is_empty() { "none".to_string() } else { devices.join(", ") },
        if known.is_empty() { "none".to_string() } else { known.join(", ") }
    )]
    NoProfile {
        devices: Vec<String>,
        known: Vec<String>,
    },
    #[error("camera ambiguous: {} matching USB devices ({})", devices.len(), devices.join(", "))]
    Ambiguous { devices: Vec<String> },
    #[error("camera mismatch: pinned {pinned} not present (found: {})", if found.is_empty() { "none".to_string() } else { found.join(", ") })]
    Mismatch { pinned: String, found: Vec<String> },
    #[error("camera {usb_sysfs} has no metadata node (index 1) on the IR interface")]
    NoMetaNode { usb_sysfs: String },
    #[error(
        "refusing {dev}: driver is '{driver}' (bus_info '{bus_info}'), expected 'uvcvideo' (v4l2 loopback and other virtual cameras are never accepted)"
    )]
    Refused {
        dev: String,
        driver: String,
        bus_info: String,
    },
    #[error(
        "refusing {dev}: node lacks the expected capture capability (device_caps {device_caps:#x})"
    )]
    Capability { dev: String, device_caps: u32 },
    #[error("driver changed the requested format on {dev}: wanted {wanted}, got {got}")]
    Format {
        dev: String,
        wanted: String,
        got: String,
    },
    #[error(
        "{dev} is held by another application (browser, video call, PipeWire client...); `fuser -v {dev}` shows the holder"
    )]
    Busy { dev: String },
    #[error("{what} on {dev}: {source}")]
    Sys {
        what: &'static str,
        dev: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Metadata(Violation),
}

impl Error {
    /// Maps an OS error of a device call: `EBUSY` becomes [`Error::Busy`].
    pub fn from_os(what: &'static str, dev: &Path, source: std::io::Error) -> Self {
        if source.raw_os_error() == Some(sys::errno::EBUSY) {
            Error::Busy {
                dev: dev.display().to_string(),
            }
        } else {
            Error::Sys {
                what,
                dev: dev.display().to_string(),
                source,
            }
        }
    }

    /// The `unavailable <reason>` token of DESIGN §2.9.
    pub fn reason(&self) -> &'static str {
        match self {
            // Same token as NotFound on purpose: to a client the effect is
            // identical (no usable camera) and the `unavailable` vocabulary
            // in PROTOCOL.md is a closed set. The actionable part — which
            // camera is present and unclaimed — is in the message and the
            // journal, not in the token.
            Error::NotFound { .. } | Error::NoProfile { .. } => "camera_missing",
            Error::Ambiguous { .. } => "camera_ambiguous",
            Error::Mismatch { .. } | Error::NoMetaNode { .. } | Error::Refused { .. } => {
                "camera_mismatch"
            }
            Error::Capability { .. } | Error::Format { .. } => "camera_format",
            Error::Busy { .. } => "camera_busy",
            Error::Metadata(_) => "metadata",
            Error::Io { .. } | Error::Profile(_) | Error::Sys { .. } => "camera_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_and_messages() {
        let e = Error::from_os(
            "open",
            Path::new("/dev/video2"),
            std::io::Error::from_raw_os_error(sys::errno::EBUSY),
        );
        assert!(matches!(e, Error::Busy { .. }));
        assert!(e.to_string().contains("fuser -v /dev/video2"));
        let e = Error::from_os(
            "open",
            Path::new("/dev/video2"),
            std::io::Error::from_raw_os_error(sys::errno::ENOENT),
        );
        assert!(matches!(e, Error::Sys { what: "open", .. }));
        assert_eq!(e.reason(), "camera_error");
        assert_eq!(
            Error::Ambiguous {
                devices: vec!["a".into(), "b".into()]
            }
            .to_string(),
            "camera ambiguous: 2 matching USB devices (a, b)"
        );
        assert_eq!(
            Error::Mismatch {
                pinned: "p".into(),
                found: vec![]
            }
            .to_string(),
            "camera mismatch: pinned p not present (found: none)"
        );
        assert_eq!(Error::Profile("x".into()).reason(), "camera_error");
        assert_eq!(
            Error::Metadata(Violation::TooManyUnlabelled {
                unlabelled: 3,
                usable: 4
            })
            .reason(),
            "metadata"
        );
        assert_eq!(
            Error::Capability {
                dev: "d".into(),
                device_caps: 1
            }
            .reason(),
            "camera_format"
        );
    }

    /// Source text with `//` line comments and `/* */` block comments
    /// removed (string literals are not special-cased: that can only hide
    /// text from the positive checks, never create a false pass, and the
    /// crate has no string literal containing those sequences).
    fn strip_comments(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i..].starts_with(b"//") {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            } else if b[i..].starts_with(b"/*") {
                i += 2;
                while i < b.len() && !b[i..].starts_with(b"*/") {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(' ');
            } else {
                out.push(b[i] as char);
                i += 1;
            }
        }
        out
    }

    fn is_word(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_'
    }

    /// Every `PREFIX[A-Za-z0-9_]*` token of `text` (whole words only).
    fn tokens(text: &str, prefix: &str) -> Vec<String> {
        let b = text.as_bytes();
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(pos) = text[from..].find(prefix) {
            let start = from + pos;
            let mut end = start + prefix.len();
            while end < b.len() && is_word(b[end]) {
                end += 1;
            }
            if start == 0 || !is_word(b[start - 1]) {
                out.push(text[start..end].to_string());
            }
            from = end;
        }
        out
    }

    /// Source files of the crate: `src/*.rs` and `tests/*.rs`.
    fn crate_sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in ["src", "tests"] {
            let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
                continue;
            };
            for entry in rd {
                let p = entry.unwrap().path();
                if p.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let name = format!("{dir}/{}", p.file_name().unwrap().to_string_lossy());
                files.push((name, std::fs::read_to_string(&p).unwrap()));
            }
        }
        files.sort();
        files
    }

    /// ADR-0005 / DESIGN §2.3 step 3, mechanically. See the crate docs for
    /// the list of what this pins.
    #[test]
    fn unsafe_and_ioctl_surface_is_the_declared_one() {
        let files = crate_sources();
        assert!(files.len() >= 9, "expected the crate's modules: {files:?}");

        // (0) The crate root denies unsafe code; no scope-level allow.
        let lib = &files.iter().find(|(n, _)| n == "src/lib.rs").unwrap().1;
        assert!(lib.contains("#![deny(unsafe_code)]"));
        // Assembled so that this file does not contain the attribute text.
        let tag = ["allow(", "unsafe_code)"].concat();
        let inner_allow = format!("#![{tag}]");
        let attr = format!("#[{tag}]");
        // (a) `#[allow(unsafe_code)]`: exact count per file, each on a fn.
        let expected: &[(&str, usize)] = &[
            ("src/v4l2.rs", 8),   // union accessors ×7 + the ioctl wrapper
            ("src/sys.rs", 7),    // mmap, munmap, poll, fstat_rdev, close, mono_now, umask_private
            ("src/stream.rs", 1), // Stream::buffer
            ("src/fake.rs", 2),   // ioctl (DQBUF copy), munmap (test double)
        ];
        for (name, text) in &files {
            let code = strip_comments(text);
            assert!(!code.contains(&inner_allow), "{name}: scope-level allow");
            let want = expected
                .iter()
                .find(|(n, _)| n == name)
                .map_or(0, |(_, c)| *c);
            let lines: Vec<&str> = code.lines().collect();
            let mut count = 0;
            for (i, l) in lines.iter().enumerate() {
                if l.trim() == attr {
                    count += 1;
                    let next = lines[i + 1..]
                        .iter()
                        .map(|l| l.trim_start())
                        .find(|l| !l.is_empty())
                        .unwrap_or("");
                    assert!(
                        next.starts_with("fn ")
                            || next.starts_with("pub fn ")
                            || next.starts_with("pub const fn "),
                        "{name}:{}: the unsafe allow must sit on a function, found {next:?}",
                        i + 1
                    );
                } else {
                    assert!(!l.contains(&tag), "{name}:{}: {l}", i + 1);
                }
            }
            assert_eq!(count, want, "{name}: unsafe allow count");
            let has_unsafe = ["unsafe", " {"].concat();
            let has_unsafe_fn = ["unsafe", " fn"].concat();
            let has_unsafe_impl = ["unsafe", " impl"].concat();
            assert!(
                !code.contains(&has_unsafe_fn) && !code.contains(&has_unsafe_impl),
                "{name}"
            );
            assert_eq!(
                code.contains(&has_unsafe),
                want > 0,
                "{name}: unsafe blocks only in files with an allow"
            );
        }

        // (b) FFI surface: no extern blocks, no `use libc`, `libc::` only
        // in the two seam files, no ioctl macros.
        let extern_c = ["extern ", "\"C\""].concat();
        let extern_block = ["extern ", "{"].concat();
        let libc_path = ["lib", "c::"].concat();
        let use_libc = format!("use {libc_path}");
        let ioctl_macro = ["ioctl", "!"].concat();
        for (name, text) in &files {
            let code = strip_comments(text);
            assert!(!code.contains(&extern_c), "{name}: extern \"C\"");
            assert!(!code.contains(&extern_block), "{name}: extern block");
            assert!(!code.contains(&use_libc), "{name}: `use libc`");
            assert!(!code.contains(&ioctl_macro), "{name}: ioctl macro");
            let asm = ["global_", "asm"].concat();
            assert!(!code.contains(&asm), "{name}");
            if name != "src/v4l2.rs" && name != "src/sys.rs" {
                assert!(!code.contains(&libc_path), "{name}: names libc");
            }
        }

        // (c) Positive check: every VIDIOC_* token is one of the nine.
        let allowed = [
            "VIDIOC_QUERYCAP",
            "VIDIOC_G_FMT",
            "VIDIOC_S_FMT",
            "VIDIOC_REQBUFS",
            "VIDIOC_QUERYBUF",
            "VIDIOC_QBUF",
            "VIDIOC_DQBUF",
            "VIDIOC_STREAMON",
            "VIDIOC_STREAMOFF",
        ];
        let mut seen = std::collections::BTreeSet::new();
        // Assembled at run time so that this file does not itself contain
        // the forbidden identifiers.
        let forbidden: Vec<String> = [
            ("UVC", "IOC"),
            ("uvc_", "xu_"),
            ("USB", "DEVFS"),
            ("DFU", "_"),
            ("V4L2_", "CID_"),
            ("v4l2_", "control"),
            ("v4l2_", "ext_control"),
            ("v4l2_", "streamparm"),
            ("v4l2_", "query"),
        ]
        .iter()
        .map(|(a, b)| format!("{a}{b}"))
        .collect();
        let prefix = ["VIDI", "OC_"].concat();
        for (name, text) in &files {
            let code = strip_comments(text);
            for t in tokens(&code, &prefix) {
                assert!(
                    allowed.contains(&t.as_str()),
                    "{name}: {t} is not an allowed request"
                );
                seen.insert(t);
            }
            for f in &forbidden {
                assert!(!text.contains(f.as_str()), "{name} mentions {f}");
            }
        }
        assert_eq!(seen.len(), 9, "{seen:?}");

        // (d) Raw `ioctl(` call sites: exactly one (`libc::ioctl` in
        // v4l2.rs); `v4l2::ioctl(` only from sys.rs; everything else must
        // be a method call (`.ioctl(`) or a definition (`fn ioctl(`).
        let mut raw_sites = 0;
        for (name, text) in &files {
            let code = strip_comments(text);
            let b = code.as_bytes();
            let mut from = 0;
            while let Some(pos) = code[from..].find("ioctl") {
                let start = from + pos;
                let mut end = start + 5;
                while end < b.len() && b[end].is_ascii_whitespace() {
                    end += 1;
                }
                from = start + 5;
                if end >= b.len() || b[end] != b'(' {
                    continue; // not a call (a word, a path segment, ...)
                }
                let before = &code[..start];
                let prev = before.bytes().last().unwrap_or(b' ');
                if is_word(prev) {
                    continue; // e.g. `no_forbidden_ioctl(`... part of a longer word
                }
                if prev == b'.' {
                    continue; // `sys.ioctl(` / `s.ioctl(`: the trait method
                }
                if before.ends_with("fn ") {
                    continue; // a definition
                }
                if before.ends_with(&libc_path) {
                    assert_eq!(name, "src/v4l2.rs", "{name}: raw libc ioctl");
                    raw_sites += 1;
                    continue;
                }
                if before.ends_with("v4l2::") {
                    assert_eq!(name, "src/sys.rs", "{name}: wrapper call outside the seam");
                    continue;
                }
                if name == "src/v4l2.rs" && !before.ends_with("::") {
                    continue; // the wrapper calling itself in its own tests
                }
                panic!("{name}: unexpected ioctl call site at byte {start}");
            }
        }
        assert_eq!(
            raw_sites, 1,
            "exactly one raw ioctl call site (v4l2::ioctl)"
        );

        // (e) Dependencies: exactly the four, no dev/build deps, no build.rs.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let mut section = String::new();
        let mut deps = std::collections::BTreeSet::new();
        for l in manifest.lines().map(str::trim) {
            if l.starts_with('[') {
                section = l.to_string();
                assert!(
                    section != "[dev-dependencies]" && section != "[build-dependencies]",
                    "{section} not allowed"
                );
                continue;
            }
            if section == "[dependencies]"
                && let Some((k, _)) = l.split_once('=')
            {
                deps.insert(k.trim().to_string());
            }
            assert!(!l.starts_with("build"), "no build script: {l}");
        }
        let want: std::collections::BTreeSet<String> = ["libc", "serde", "thiserror", "toml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(deps, want);
        assert!(!root.join("build.rs").exists());

        // (f) The wrapper's enum is closed: nine variants, no catch-all.
        let v4l2 = &files.iter().find(|(n, _)| n == "src/v4l2.rs").unwrap().1;
        let enum_body = v4l2
            .split("pub enum Request<'a> {")
            .nth(1)
            .and_then(|s| s.split("\n}\n").next())
            .unwrap();
        let variants = enum_body
            .lines()
            .filter(|l| {
                l.trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
            })
            .count();
        assert_eq!(variants, 9, "{enum_body}");
    }

    /// The guard's own helpers behave.
    #[test]
    fn guard_helpers() {
        assert_eq!(strip_comments("a // b\nc /* d */ e"), "a \nc   e");
        assert_eq!(tokens("x AB_QBUF, XAB_A AB_B(", "AB_"), ["AB_QBUF", "AB_B"]);
        assert!(crate_sources().iter().any(|(n, _)| n == "src/v4l2.rs"));
    }
}
