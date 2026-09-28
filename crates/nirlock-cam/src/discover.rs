//! Camera discovery over sysfs (port of `discover_camera` in
//! `phase0/v4l2cap.cpp`) plus the pinning of DESIGN §2.3 step 1.
//!
//! Walks `<root>/class/video4linux/video*` in **numeric** node order
//! (`video2` before `video12`; names without a number sort last, by name)
//! and keeps the nodes whose `device` link resolves to a USB interface that
//! is bound to `uvcvideo`, whose parent USB device matches the profile
//! (`3277:0055`, `removable = fixed`; an external clone of the VID:PID
//! reports `removable`). Roles come from the profile's `interface × index`
//! pairs (interface 2 index 0 = IR video, index 1 = its metadata node,
//! interface 0 index 0 = RGB).
//!
//! Every matching USB device is a candidate ([`discover_all_in`]); the
//! daemon then [`pin`]s: with a pinned `usb_sysfs` (from the template
//! manifest) the candidate at that path is required (`camera_mismatch`
//! otherwise); without one (enrolment) exactly one candidate is required
//! (`camera_ambiguous`); either way the IR metadata sibling must exist.
//! v4l2loopback and other virtual nodes never qualify (no USB parent, no
//! `uvcvideo` driver link) and are listed in [`Camera::refused`] for
//! `doctor`; `Stream::open`'s `QUERYCAP` check is the second line of
//! defence.
//!
//! Nothing here opens `/dev/video*`.

use std::path::{Path, PathBuf};

use crate::hw::{HwProfile, ProfileSet};
use crate::{Error, Result};

/// The Zenbook's IR/RGB camera (USB VID:PID); the built-in profile's values.
pub const VID: &str = "3277";
pub const PID: &str = "0055";

/// One `videoN` node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// `/sys/class/video4linux/videoN` (under the discovery root).
    pub sysfs: PathBuf,
    /// `/dev/videoN`.
    pub dev: PathBuf,
    /// Contents of `name` (e.g. `USB Camera: IR Camera`).
    pub card: String,
    /// `bInterfaceNumber` of the owning USB interface.
    pub iface: u32,
    /// `index` within the interface (0 = video, 1 = metadata).
    pub index: u32,
    /// The class node's `dev` attribute (`major:minor`); what `open(2)` of
    /// `dev` must refer to (checked with `fstat` at open time).
    pub rdev: Option<(u32, u32)>,
}

impl Node {
    pub fn found(&self) -> bool {
        !self.dev.as_os_str().is_empty()
    }
}

/// The four nodes of the camera plus the USB device identity used for
/// pinning (`usb_sysfs`, `bcdDevice`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Camera {
    pub usb_sysfs: PathBuf,
    pub bcd_device: String,
    pub rgb_cap: Node,
    pub rgb_meta: Node,
    pub ir_cap: Node,
    pub ir_meta: Node,
    /// Other USB devices that matched the profile too (`camera_ambiguous`
    /// when pinning without a path).
    pub other_devices: Vec<PathBuf>,
    /// Class nodes refused at discovery with the reason (virtual /
    /// v4l2loopback / non-uvcvideo), for `doctor`.
    pub refused: Vec<(PathBuf, String)>,
}

/// First line of a sysfs attribute, trailing newline/space stripped; empty
/// when unreadable (as `read_sysfs` in the C++).
pub fn read_attr(path: &Path) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return String::new();
    };
    s.lines()
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '\n', '\r'])
        .to_string()
}

/// `major:minor` as sysfs prints it in a `dev` attribute.
pub fn parse_rdev(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// The `bus_info` uvcvideo reports for the device at `usb_sysfs`
/// (`usb_make_path`: `usb-<bus name>-<devpath>`, the bus name being the
/// host controller's PCI id and `devpath` the port path). The sysfs
/// directory of a USB device is `<controller>/usb<busnum>/<busnum>-<devpath>`,
/// so `/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9` → `usb-0000:00:14.0-9`
/// and `.../usb1/1-2.4` → `usb-0000:00:14.0-2.4`.
pub fn usb_bus_info(usb_sysfs: &Path) -> Option<String> {
    let name = usb_sysfs.file_name()?.to_str()?;
    let (busnum, devpath) = name.split_once('-')?;
    let bus_dir = usb_sysfs.parent()?;
    let bus_name = bus_dir.file_name()?.to_str()?;
    if bus_name != format!("usb{busnum}") || devpath.is_empty() {
        return None;
    }
    let controller = bus_dir.parent()?.file_name()?.to_str()?;
    Some(format!("usb-{controller}-{devpath}"))
}

/// Discovers the camera under the live sysfs (`/sys`) with the built-in
/// profile; first candidate.
pub fn discover() -> Result<Camera> {
    discover_in(Path::new("/sys"))
}

/// All candidates under the live sysfs.
pub fn discover_all(profile: &HwProfile) -> Result<Vec<Camera>> {
    discover_all_in(Path::new("/sys"), profile)
}

/// Sort key for `videoN` entries: the numeric suffix, so that `video2`
/// precedes `video12` (plain lexical order would not), then the name.
/// "First matching USB device wins" therefore means the device owning the
/// lowest-numbered matching node.
fn node_sort_key(p: &Path) -> (u64, String) {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let num = name
        .strip_prefix("video")
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    (num, name)
}

/// Discovers the camera under an arbitrary sysfs root with the built-in
/// profile (tests pass a fake tree; the daemon passes `/sys`). Returns the
/// first candidate, the others listed in `other_devices`.
pub fn discover_in(root: &Path) -> Result<Camera> {
    let profile = HwProfile::builtin()?;
    let mut all = discover_all_in(root, &profile)?;
    if all.is_empty() {
        return Err(not_found(root, &profile));
    }
    Ok(all.swap_remove(0))
}

fn not_found(root: &Path, profile: &HwProfile) -> Error {
    Error::NotFound {
        vid: profile.match_.vendor.clone(),
        pid: profile.match_.product.clone(),
        root: root.join("class/video4linux").display().to_string(),
    }
}

/// Why a class node does not qualify, when that is worth listing.
fn refusal_reason(class_node: &Path, iface: Option<&Path>) -> Option<String> {
    let name = read_attr(&class_node.join("name"));
    let virtual_ = iface.is_none_or(|i| i.components().any(|c| c.as_os_str() == "virtual"));
    if name.contains("v4l2loopback") || name.starts_with("Dummy video device") {
        return Some(format!("v4l2loopback ('{name}')"));
    }
    if virtual_ {
        return Some(format!("virtual device without a USB parent ('{name}')"));
    }
    None
}

/// Every USB device matching `profile` that owns an IR video node, in
/// node order; each with all of its nodes and the shared `refused` list.
pub fn discover_all_in(root: &Path, profile: &HwProfile) -> Result<Vec<Camera>> {
    let class = root.join("class/video4linux");
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(&class) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(Error::Io {
                path: class.display().to_string(),
                source: e,
            });
        }
    };
    entries.sort_by_cached_key(|p| node_sort_key(p));
    let mut cams: Vec<Camera> = Vec::new();
    let mut refused: Vec<(PathBuf, String)> = Vec::new();
    for p in entries {
        // videoN/device -> the USB *interface* directory (e.g. .../3-9/3-9:1.2)
        let iface = std::fs::canonicalize(p.join("device")).ok();
        // The interface must be bound to uvcvideo. v4l2loopback devices are
        // virtual (no USB parent, no such driver link) and fail right here;
        // the QUERYCAP check at open time is the second line of defence.
        let drv = iface
            .as_deref()
            .and_then(|i| std::fs::canonicalize(i.join("driver")).ok());
        let is_uvc = drv
            .as_deref()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "uvcvideo");
        if !is_uvc {
            if let Some(r) = refusal_reason(&p, iface.as_deref()) {
                refused.push((p.clone(), r));
            }
            continue;
        }
        let Some(iface) = iface else { continue };
        let ifnum = read_attr(&iface.join("bInterfaceNumber"));
        if ifnum.is_empty() {
            continue;
        }
        let Some(usbdev) = iface.parent() else {
            continue;
        };
        if read_attr(&usbdev.join("idVendor")) != profile.match_.vendor
            || read_attr(&usbdev.join("idProduct")) != profile.match_.product
        {
            continue;
        }
        // 'fixed' = the firmware declares the port as internal. An attacker's
        // external camera with a cloned VID:PID reports 'removable'.
        if read_attr(&usbdev.join("removable")) != profile.match_.removable {
            continue;
        }
        let Ok(iface_no) = u32::from_str_radix(&ifnum, 16) else {
            continue;
        };
        let Some(name) = p.file_name() else {
            continue;
        };
        // A class node without a device number cannot be opened (or pinned).
        let Some(rdev) = parse_rdev(&read_attr(&p.join("dev"))) else {
            continue;
        };
        let n = Node {
            sysfs: p.clone(),
            dev: Path::new("/dev").join(name),
            card: read_attr(&p.join("name")),
            iface: iface_no,
            index: read_attr(&p.join("index")).parse().unwrap_or(0),
            rdev: Some(rdev),
        };
        let cam = match cams.iter_mut().find(|c| c.usb_sysfs == usbdev) {
            Some(c) => c,
            None => {
                cams.push(Camera {
                    usb_sysfs: usbdev.to_path_buf(),
                    bcd_device: read_attr(&usbdev.join("bcdDevice")),
                    ..Camera::default()
                });
                cams.last_mut()
                    .unwrap_or_else(|| unreachable!("just pushed"))
            }
        };
        let key = (n.iface, n.index);
        if key == (profile.rgb.interface, profile.rgb.index) {
            cam.rgb_cap = n;
        } else if key == (profile.rgb.interface, profile.rgb.index + 1) {
            cam.rgb_meta = n;
        } else if key == (profile.ir.interface, profile.ir.index) {
            cam.ir_cap = n;
        } else if key == (profile.meta.interface, profile.meta.index) {
            cam.ir_meta = n;
        }
    }
    // A device without the IR video node is not a candidate at all.
    cams.retain(|c| c.ir_cap.found());
    let paths: Vec<PathBuf> = cams.iter().map(|c| c.usb_sysfs.clone()).collect();
    for c in &mut cams {
        c.other_devices = paths
            .iter()
            .filter(|p| **p != c.usb_sysfs)
            .cloned()
            .collect();
        c.refused = refused.clone();
    }
    Ok(cams)
}

/// A USB device bound to `uvcvideo`, whether or not any profile claims it.
///
/// Discovery used to start from the profile and ask "is this device here?",
/// which meant a machine with a different camera was told its camera did not
/// exist. This is the other half: what the machine actually has, reported
/// without filtering, so an unclaimed camera can be named in an error and
/// turned into a profile instead of being invisible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsbCamera {
    pub usb_sysfs: PathBuf,
    /// `idVendor`/`idProduct` as sysfs prints them (4 lowercase hex digits).
    pub vendor: String,
    pub product: String,
    /// The USB product string, for humans ("Integrated Camera").
    pub name: String,
    /// `fixed` for a built-in camera, `removable` for one plugged in.
    pub removable: String,
    /// Every video/metadata node the device owns, in node order.
    pub nodes: Vec<Node>,
}

impl UsbCamera {
    /// `vid:pid 'name'`, for messages.
    pub fn label(&self) -> String {
        format!("{}:{} '{}'", self.vendor, self.product, self.name)
    }
}

/// Every uvcvideo USB device under `root`, in node order, unfiltered.
pub fn usb_cameras_in(root: &Path) -> Vec<UsbCamera> {
    let class = root.join("class/video4linux");
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(&class) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_cached_key(|p| node_sort_key(p));
    let mut cams: Vec<UsbCamera> = Vec::new();
    for p in entries {
        let Some(iface) = std::fs::canonicalize(p.join("device")).ok() else {
            continue;
        };
        let is_uvc = std::fs::canonicalize(iface.join("driver"))
            .ok()
            .as_deref()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "uvcvideo");
        if !is_uvc {
            continue;
        }
        let ifnum = read_attr(&iface.join("bInterfaceNumber"));
        let Ok(iface_no) = u32::from_str_radix(&ifnum, 16) else {
            continue;
        };
        let Some(usbdev) = iface.parent() else { continue };
        let Some(name) = p.file_name() else { continue };
        let node = Node {
            sysfs: p.clone(),
            dev: Path::new("/dev").join(name),
            card: read_attr(&p.join("name")),
            iface: iface_no,
            index: read_attr(&p.join("index")).parse().unwrap_or(0),
            rdev: parse_rdev(&read_attr(&p.join("dev"))),
        };
        match cams.iter_mut().find(|c| c.usb_sysfs == usbdev) {
            Some(c) => c.nodes.push(node),
            None => cams.push(UsbCamera {
                usb_sysfs: usbdev.to_path_buf(),
                vendor: read_attr(&usbdev.join("idVendor")),
                product: read_attr(&usbdev.join("idProduct")),
                name: read_attr(&usbdev.join("product")),
                removable: read_attr(&usbdev.join("removable")),
                nodes: vec![node],
            }),
        }
    }
    cams
}

/// Every uvcvideo USB device on the live sysfs.
pub fn usb_cameras() -> Vec<UsbCamera> {
    usb_cameras_in(Path::new("/sys"))
}

/// Picks the camera to use and the profile that claims it.
///
/// Profiles are tried in priority order and the first that finds a camera
/// wins, so an `/etc/nirlock/hw/` override takes effect simply by being
/// loaded first. When nothing matches, the error names the cameras that ARE
/// present: "no profile for 04f2:b6d0" is actionable, "camera not found" is
/// not.
pub fn select_in(
    root: &Path,
    set: &ProfileSet,
    pinned: Option<&Path>,
) -> Result<(Camera, HwProfile)> {
    for profile in set.iter() {
        let cams = discover_all_in(root, profile)?;
        if cams.is_empty() {
            continue;
        }
        return pin(cams, pinned, root, profile).map(|c| (c, profile.clone()));
    }
    Err(Error::NoProfile {
        devices: usb_cameras_in(root)
            .iter()
            .map(|c| c.label())
            .collect(),
        known: set.claimed(),
    })
}

/// `select_in` on the live sysfs with the installed profiles.
///
/// Profile files that failed to load are returned so the caller can report
/// them. Collecting them and never printing them would defeat the point.
pub fn select(pinned: Option<&Path>) -> Result<(Camera, HwProfile, Vec<String>)> {
    let set = ProfileSet::load();
    let problems = set.problems().to_vec();
    select_in(Path::new("/sys"), &set, pinned).map(|(c, p)| (c, p, problems))
}

/// DESIGN §2.3 step 1: selects the camera to use. `pinned` is the
/// `usb_sysfs` recorded in the template manifest at enrolment (`None` at
/// enrolment itself). The chosen camera must carry the IR metadata
/// sibling node.
pub fn pin(
    candidates: Vec<Camera>,
    pinned: Option<&Path>,
    root: &Path,
    profile: &HwProfile,
) -> Result<Camera> {
    let found: Vec<String> = candidates
        .iter()
        .map(|c| c.usb_sysfs.display().to_string())
        .collect();
    let cam = match pinned {
        Some(p) => candidates
            .into_iter()
            .find(|c| c.usb_sysfs == p)
            .ok_or_else(|| {
                if found.is_empty() {
                    not_found(root, profile)
                } else {
                    Error::Mismatch {
                        pinned: p.display().to_string(),
                        found,
                    }
                }
            })?,
        None => {
            let mut it = candidates.into_iter();
            let Some(first) = it.next() else {
                return Err(not_found(root, profile));
            };
            if it.next().is_some() {
                return Err(Error::Ambiguous { devices: found });
            }
            first
        }
    };
    if !cam.ir_meta.found() {
        return Err(Error::NoMetaNode {
            usb_sysfs: cam.usb_sysfs.display().to_string(),
        });
    }
    Ok(cam)
}

/// `power/runtime_status` of the pinned USB device (`active` / `suspended`).
pub fn usb_runtime_status(cam: &Camera) -> String {
    read_attr(&cam.usb_sysfs.join("power/runtime_status"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// Builds a fake sysfs. Layout mirrors the real one:
    /// `devices/pci.../usb3/3-9` (USB device) → `3-9:1.0`, `3-9:1.2`
    /// (interfaces, `driver` → `bus/usb/drivers/uvcvideo`), each holding
    /// `video4linux/videoN`; `class/video4linux/videoN` → symlink to it.
    struct Fake {
        root: PathBuf,
    }

    impl Fake {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nirlock-sysfs-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("class/video4linux")).unwrap();
            fs::create_dir_all(root.join("bus/usb/drivers/uvcvideo")).unwrap();
            fs::create_dir_all(root.join("bus/usb/drivers/snd-usb-audio")).unwrap();
            Self { root }
        }

        fn usb_device(
            &self,
            name: &str,
            vid: &str,
            pid: &str,
            removable: &str,
            bcd: &str,
        ) -> PathBuf {
            let d = self
                .root
                .join("devices/pci0000:00/0000:00:14.0/usb3")
                .join(name);
            fs::create_dir_all(d.join("power")).unwrap();
            fs::write(d.join("idVendor"), format!("{vid}\n")).unwrap();
            fs::write(d.join("idProduct"), format!("{pid}\n")).unwrap();
            fs::write(d.join("removable"), format!("{removable}\n")).unwrap();
            fs::write(d.join("bcdDevice"), format!("{bcd}\n")).unwrap();
            fs::write(d.join("power/runtime_status"), "suspended\n").unwrap();
            d
        }

        fn iface(&self, usbdev: &Path, ifnum: &str, driver: &str) -> PathBuf {
            let name = format!(
                "{}:1.{}",
                usbdev.file_name().unwrap().to_string_lossy(),
                u32::from_str_radix(ifnum, 16).unwrap()
            );
            let i = usbdev.join(name);
            fs::create_dir_all(&i).unwrap();
            fs::write(i.join("bInterfaceNumber"), format!("{ifnum}\n")).unwrap();
            symlink(
                self.root.join("bus/usb/drivers").join(driver),
                i.join("driver"),
            )
            .unwrap();
            i
        }

        fn video(&self, iface: &Path, n: u32, index: u32, card: &str) {
            let v = iface.join("video4linux").join(format!("video{n}"));
            fs::create_dir_all(&v).unwrap();
            fs::write(v.join("name"), format!("{card}\n")).unwrap();
            fs::write(v.join("index"), format!("{index}\n")).unwrap();
            fs::write(v.join("dev"), format!("81:{n}\n")).unwrap();
            symlink(iface, v.join("device")).unwrap();
            symlink(
                &v,
                self.root
                    .join("class/video4linux")
                    .join(format!("video{n}")),
            )
            .unwrap();
        }

        fn zenbook_ir_only(&self, dev: &Path) {
            let i2 = self.iface(dev, "02", "uvcvideo");
            self.video(&i2, 2, 0, "USB Camera: IR Camera");
            self.video(&i2, 3, 1, "USB Camera: IR Camera");
        }

        fn zenbook(&self) -> PathBuf {
            let dev = self.usb_device("3-9", "3277", "0055", "fixed", "0002");
            let i0 = self.iface(&dev, "00", "uvcvideo");
            let i2 = self.iface(&dev, "02", "uvcvideo");
            self.video(&i0, 0, 0, "USB Camera: USB Camera");
            self.video(&i0, 1, 1, "USB Camera: USB Camera");
            self.video(&i2, 2, 0, "USB Camera: IR Camera");
            self.video(&i2, 3, 1, "USB Camera: IR Camera");
            dev
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn usb_cameras_lists_devices_no_profile_claims() {
        let f = Fake::new("unclaimed");
        f.zenbook();
        // A second camera nothing knows about: the case that made this
        // unpublishable, because the old discovery simply could not see it.
        let other = f.usb_device("3-4", "04f2", "b6d0", "fixed", "0011");
        fs::write(other.join("product"), "Integrated Camera\n").unwrap();
        let i0 = f.iface(&other, "00", "uvcvideo");
        f.video(&i0, 8, 0, "Integrated Camera");

        let cams = usb_cameras_in(&f.root);
        let labels: Vec<String> = cams.iter().map(|c| c.label()).collect();
        assert_eq!(cams.len(), 2, "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("04f2:b6d0")),
            "the unclaimed camera must be visible: {labels:?}"
        );
        let unclaimed = cams
            .iter()
            .find(|c| c.vendor == "04f2")
            .expect("unclaimed camera");
        assert_eq!(unclaimed.name, "Integrated Camera");
        assert_eq!(unclaimed.removable, "fixed");
        assert_eq!(unclaimed.nodes.len(), 1);
    }

    #[test]
    fn select_names_the_camera_present_when_no_profile_claims_it() {
        let f = Fake::new("noprofile");
        let dev = f.usb_device("3-4", "04f2", "b6d0", "fixed", "0011");
        fs::write(dev.join("product"), "Integrated Camera\n").unwrap();
        let i0 = f.iface(&dev, "00", "uvcvideo");
        f.video(&i0, 0, 0, "Integrated Camera");

        let set = ProfileSet::from_profiles(vec![HwProfile::builtin().unwrap()]);
        let e = select_in(&f.root, &set, None).unwrap_err();
        let msg = e.to_string();
        // "camera not found" is true and useless. The message has to name
        // what IS there, or the user has nothing to act on.
        assert!(matches!(e, Error::NoProfile { .. }), "{e}");
        assert!(msg.contains("04f2:b6d0"), "{msg}");
        assert!(msg.contains("Integrated Camera"), "{msg}");
        assert!(msg.contains("3277:0055"), "known profiles listed: {msg}");
        assert_eq!(e.reason(), "camera_missing");
    }

    #[test]
    fn select_returns_the_profile_that_claimed_the_camera() {
        let f = Fake::new("selectok");
        let dev = f.zenbook();
        let set = ProfileSet::from_profiles(vec![HwProfile::builtin().unwrap()]);
        let (cam, profile) = select_in(&f.root, &set, None).unwrap();
        assert_eq!(cam.usb_sysfs, dev);
        assert_eq!(profile.match_.product, "0055");
    }

    #[test]
    fn finds_all_four_nodes() {
        let f = Fake::new("ok");
        let dev = f.zenbook();
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.usb_sysfs, dev);
        assert_eq!(cam.bcd_device, "0002");
        assert_eq!(cam.ir_cap.dev, Path::new("/dev/video2"));
        assert_eq!(cam.ir_meta.dev, Path::new("/dev/video3"));
        assert_eq!(cam.rgb_cap.dev, Path::new("/dev/video0"));
        assert_eq!(cam.rgb_meta.dev, Path::new("/dev/video1"));
        assert_eq!(cam.ir_cap.card, "USB Camera: IR Camera");
        assert_eq!((cam.ir_cap.iface, cam.ir_cap.index), (2, 0));
        assert_eq!((cam.ir_meta.iface, cam.ir_meta.index), (2, 1));
        assert_eq!(cam.ir_cap.rdev, Some((81, 2)));
        assert_eq!(cam.ir_meta.rdev, Some((81, 3)));
        assert_eq!(cam.rgb_cap.rdev, Some((81, 0)));
        assert_eq!(usb_runtime_status(&cam), "suspended");
        assert_eq!(
            usb_bus_info(&cam.usb_sysfs).as_deref(),
            Some("usb-0000:00:14.0-9")
        );
    }

    #[test]
    fn node_without_dev_attribute_is_skipped() {
        let f = Fake::new("nodev");
        let dev = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let i2 = f.iface(&dev, "02", "uvcvideo");
        f.video(&i2, 2, 0, "IR");
        f.video(&i2, 3, 1, "IR meta");
        fs::remove_file(i2.join("video4linux/video2/dev")).unwrap();
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
    }

    #[test]
    fn bus_info_derivation_matches_usb_make_path() {
        let p = |s: &str| usb_bus_info(Path::new(s));
        assert_eq!(
            p("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9").as_deref(),
            Some("usb-0000:00:14.0-9")
        );
        assert_eq!(
            p("/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2.4").as_deref(),
            Some("usb-0000:00:14.0-2.4")
        );
        // Interface dir, bus mismatch, root hub, garbage: none derivable.
        assert_eq!(
            p("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9/3-9:1.2"),
            None
        );
        assert_eq!(p("/sys/devices/pci0000:00/0000:00:14.0/usb1/3-9"), None);
        assert_eq!(p("/sys/devices/pci0000:00/0000:00:14.0/usb3"), None);
        assert_eq!(p("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-"), None);
        assert_eq!(p(""), None);
        assert_eq!(parse_rdev("81:2"), Some((81, 2)));
        assert_eq!(parse_rdev("81"), None);
        assert_eq!(parse_rdev("x:2"), None);
        assert_eq!(parse_rdev(""), None);
    }

    #[test]
    fn wrong_vid_is_ignored() {
        let f = Fake::new("vid");
        let dev = f.usb_device("3-9", "046d", "0055", "fixed", "0002");
        let i2 = f.iface(&dev, "02", "uvcvideo");
        f.video(&i2, 2, 0, "Other");
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
    }

    #[test]
    fn removable_clone_is_ignored() {
        let f = Fake::new("removable");
        let dev = f.usb_device("3-9", "3277", "0055", "removable", "0002");
        let i2 = f.iface(&dev, "02", "uvcvideo");
        f.video(&i2, 2, 0, "Clone");
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
    }

    #[test]
    fn non_uvc_driver_is_ignored() {
        let f = Fake::new("driver");
        let dev = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let i2 = f.iface(&dev, "02", "snd-usb-audio");
        f.video(&i2, 2, 0, "Audio?");
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
    }

    #[test]
    fn loopback_without_usb_parent_is_ignored() {
        let f = Fake::new("loopback");
        // v4l2loopback: class entry whose device is a platform dir with no driver link.
        let plat = f.root.join("devices/platform/v4l2loopback-000");
        let v = plat.join("video4linux/video9");
        fs::create_dir_all(&v).unwrap();
        fs::write(v.join("name"), "Dummy\n").unwrap();
        fs::write(v.join("index"), "0\n").unwrap();
        symlink(&plat, v.join("device")).unwrap();
        symlink(&v, f.root.join("class/video4linux/video9")).unwrap();
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
        // With the real camera present as well, the loopback is simply skipped.
        f.zenbook();
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.ir_cap.dev, Path::new("/dev/video2"));
    }

    #[test]
    fn second_matching_device_is_ignored_first_wins() {
        let f = Fake::new("two");
        let first = f.zenbook();
        let dev2 = f.usb_device("3-4", "3277", "0055", "fixed", "0003");
        let i2 = f.iface(&dev2, "02", "uvcvideo");
        f.video(&i2, 12, 0, "Second IR");
        let cam = discover_in(&f.root).unwrap();
        // Numeric node order: video0..3 (3-9) come before video12 (3-4);
        // the USB device owning the first node seen wins, the other is
        // dropped.
        assert_eq!(cam.usb_sysfs, first);
        assert_eq!(cam.ir_cap.dev, Path::new("/dev/video2"));
        assert_eq!(cam.bcd_device, "0002");
    }

    #[test]
    fn node_order_is_numeric_not_lexical() {
        // Device A owns video10/11 (IR cap + meta), device B owns video2/3.
        // Lexically "video10" < "video2" and A would win; numerically B does.
        let f = Fake::new("numeric");
        let a = f.usb_device("3-4", "3277", "0055", "fixed", "0003");
        let ia = f.iface(&a, "02", "uvcvideo");
        f.video(&ia, 10, 0, "A IR");
        f.video(&ia, 11, 1, "A IR meta");
        let b = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let ib = f.iface(&b, "02", "uvcvideo");
        f.video(&ib, 2, 0, "B IR");
        f.video(&ib, 3, 1, "B IR meta");
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.usb_sysfs, b);
        assert_eq!(cam.ir_cap.dev, Path::new("/dev/video2"));
        assert_eq!(cam.ir_meta.dev, Path::new("/dev/video3"));
        assert!(node_sort_key(Path::new("/x/video2")) < node_sort_key(Path::new("/x/video12")));
        assert!(node_sort_key(Path::new("/x/video12")) < node_sort_key(Path::new("/x/other")));
    }

    #[test]
    fn missing_ir_node_is_not_found_and_empty_root_is_not_found() {
        let f = Fake::new("rgbonly");
        let dev = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let i0 = f.iface(&dev, "00", "uvcvideo");
        f.video(&i0, 0, 0, "RGB only");
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
        let empty = Fake::new("empty");
        assert!(matches!(
            discover_in(&empty.root),
            Err(Error::NotFound { .. })
        ));
        assert!(matches!(
            discover_in(Path::new("/nonexistent/root")),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn read_attr_strips_trailing_whitespace_and_tolerates_missing() {
        let f = Fake::new("attr");
        let p = f.root.join("attr");
        fs::write(&p, "fixed \n").unwrap();
        assert_eq!(read_attr(&p), "fixed");
        assert_eq!(read_attr(&f.root.join("missing")), "");
    }

    fn profile() -> HwProfile {
        HwProfile::builtin().unwrap()
    }

    #[test]
    fn pin_requires_the_pinned_path_and_the_meta_sibling() {
        let f = Fake::new("pin");
        let dev = f.zenbook();
        let all = discover_all_in(&f.root, &profile()).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].other_devices.is_empty());
        let cam = pin(all.clone(), Some(&dev), &f.root, &profile()).unwrap();
        assert_eq!(cam.usb_sysfs, dev);
        let cam = pin(all.clone(), None, &f.root, &profile()).unwrap();
        assert_eq!(cam.ir_meta.dev, Path::new("/dev/video3"));
        // Pinned elsewhere: mismatch listing what is there.
        let other = f.root.join("devices/pci0000:00/0000:00:14.0/usb3/3-4");
        let e = pin(all.clone(), Some(&other), &f.root, &profile()).unwrap_err();
        assert!(
            matches!(e, Error::Mismatch { ref pinned, ref found } if pinned == &other.display().to_string() && found.len() == 1),
            "{e}"
        );
        assert_eq!(e.reason(), "camera_mismatch");
        // Nothing at all: camera_missing whichever way it is asked.
        let e = pin(Vec::new(), Some(&dev), &f.root, &profile()).unwrap_err();
        assert!(matches!(e, Error::NotFound { .. }));
        assert_eq!(e.reason(), "camera_missing");
        assert!(matches!(
            pin(Vec::new(), None, &f.root, &profile()),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn pin_without_meta_sibling_is_refused() {
        let f = Fake::new("nometa");
        let dev = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let i2 = f.iface(&dev, "02", "uvcvideo");
        f.video(&i2, 2, 0, "IR only");
        let all = discover_all_in(&f.root, &profile()).unwrap();
        assert_eq!(all.len(), 1);
        let e = pin(all, Some(&dev), &f.root, &profile()).unwrap_err();
        assert!(matches!(e, Error::NoMetaNode { .. }), "{e}");
        assert_eq!(e.reason(), "camera_mismatch");
    }

    #[test]
    fn two_candidates_are_ambiguous_at_enrolment_but_pinnable() {
        let f = Fake::new("ambiguous");
        let first = f.zenbook();
        let dev2 = f.usb_device("3-4", "3277", "0055", "fixed", "0003");
        let i2 = f.iface(&dev2, "02", "uvcvideo");
        f.video(&i2, 12, 0, "Second IR");
        f.video(&i2, 13, 1, "Second IR meta");
        let all = discover_all_in(&f.root, &profile()).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].usb_sysfs, first);
        assert_eq!(all[0].other_devices, vec![dev2.clone()]);
        assert_eq!(all[1].other_devices, vec![first.clone()]);
        let e = pin(all.clone(), None, &f.root, &profile()).unwrap_err();
        assert!(
            matches!(e, Error::Ambiguous { ref devices } if devices.len() == 2),
            "{e}"
        );
        assert_eq!(e.reason(), "camera_ambiguous");
        // Either one can be pinned explicitly.
        assert_eq!(
            pin(all.clone(), Some(&dev2), &f.root, &profile())
                .unwrap()
                .bcd_device,
            "0003"
        );
        assert_eq!(
            pin(all, Some(&first), &f.root, &profile())
                .unwrap()
                .bcd_device,
            "0002"
        );
        // discover_in keeps the compatibility behaviour: first wins, the
        // other is listed.
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.usb_sysfs, first);
        assert_eq!(cam.other_devices, vec![dev2]);
    }

    #[test]
    fn loopback_is_refused_at_discovery_and_listed() {
        let f = Fake::new("loopback-listed");
        // Real v4l2loopback: the class entry is itself under devices/virtual
        // (no `device` link at all) and reports 'Dummy video device (0x0000)'.
        let v = f.root.join("devices/virtual/video4linux/video10");
        fs::create_dir_all(&v).unwrap();
        fs::write(v.join("name"), "Dummy video device (0x0000)\n").unwrap();
        fs::write(v.join("index"), "0\n").unwrap();
        symlink(&v, f.root.join("class/video4linux/video10")).unwrap();
        // A platform-style one with a `device` link but no driver.
        let plat = f.root.join("devices/platform/v4l2loopback-000");
        let w = plat.join("video4linux/video11");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join("name"), "v4l2loopback\n").unwrap();
        symlink(&plat, w.join("device")).unwrap();
        symlink(&w, f.root.join("class/video4linux/video11")).unwrap();
        assert!(matches!(discover_in(&f.root), Err(Error::NotFound { .. })));
        f.zenbook();
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.ir_cap.dev, Path::new("/dev/video2"));
        let names: Vec<String> = cam
            .refused
            .iter()
            .map(|(p, r)| format!("{} {r}", p.file_name().unwrap().to_string_lossy()))
            .collect();
        assert_eq!(
            names,
            vec![
                "video10 v4l2loopback ('Dummy video device (0x0000)')".to_string(),
                "video11 v4l2loopback ('v4l2loopback')".to_string(),
            ]
        );
        // A USB node with another driver is skipped silently (not virtual).
        let f = Fake::new("otherdrv");
        let dev = f.usb_device("3-9", "3277", "0055", "fixed", "0002");
        let i3 = f.iface(&dev, "03", "snd-usb-audio");
        f.video(&i3, 5, 0, "Audio?");
        f.zenbook_ir_only(&dev);
        let cam = discover_in(&f.root).unwrap();
        assert!(cam.refused.is_empty());
        // A virtual node without a recognisable name is listed as virtual.
        let f = Fake::new("virt");
        let v = f.root.join("devices/virtual/video4linux/video20");
        fs::create_dir_all(&v).unwrap();
        fs::write(v.join("name"), "vivid-000-vid-cap\n").unwrap();
        symlink(&v, f.root.join("class/video4linux/video20")).unwrap();
        f.zenbook();
        let cam = discover_in(&f.root).unwrap();
        assert_eq!(cam.refused.len(), 1);
        assert!(cam.refused[0].1.starts_with("virtual device"));
    }

    #[test]
    fn profile_roles_drive_the_mapping() {
        // A profile that puts the IR pair on interface 4 finds nothing on
        // the Zenbook tree, and vice versa.
        let f = Fake::new("roles");
        f.zenbook();
        let text = crate::hw::BUILTIN_3277_0055
            .replace("ir   = { interface = 2", "ir   = { interface = 4")
            .replace("meta = { interface = 2", "meta = { interface = 4");
        let p = HwProfile::parse(&text).unwrap();
        assert!(discover_all_in(&f.root, &p).unwrap().is_empty());
        let text = crate::hw::BUILTIN_3277_0055.replace("product = \"0055\"", "product = \"0059\"");
        let p = HwProfile::parse(&text).unwrap();
        assert!(discover_all_in(&f.root, &p).unwrap().is_empty());
        assert!(matches!(
            discover_all_in(Path::new("/nonexistent"), &profile()),
            Ok(ref v) if v.is_empty()
        ));
    }
}
