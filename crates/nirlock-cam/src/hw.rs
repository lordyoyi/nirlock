//! Hardware profile (`hw/3277-0055.toml`, DESIGN §2.3): which USB device
//! the daemon accepts, which interface/index pair plays each role and the
//! exact formats every `S_FMT` must read back.
//!
//! The profile is data, not policy: `emitter` and `labeler` are validated
//! against the only values v1 accepts for authentication so that a profile
//! for another camera cannot silently switch the labelling source, and the
//! three formats (with the IR and RGB geometry) are pinned to the only
//! `S_FMT` payloads the daemon may negotiate (DESIGN §2.3 step 2): an
//! `/etc/nirlock/hw/` override can move interface/index numbers, never the
//! format the camera is asked for.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::v4l2::{V4L2_META_FMT_UVC_MSXU_1_5, V4L2_PIX_FMT_GREY, V4L2_PIX_FMT_MJPEG};
use crate::{Error, Result};

/// The profile shipped for the Zenbook UX3405CA camera, embedded so that
/// `nirlockctl` works from a checkout (`/usr/share/nirlock/hw/` is the
/// installed copy; `/etc/nirlock/hw/` overrides it).
pub const BUILTIN_3277_0055: &str = include_str!("../../../hw/3277-0055.toml");

/// The only emitter mode v1 authenticates with.
pub const EMITTER_FIRMWARE_STROBE: &str = "firmware-strobe";
/// The only frame labeller v1 authenticates with.
pub const LABELER_UVCM_METADATA: &str = "uvcm-metadata";
/// The only IR video format v1 negotiates (`S_FMT` payload).
pub const IR_FORMAT: &str = "GREY";
/// The only IR metadata format v1 negotiates.
pub const META_FORMAT: &str = "UVCM";
/// The only RGB format v1 negotiates (ADR-0006 lever).
pub const RGB_FORMAT: &str = "MJPG";

// Geometry is per camera, not per protocol. The first profile pinned
// 640x360 and 1280x720 because that is what the Zenbook UX3405CA reports,
// and pinning it made every other Windows Hello camera unprofilable — the
// one thing standing between this and being publishable. What the pinning
// was actually protecting is the *format* and the labelling source, and
// those stay pinned: a profile dropped into `/etc/nirlock/hw/` still cannot
// ask for RGB where IR is expected, nor switch the frame labeller.
//
// The bounds below are a sanity floor and ceiling, not a spec. The floor
// comes from Microsoft's own NIR camera requirement for Windows Hello
// (>= 340x340); anything smaller is not a face camera. The ceiling keeps a
// malformed profile from asking the driver for a frame large enough to
// matter.
pub const IR_MIN_DIM: u32 = 320;
pub const IR_MAX_DIM: u32 = 1920;
pub const IR_MIN_FPS: u32 = 5;
pub const IR_MAX_FPS: u32 = 120;
pub const RGB_MAX_DIM: u32 = 3840;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// `idVendor` as sysfs prints it (4 lowercase hex digits).
    pub vendor: String,
    /// `idProduct`, same form.
    pub product: String,
    /// The `removable` attribute the USB device must declare (`fixed`).
    pub removable: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IrNode {
    pub interface: u32,
    pub index: u32,
    /// FourCC (`GREY`).
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Exact `bytesused` of a complete frame (`width * height` for GREY).
    pub bytes: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetaNode {
    pub interface: u32,
    pub index: u32,
    /// FourCC (`UVCM`).
    pub format: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RgbNode {
    pub interface: u32,
    pub index: u32,
    /// FourCC (`MJPG`).
    pub format: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HwProfile {
    pub id: String,
    #[serde(rename = "match")]
    pub match_: Match,
    pub ir: IrNode,
    pub meta: MetaNode,
    pub rgb: RgbNode,
    pub emitter: String,
    pub labeler: String,
    #[serde(default)]
    pub notes: String,
}

fn pinned(s: &str, want: &str, field: &str) -> Result<()> {
    if s == want {
        Ok(())
    } else {
        Err(Error::Profile(format!(
            "{field}.format {s:?} is not accepted for authentication (only {want:?})"
        )))
    }
}

fn bounded(v: u32, lo: u32, hi: u32, field: &str) -> Result<()> {
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(Error::Profile(format!(
            "{field}: {v} is out of range ({lo}..={hi})"
        )))
    }
}

impl HwProfile {
    /// Parses and validates a profile.
    pub fn parse(text: &str) -> Result<Self> {
        let p: HwProfile = toml::from_str(text).map_err(|e| Error::Profile(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    /// Loads and validates a profile file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    /// The embedded Zenbook profile. It is validated at build time by the
    /// unit tests; a corrupt embedded profile is a programming error.
    pub fn builtin() -> Result<Self> {
        Self::parse(BUILTIN_3277_0055)
    }

    fn validate(&self) -> Result<()> {
        let hex4 = |s: &str, f: &str| -> Result<()> {
            if s.len() == 4
                && s.bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                Ok(())
            } else {
                Err(Error::Profile(format!(
                    "match.{f}: expected 4 lowercase hex digits, got {s:?}"
                )))
            }
        };
        hex4(&self.match_.vendor, "vendor")?;
        hex4(&self.match_.product, "product")?;
        if self.match_.removable != "fixed" {
            return Err(Error::Profile(format!(
                "match.removable must be \"fixed\" (an external clone reports \"removable\"), got {:?}",
                self.match_.removable
            )));
        }
        if self.emitter != EMITTER_FIRMWARE_STROBE {
            return Err(Error::Profile(format!(
                "emitter {:?} is not accepted for authentication (only {EMITTER_FIRMWARE_STROBE:?})",
                self.emitter
            )));
        }
        if self.labeler != LABELER_UVCM_METADATA {
            return Err(Error::Profile(format!(
                "labeler {:?} is not accepted for authentication (only {LABELER_UVCM_METADATA:?})",
                self.labeler
            )));
        }
        pinned(&self.ir.format, IR_FORMAT, "ir")?;
        pinned(&self.meta.format, META_FORMAT, "meta")?;
        pinned(&self.rgb.format, RGB_FORMAT, "rgb")?;
        bounded(self.ir.width, IR_MIN_DIM, IR_MAX_DIM, "ir.width")?;
        bounded(self.ir.height, IR_MIN_DIM, IR_MAX_DIM, "ir.height")?;
        bounded(self.ir.fps, IR_MIN_FPS, IR_MAX_FPS, "ir.fps")?;
        bounded(self.rgb.width, 1, RGB_MAX_DIM, "rgb.width")?;
        bounded(self.rgb.height, 1, RGB_MAX_DIM, "rgb.height")?;
        if self.ir.interface != self.meta.interface {
            return Err(Error::Profile(
                "meta must be the sibling node of ir (same interface)".into(),
            ));
        }
        if self.ir.index == self.meta.index {
            return Err(Error::Profile(
                "ir and meta must have distinct index".into(),
            ));
        }
        let expect = self
            .ir
            .width
            .checked_mul(self.ir.height)
            .ok_or_else(|| Error::Profile("ir: width*height overflows".into()))?;
        if self.ir.bytes != expect {
            return Err(Error::Profile(format!(
                "ir.bytes must equal width*height for GREY ({} != {expect})",
                self.ir.bytes,
            )));
        }
        Ok(())
    }

    /// The IR `S_FMT` pixel format: always `V4L2_PIX_FMT_GREY` (validation
    /// pinned `ir.format`; the constant is returned, not re-derived).
    pub fn ir_fourcc(&self) -> Result<u32> {
        pinned(&self.ir.format, IR_FORMAT, "ir").map(|()| V4L2_PIX_FMT_GREY)
    }

    /// The metadata `S_FMT` format: always `V4L2_META_FMT_UVC_MSXU_1_5`.
    pub fn meta_fourcc(&self) -> Result<u32> {
        pinned(&self.meta.format, META_FORMAT, "meta").map(|()| V4L2_META_FMT_UVC_MSXU_1_5)
    }

    /// The RGB `S_FMT` pixel format: always `V4L2_PIX_FMT_MJPEG`.
    pub fn rgb_fourcc(&self) -> Result<u32> {
        pinned(&self.rgb.format, RGB_FORMAT, "rgb").map(|()| V4L2_PIX_FMT_MJPEG)
    }
}

/// Where profiles are looked up, in priority order: a file in `/etc` shadows
/// the shipped one for the same USB device, so a user can correct a profile
/// without editing a package-owned file.
pub const PROFILE_DIRS: [&str; 2] = ["/etc/nirlock/hw", "/usr/share/nirlock/hw"];

/// Every profile the daemon knows about.
///
/// The daemon used to hold exactly one profile, compiled in, for the camera
/// this was developed on. That is why it could not be published: a second
/// machine would have been told its own camera "is not found". Discovery now
/// runs the other way round — enumerate what the machine actually has, then
/// look for a profile that claims it — and this is the lookup table.
#[derive(Clone, Debug, Default)]
pub struct ProfileSet {
    entries: Vec<Entry>,
    problems: Vec<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    /// Where it came from, for messages: a path, or "built-in".
    source: String,
    profile: HwProfile,
}

impl ProfileSet {
    /// Loads every profile under `PROFILE_DIRS`, with the embedded profile
    /// last so that a checkout with nothing installed still works.
    pub fn load() -> Self {
        let dirs: Vec<&Path> = PROFILE_DIRS.iter().map(Path::new).collect();
        Self::load_from(&dirs)
    }

    /// Loads from an explicit list of directories, in priority order.
    ///
    /// A missing directory is normal and silent. A file that does not parse
    /// is NOT silent: it is skipped and recorded in `problems()`, because a
    /// profile the user wrote and that never takes effect is exactly the
    /// failure that is impossible to diagnose from the outside.
    pub fn load_from(dirs: &[&Path]) -> Self {
        let mut set = Self::default();
        for dir in dirs {
            let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "toml"))
                    .collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    set.problems
                        .push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            // Deterministic order, so which of two profiles claiming the same
            // device wins does not depend on the filesystem.
            files.sort();
            for f in files {
                match HwProfile::load(&f) {
                    Ok(profile) => set.push(f.display().to_string(), profile),
                    Err(e) => set.problems.push(format!("{}: {e}", f.display())),
                }
            }
        }
        match HwProfile::builtin() {
            Ok(p) => set.push("built-in".into(), p),
            // A corrupt embedded profile is a programming error, and the unit
            // tests catch it; do not take the daemon down over it at runtime.
            Err(e) => set.problems.push(format!("built-in: {e}")),
        }
        set
    }

    /// Builds a set from profiles already in hand (tests, `probe`).
    pub fn from_profiles(profiles: Vec<HwProfile>) -> Self {
        let mut set = Self::default();
        for p in profiles {
            set.push("in-memory".into(), p);
        }
        set
    }

    /// Adds a profile unless the device it claims is already claimed. First
    /// wins, which is what makes `/etc` shadow `/usr/share`.
    fn push(&mut self, source: String, profile: HwProfile) {
        if self
            .find(&profile.match_.vendor, &profile.match_.product)
            .is_some()
        {
            return;
        }
        self.entries.push(Entry { source, profile });
    }

    /// The profile claiming this USB device, if any.
    pub fn find(&self, vendor: &str, product: &str) -> Option<&HwProfile> {
        self.entries
            .iter()
            .find(|e| e.profile.match_.vendor == vendor && e.profile.match_.product == product)
            .map(|e| &e.profile)
    }

    /// Where the profile for this device was read from.
    pub fn source_of(&self, vendor: &str, product: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.profile.match_.vendor == vendor && e.profile.match_.product == product)
            .map(|e| e.source.as_str())
    }

    /// Every device claimed, as `vid:pid (id)`, for messages.
    pub fn claimed(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| {
                format!(
                    "{}:{} ({})",
                    e.profile.match_.vendor, e.profile.match_.product, e.profile.id
                )
            })
            .collect()
    }

    /// Every profile, in priority order.
    pub fn iter(&self) -> impl Iterator<Item = &HwProfile> {
        self.entries.iter().map(|e| &e.profile)
    }

    /// Files that looked like profiles and were skipped, with the reason.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4l2::{V4L2_META_FMT_UVC_MSXU_1_5, V4L2_PIX_FMT_GREY, V4L2_PIX_FMT_MJPEG};

    #[test]
    fn builtin_profile_is_the_design_one() {
        let p = HwProfile::builtin().unwrap();
        assert_eq!(p.id, "shinetech-3277-0055");
        assert_eq!(
            p.match_,
            Match {
                vendor: "3277".into(),
                product: "0055".into(),
                removable: "fixed".into()
            }
        );
        assert_eq!(
            p.ir,
            IrNode {
                interface: 2,
                index: 0,
                format: "GREY".into(),
                width: 640,
                height: 360,
                fps: 15,
                bytes: 230_400
            }
        );
        assert_eq!(
            p.meta,
            MetaNode {
                interface: 2,
                index: 1,
                format: "UVCM".into()
            }
        );
        assert_eq!(
            p.rgb,
            RgbNode {
                interface: 0,
                index: 0,
                format: "MJPG".into(),
                width: 1280,
                height: 720
            }
        );
        assert_eq!(p.emitter, EMITTER_FIRMWARE_STROBE);
        assert_eq!(p.labeler, LABELER_UVCM_METADATA);
        assert!(p.notes.contains("DFU"));
        assert_eq!(p.ir_fourcc().unwrap(), V4L2_PIX_FMT_GREY);
        assert_eq!(p.meta_fourcc().unwrap(), V4L2_META_FMT_UVC_MSXU_1_5);
        assert_eq!(p.rgb_fourcc().unwrap(), V4L2_PIX_FMT_MJPEG);
    }

    #[test]
    fn load_reads_the_repo_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hw/3277-0055.toml");
        let p = HwProfile::load(&path).unwrap();
        assert_eq!(p, HwProfile::builtin().unwrap());
        assert!(matches!(
            HwProfile::load(Path::new("/nonexistent/hw.toml")),
            Err(Error::Io { .. })
        ));
    }

    fn edited(from: &str, to: &str) -> Result<HwProfile> {
        assert!(BUILTIN_3277_0055.contains(from), "{from} not in profile");
        HwProfile::parse(&BUILTIN_3277_0055.replace(from, to))
    }

    #[test]
    fn rejects_other_emitter_or_labeler() {
        let e = edited("\"firmware-strobe\"", "\"host-toggle\"").unwrap_err();
        assert!(
            matches!(e, Error::Profile(ref m) if m.contains("emitter")),
            "{e}"
        );
        let e = edited("\"uvcm-metadata\"", "\"brightness\"").unwrap_err();
        assert!(
            matches!(e, Error::Profile(ref m) if m.contains("labeler")),
            "{e}"
        );
    }

    #[test]
    fn rejects_removable_bad_hex_and_unknown_keys() {
        assert!(edited("removable = \"fixed\"", "removable = \"removable\"").is_err());
        assert!(edited("vendor = \"3277\"", "vendor = \"3277a\"").is_err());
        assert!(edited("vendor = \"3277\"", "vendor = \"32G7\"").is_err());
        assert!(edited("product = \"0055\"", "product = \"00AB\"").is_err());
        assert!(edited("id = ", "unknown_key = 1\nid = ").is_err());
        assert!(edited("bytes = 230400", "bytes = 230399").is_err());
        assert!(edited("format = \"GREY\"", "format = \"GREYS\"").is_err());
        // A width*height that overflows u32 is rejected loudly, not wrapped.
        assert!(
            edited(
                "width = 640, height = 360, fps = 15, bytes = 230400",
                "width = 65536, height = 65536, fps = 15, bytes = 0"
            )
            .is_err()
        );
        assert!(edited("meta = { interface = 2", "meta = { interface = 0").is_err());
        assert!(
            edited(
                "index = 1, format = \"UVCM\"",
                "index = 0, format = \"UVCM\""
            )
            .is_err()
        );
        assert!(matches!(
            HwProfile::parse("not = [toml"),
            Err(Error::Profile(_))
        ));
    }

    #[test]
    fn formats_are_pinned_but_geometry_is_per_camera() {
        // The format and the labelling source are what the pinning protects:
        // a profile must not be able to put RGB where IR is expected.
        for (from, to, what) in [
            ("format = \"GREY\"", "format = \"YUYV\"", "ir.format"),
            ("format = \"GREY\"", "format = \"grey\"", "ir.format"),
            ("format = \"UVCM\"", "format = \"UVCH\"", "meta.format"),
            ("format = \"MJPG\"", "format = \"YUYV\"", "rgb.format"),
            ("format = \"MJPG\"", "format = \"€1\"", "rgb.format"),
        ] {
            let e = edited(from, to).unwrap_err();
            assert!(
                matches!(e, Error::Profile(ref m) if m.contains(what)),
                "{from} -> {to}: {e}"
            );
        }

        // Geometry is not. Another Windows Hello camera reporting 640x480 IR
        // and 1920x1080 RGB must profile cleanly: rejecting it is what made
        // this unpublishable.
        let p = edited(
            "width = 640, height = 360, fps = 15, bytes = 230400",
            "width = 640, height = 480, fps = 30, bytes = 307200",
        )
        .unwrap();
        assert_eq!((p.ir.width, p.ir.height, p.ir.fps), (640, 480, 30));
        let p = edited("width = 1280, height = 720", "width = 1920, height = 1080").unwrap();
        assert_eq!((p.rgb.width, p.rgb.height), (1920, 1080));

        // The bounds still hold: too small to be a face camera, too fast,
        // and a byte count that disagrees with the geometry.
        for (from, to, what) in [
            (
                "width = 640, height = 360, fps = 15, bytes = 230400",
                "width = 64, height = 64, fps = 15, bytes = 4096",
                "ir.width",
            ),
            (
                "width = 640, height = 360, fps = 15, bytes = 230400",
                "width = 640, height = 360, fps = 240, bytes = 230400",
                "ir.fps",
            ),
            (
                "width = 640, height = 360, fps = 15, bytes = 230400",
                "width = 640, height = 480, fps = 15, bytes = 230400",
                "ir.bytes",
            ),
        ] {
            let e = edited(from, to).unwrap_err();
            assert!(
                matches!(e, Error::Profile(ref m) if m.contains(what)),
                "{from} -> {to}: {e}"
            );
        }

        // Interface/index may move (that is what an override is for).
        let p = edited(
            "rgb  = { interface = 0, index = 0",
            "rgb  = { interface = 4, index = 0",
        )
        .unwrap();
        assert_eq!(p.rgb.interface, 4);
        assert_eq!(p.rgb_fourcc().unwrap(), V4L2_PIX_FMT_MJPEG);
    }

    #[test]
    fn profile_set_shadows_by_priority_and_never_hides_a_broken_file() {
        let base = std::env::temp_dir().join(format!("nirlock-hw-{}", std::process::id()));
        let etc = base.join("etc");
        let share = base.join("share");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::create_dir_all(&share).unwrap();

        // The same camera claimed twice, with different ids.
        std::fs::write(
            etc.join("3277-0055.toml"),
            BUILTIN_3277_0055.replace("shinetech-3277-0055", "local-override"),
        )
        .unwrap();
        std::fs::write(
            share.join("3277-0055.toml"),
            BUILTIN_3277_0055.replace("shinetech-3277-0055", "packaged"),
        )
        .unwrap();
        // A different camera, only in the packaged directory.
        std::fs::write(
            share.join("04f2-b6d0.toml"),
            BUILTIN_3277_0055
                .replace("vendor = \"3277\"", "vendor = \"04f2\"")
                .replace("product = \"0055\"", "product = \"b6d0\"")
                .replace("shinetech-3277-0055", "other-camera"),
        )
        .unwrap();
        // A file the user wrote that does not parse. Skipping it silently is
        // the one failure that cannot be diagnosed from outside, so it has to
        // show up in problems().
        std::fs::write(share.join("broken.toml"), "id = [this is not toml").unwrap();
        // Not a profile at all: ignored without complaint.
        std::fs::write(share.join("README.md"), "# not a profile").unwrap();

        let set = ProfileSet::load_from(&[etc.as_path(), share.as_path()]);

        assert_eq!(set.find("3277", "0055").unwrap().id, "local-override");
        assert!(set.source_of("3277", "0055").unwrap().contains("etc"));
        assert_eq!(set.find("04f2", "b6d0").unwrap().id, "other-camera");
        assert!(set.find("dead", "beef").is_none());
        // The embedded profile is always there as a last resort, but it must
        // not displace a file that claims the same device.
        assert_eq!(set.len(), 2);

        let problems = set.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("broken.toml"), "{problems:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn profile_set_falls_back_to_the_embedded_profile() {
        let set = ProfileSet::load_from(&[Path::new("/nonexistent/nirlock/hw")]);
        assert_eq!(set.len(), 1);
        assert_eq!(set.find("3277", "0055").unwrap().id, "shinetech-3277-0055");
        assert_eq!(set.source_of("3277", "0055").unwrap(), "built-in");
        assert!(set.problems().is_empty(), "{:?}", set.problems());
    }

    #[test]
    fn notes_are_optional() {
        let text = BUILTIN_3277_0055
            .lines()
            .filter(|l| !l.starts_with("notes"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(HwProfile::parse(&text).unwrap().notes, "");
    }
}
