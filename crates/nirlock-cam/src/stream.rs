//! One V4L2 node streamed with MMAP buffers (port of `fu::Stream`,
//! `phase0/v4l2cap.cpp`). RAII: dropping a `Stream` always `STREAMOFF`s,
//! unmaps and closes.
//!
//! `open` refuses any driver other than `uvcvideo` — explicitly including
//! `v4l2 loopback` (`bus_info` `platform:v4l2loopback`) — and nodes lacking
//! the expected capture capability. `open_pinned` additionally requires the
//! opened descriptor to be the very node found at discovery: `bus_info`
//! must be the one derived from the pinned USB sysfs path and `fstat`'s
//! device number must equal the class node's `dev` attribute (a
//! re-enumeration or renumbering between discovery and `open(2)` is
//! `Error::Mismatch`). `set_pix_format` / `set_meta_format` insist on the
//! exact format they asked for (a silent change by the driver is
//! `Error::Format`, `unavailable camera_format`).
//!
//! Buffer access is stateful: `buffer` only hands out the bytes of an index
//! that is currently dequeued (between `dequeue` and `requeue`), so a slice
//! can never alias a buffer the driver is filling.

use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use crate::discover::Node;
use crate::sys::{Sys, errno};
use crate::v4l2::{
    Request, V4L2_BUF_TYPE_META_CAPTURE, V4L2_BUF_TYPE_VIDEO_CAPTURE, V4L2_CAP_META_CAPTURE,
    V4L2_CAP_STREAMING, V4L2_CAP_VIDEO_CAPTURE, V4L2_FIELD_ANY, V4L2_MEMORY_MMAP, fourcc_str,
    v4l2_buffer, v4l2_buffer_m, v4l2_capability, v4l2_format, v4l2_requestbuffers,
};
use crate::{Error, Result};

/// The driver name every node must report on `VIDIOC_QUERYCAP`.
pub const DRIVER_UVCVIDEO: &str = "uvcvideo";
/// The `bus_info` prefix of v4l2loopback nodes (refused even if the driver
/// string were ever spoofed to match).
pub const BUS_INFO_V4L2LOOPBACK: &str = "platform:v4l2loopback";
/// The driver name v4l2loopback reports.
pub const DRIVER_V4L2LOOPBACK: &str = "v4l2 loopback";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Meta,
}

impl Kind {
    pub fn buf_type(self) -> u32 {
        match self {
            Kind::Video => V4L2_BUF_TYPE_VIDEO_CAPTURE,
            Kind::Meta => V4L2_BUF_TYPE_META_CAPTURE,
        }
    }
}

/// What a pinned node must look like once opened (DESIGN §2.3 step 1):
/// the `bus_info` uvcvideo derives from the USB device and the character
/// device number of the class node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// `usb-<controller PCI id>-<port path>`, e.g. `usb-0000:00:14.0-9`.
    pub bus_info: String,
    /// `(major, minor)` from the sysfs `dev` attribute.
    pub rdev: (u32, u32),
}

impl Identity {
    /// The identity of `node` on the USB device at `usb_sysfs`; `None`
    /// when either cannot be derived (a `Node` without `rdev`, an
    /// unexpected sysfs path), in which case the open is refused.
    pub fn of(usb_sysfs: &Path, node: &Node) -> Option<Self> {
        Some(Self {
            bus_info: crate::discover::usb_bus_info(usb_sysfs)?,
            rdev: node.rdev?,
        })
    }
}

/// A dequeued buffer's bookkeeping. The bytes live in the stream's mapping
/// and are read with [`Stream::buffer`] until [`Stream::requeue`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawFrame {
    /// Buffer index, for `requeue`.
    pub index: u32,
    pub bytesused: u32,
    pub sequence: u32,
    /// `V4L2_BUF_FLAG_*`.
    pub flags: u32,
    /// v4l2 buffer timestamp (seconds, monotonic).
    pub timestamp: f64,
}

struct Map {
    ptr: *mut u8,
    len: usize,
}

pub struct Stream<S: Sys> {
    sys: S,
    fd: RawFd,
    buf_type: u32,
    streaming: bool,
    dev: PathBuf,
    /// `bus_info` from `QUERYCAP`.
    bus_info: String,
    maps: Vec<Map>,
    /// `dequeued[i]`: buffer `i` is owned by us (between `DQBUF` and
    /// `QBUF`); only then may its bytes be read.
    dequeued: Vec<bool>,
}

impl<S: Sys> Stream<S> {
    /// `open(2)` + `VIDIOC_QUERYCAP` + driver/capability checks.
    pub fn open(mut sys: S, dev: &Path, kind: Kind) -> Result<Self> {
        let fd = sys.open(dev).map_err(|e| Error::from_os("open", dev, e))?;
        let mut s = Stream {
            sys,
            fd,
            buf_type: kind.buf_type(),
            streaming: false,
            dev: dev.to_path_buf(),
            bus_info: String::new(),
            maps: Vec::new(),
            dequeued: Vec::new(),
        };
        let mut cap = v4l2_capability::default();
        s.call(Request::QueryCap(&mut cap))?;
        let driver = cap.driver_str();
        let bus_info = cap.bus_info_str();
        if driver != DRIVER_UVCVIDEO || bus_info.starts_with(BUS_INFO_V4L2LOOPBACK) {
            // `s` is dropped on return: closed.
            return Err(Error::Refused {
                dev: s.dev.display().to_string(),
                driver,
                bus_info,
            });
        }
        let need = match kind {
            Kind::Video => V4L2_CAP_VIDEO_CAPTURE,
            Kind::Meta => V4L2_CAP_META_CAPTURE,
        };
        if cap.device_caps & need == 0 || cap.device_caps & V4L2_CAP_STREAMING == 0 {
            return Err(Error::Capability {
                dev: s.dev.display().to_string(),
                device_caps: cap.device_caps,
            });
        }
        s.bus_info = bus_info;
        Ok(s)
    }

    /// [`Stream::open`] plus the pin: the node must report the `bus_info`
    /// of the pinned USB device and its descriptor must refer to the
    /// character device the class node advertised at discovery.
    pub fn open_pinned(sys: S, dev: &Path, kind: Kind, expect: &Identity) -> Result<Self> {
        let mut s = Self::open(sys, dev, kind)?;
        if s.bus_info != expect.bus_info {
            return Err(Error::Mismatch {
                pinned: expect.bus_info.clone(),
                found: vec![format!("{} on {}", s.dev.display(), s.bus_info)],
            });
        }
        let rdev = s
            .sys
            .fstat_rdev(s.fd)
            .map_err(|e| Error::from_os("fstat", &s.dev, e))?;
        if rdev != expect.rdev {
            return Err(Error::Mismatch {
                pinned: format!("{} = {}:{}", s.dev.display(), expect.rdev.0, expect.rdev.1),
                found: vec![format!("{}:{}", rdev.0, rdev.1)],
            });
        }
        Ok(s)
    }

    fn call(&mut self, req: Request<'_>) -> Result<()> {
        let what = req.name();
        self.sys
            .ioctl(self.fd, req)
            .map_err(|e| Error::from_os(what, &self.dev, e))
    }

    /// `S_FMT` for a video node; the negotiated format must be exactly
    /// what was asked (we never want a silent format change).
    pub fn set_pix_format(&mut self, w: u32, h: u32, fourcc: u32) -> Result<()> {
        let mut f = v4l2_format::new(self.buf_type);
        self.call(Request::GFmt(&mut f))?;
        {
            let p = f.pix_mut();
            p.width = w;
            p.height = h;
            p.pixelformat = fourcc;
            p.field = V4L2_FIELD_ANY;
        }
        self.call(Request::SFmt(&mut f))?;
        let p = *f.pix();
        if p.width != w || p.height != h || p.pixelformat != fourcc {
            return Err(Error::Format {
                dev: self.dev.display().to_string(),
                wanted: format!("{w}x{h} {}", fourcc_str(fourcc)),
                got: format!("{}x{} {}", p.width, p.height, fourcc_str(p.pixelformat)),
            });
        }
        Ok(())
    }

    /// `S_FMT` for a metadata node (`UVCM`); exact read-back required.
    pub fn set_meta_format(&mut self, fourcc: u32) -> Result<()> {
        let mut f = v4l2_format::new(self.buf_type);
        self.call(Request::GFmt(&mut f))?;
        f.meta_mut().dataformat = fourcc;
        self.call(Request::SFmt(&mut f))?;
        let got = f.meta().dataformat;
        if got != fourcc {
            return Err(Error::Format {
                dev: self.dev.display().to_string(),
                wanted: format!("meta {}", fourcc_str(fourcc)),
                got: format!("meta {} (UVCM needs kernel >= 6.17)", fourcc_str(got)),
            });
        }
        Ok(())
    }

    /// Current format via `VIDIOC_G_FMT` (read-only), for logs.
    pub fn format_description(&mut self) -> String {
        let mut f = v4l2_format::new(self.buf_type);
        if let Err(e) = self.call(Request::GFmt(&mut f)) {
            return format!("G_FMT failed: {e}");
        }
        if self.buf_type == V4L2_BUF_TYPE_META_CAPTURE {
            let m = f.meta();
            format!(
                "meta '{}' buffersize={}",
                fourcc_str(m.dataformat),
                m.buffersize
            )
        } else {
            let p = f.pix();
            format!(
                "{}x{} '{}' sizeimage={}",
                p.width,
                p.height,
                fourcc_str(p.pixelformat),
                p.sizeimage
            )
        }
    }

    /// `REQBUFS` + `QUERYBUF`/`mmap`/`QBUF` for every buffer + `STREAMON`.
    pub fn start(&mut self, nbuf: u32) -> Result<()> {
        let mut req = v4l2_requestbuffers {
            count: nbuf,
            type_: self.buf_type,
            memory: V4L2_MEMORY_MMAP,
            ..v4l2_requestbuffers::default()
        };
        self.call(Request::ReqBufs(&mut req))?;
        self.dequeued = vec![false; req.count as usize];
        for i in 0..req.count {
            let mut b = v4l2_buffer {
                index: i,
                type_: self.buf_type,
                memory: V4L2_MEMORY_MMAP,
                ..v4l2_buffer::default()
            };
            self.call(Request::QueryBuf(&mut b))?;
            let len = b.length as usize;
            let ptr = self
                .sys
                .mmap(self.fd, len, b.m.offset())
                .map_err(|e| Error::from_os("mmap", &self.dev, e))?;
            self.maps.push(Map { ptr, len });
            self.call(Request::QBuf(&mut b))?;
        }
        let mut t = self.buf_type;
        self.call(Request::StreamOn(&mut t))?;
        self.streaming = true;
        Ok(())
    }

    /// Non-blocking `DQBUF`; `Ok(None)` when nothing is ready.
    pub fn dequeue(&mut self) -> Result<Option<RawFrame>> {
        let mut b = v4l2_buffer {
            type_: self.buf_type,
            memory: V4L2_MEMORY_MMAP,
            m: v4l2_buffer_m::mmap(0),
            ..v4l2_buffer::default()
        };
        if let Err(e) = self.sys.ioctl(self.fd, Request::DQBuf(&mut b)) {
            if e.raw_os_error() == Some(errno::EAGAIN) {
                return Ok(None);
            }
            return Err(Error::from_os("VIDIOC_DQBUF", &self.dev, e));
        }
        let idx = b.index as usize;
        if idx >= self.maps.len() {
            return Err(Error::Sys {
                what: "VIDIOC_DQBUF",
                dev: self.dev.display().to_string(),
                source: std::io::Error::other(format!(
                    "buffer index {} out of range ({} mapped)",
                    b.index,
                    self.maps.len()
                )),
            });
        }
        if std::mem::replace(&mut self.dequeued[idx], true) {
            return Err(Error::Sys {
                what: "VIDIOC_DQBUF",
                dev: self.dev.display().to_string(),
                source: std::io::Error::other(format!(
                    "buffer index {} dequeued twice without a QBUF",
                    b.index
                )),
            });
        }
        Ok(Some(RawFrame {
            index: b.index,
            bytesused: b.bytesused,
            sequence: b.sequence,
            flags: b.flags,
            timestamp: b.timestamp.tv_sec as f64 + b.timestamp.tv_usec as f64 * 1e-6,
        }))
    }

    /// The bytes of a **currently dequeued** buffer (`bytesused` long,
    /// clipped to the mapping). Valid until `requeue`/`stop`; callers copy
    /// what they keep. An index that is not dequeued (never was, or was
    /// already requeued: the driver may be writing it) is an error, never
    /// a slice.
    #[allow(unsafe_code)]
    pub fn buffer(&self, f: &RawFrame) -> Result<&[u8]> {
        let idx = f.index as usize;
        let (Some(m), Some(true)) = (self.maps.get(idx), self.dequeued.get(idx).copied()) else {
            return Err(Error::Sys {
                what: "buffer",
                dev: self.dev.display().to_string(),
                source: std::io::Error::other(format!(
                    "buffer index {} is not dequeued ({} mapped)",
                    f.index,
                    self.maps.len()
                )),
            });
        };
        let n = (f.bytesused as usize).min(m.len);
        // SAFETY: `m.ptr`/`m.len` is a live mapping owned by `self` (released
        // only in `stop`, which needs `&mut self`); `dequeued[idx]` is true,
        // so the driver completed this buffer (it filled `bytesused` bytes
        // before `DQBUF` returned) and will not touch it until it is queued
        // again, which only `requeue` (`&mut self`, clears the flag) does.
        // The slice therefore borrows initialised memory that nothing
        // mutates while `&self` is held.
        Ok(unsafe { std::slice::from_raw_parts(m.ptr, n) })
    }

    /// `QBUF` the buffer back. Only a dequeued index can be requeued.
    pub fn requeue(&mut self, f: &RawFrame) -> Result<()> {
        let idx = f.index as usize;
        if self.dequeued.get(idx) != Some(&true) {
            return Err(Error::Sys {
                what: "VIDIOC_QBUF",
                dev: self.dev.display().to_string(),
                source: std::io::Error::other(format!("buffer index {} is not dequeued", f.index)),
            });
        }
        let mut b = v4l2_buffer {
            index: f.index,
            type_: self.buf_type,
            memory: V4L2_MEMORY_MMAP,
            ..v4l2_buffer::default()
        };
        self.call(Request::QBuf(&mut b))?;
        self.dequeued[idx] = false;
        Ok(())
    }

    /// `STREAMOFF` (best effort), unmap, close. Idempotent.
    pub fn stop(&mut self) {
        if self.fd < 0 {
            return;
        }
        if self.streaming {
            let mut t = self.buf_type;
            let _ = self.sys.ioctl(self.fd, Request::StreamOff(&mut t));
            self.streaming = false;
        }
        self.dequeued.clear();
        for m in self.maps.drain(..) {
            self.sys.munmap(m.ptr, m.len);
        }
        self.sys.close(self.fd);
        self.fd = -1;
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    pub fn streaming(&self) -> bool {
        self.streaming
    }

    pub fn dev(&self) -> &Path {
        &self.dev
    }

    /// `bus_info` as reported by `QUERYCAP`.
    pub fn bus_info(&self) -> &str {
        &self.bus_info
    }

    pub fn buffers(&self) -> usize {
        self.maps.len()
    }
}

impl<S: Sys> std::fmt::Debug for Stream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("dev", &self.dev)
            .field("fd", &self.fd)
            .field("buf_type", &self.buf_type)
            .field("streaming", &self.streaming)
            .field("buffers", &self.maps.len())
            .finish()
    }
}

impl<S: Sys> Drop for Stream<S> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNode, FakeSys};
    use crate::v4l2::{
        V4L2_META_FMT_UVC, V4L2_META_FMT_UVC_MSXU_1_5, V4L2_PIX_FMT_GREY, V4L2_PIX_FMT_MJPEG,
    };

    fn video2() -> PathBuf {
        PathBuf::from("/dev/video2")
    }

    #[test]
    fn open_start_dequeue_stop_issue_exactly_the_allowed_sequence() {
        let sys = FakeSys::new();
        sys.add_node(&video2(), FakeNode::ir_video());
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        assert_eq!(s.dev(), video2());
        assert!(!s.streaming());
        s.set_pix_format(640, 360, V4L2_PIX_FMT_GREY).unwrap();
        assert_eq!(s.format_description(), "640x360 'GREY' sizeimage=230400");
        s.start(8).unwrap();
        assert!(s.streaming());
        assert_eq!(s.buffers(), 8);
        assert!(
            s.dequeue().unwrap().is_none(),
            "nothing scheduled -> EAGAIN"
        );
        sys.deliver(&video2(), 0.0, 1, 0, vec![7u8; 230_400]);
        let f = s.dequeue().unwrap().unwrap();
        assert_eq!((f.sequence, f.bytesused, f.flags), (1, 230_400, 0));
        assert_eq!(s.buffer(&f).unwrap().len(), 230_400);
        assert!(s.buffer(&f).unwrap().iter().all(|&b| b == 7));
        s.requeue(&f).unwrap();
        // Requeued: the bytes are the driver's again.
        assert!(matches!(
            s.buffer(&f),
            Err(Error::Sys { what: "buffer", .. })
        ));
        assert!(matches!(
            s.requeue(&f),
            Err(Error::Sys {
                what: "VIDIOC_QBUF",
                ..
            })
        ));
        let fd = s.fd();
        drop(s);
        let log = sys.log_for(&video2());
        let mut expect = vec![
            "open".to_string(),
            "VIDIOC_QUERYCAP".into(),
            "VIDIOC_G_FMT".into(),
            "VIDIOC_S_FMT".into(),
            "VIDIOC_G_FMT".into(),
            "VIDIOC_REQBUFS 8".into(),
        ];
        for i in 0..8 {
            expect.push(format!("VIDIOC_QUERYBUF {i}"));
            expect.push(format!("mmap {i}"));
            expect.push(format!("VIDIOC_QBUF {i}"));
        }
        expect.extend([
            "VIDIOC_STREAMON".to_string(),
            "VIDIOC_DQBUF".into(),
            "VIDIOC_DQBUF".into(),
            "VIDIOC_QBUF 0".into(),
            "VIDIOC_STREAMOFF".into(),
        ]);
        for i in 0..8 {
            expect.push(format!("munmap {i}"));
        }
        expect.push("close".into());
        assert_eq!(log, expect);
        assert!(!sys.is_open(fd));
        assert_eq!(sys.live_mappings(), 0);
    }

    #[test]
    fn refuses_v4l2loopback_by_driver_and_by_bus_info() {
        let sys = FakeSys::new();
        let mut n = FakeNode::ir_video();
        n.driver = DRIVER_V4L2LOOPBACK.into();
        n.bus_info = BUS_INFO_V4L2LOOPBACK.into();
        sys.add_node(&video2(), n);
        let e = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap_err();
        assert!(matches!(e, Error::Refused { ref driver, .. } if driver == DRIVER_V4L2LOOPBACK));
        assert_eq!(e.reason(), "camera_mismatch");
        assert!(e.to_string().contains("v4l2 loopback"), "{e}");
        // Driver string spoofed, bus_info still tells.
        let mut n = FakeNode::ir_video();
        n.bus_info = "platform:v4l2loopback-000".into();
        sys.add_node(&video2(), n);
        assert!(matches!(
            Stream::open(sys.clone(), &video2(), Kind::Video),
            Err(Error::Refused { .. })
        ));
        // Any other driver is refused too, and the fd is closed on refusal.
        let mut n = FakeNode::ir_video();
        n.driver = "vivid".into();
        sys.add_node(&video2(), n);
        assert!(matches!(
            Stream::open(sys.clone(), &video2(), Kind::Video),
            Err(Error::Refused { .. })
        ));
        assert_eq!(sys.open_fds(), 0);
        assert_eq!(sys.log_for(&video2()).last().unwrap(), "close");
    }

    #[test]
    fn refuses_nodes_without_the_capability_and_reports_busy() {
        let sys = FakeSys::new();
        let mut n = FakeNode::ir_video();
        n.device_caps = V4L2_CAP_VIDEO_CAPTURE; // no STREAMING
        sys.add_node(&video2(), n);
        assert!(matches!(
            Stream::open(sys.clone(), &video2(), Kind::Video),
            Err(Error::Capability { .. })
        ));
        // A video node opened as a meta node lacks META_CAPTURE.
        sys.add_node(&video2(), FakeNode::ir_video());
        assert!(matches!(
            Stream::open(sys.clone(), &video2(), Kind::Meta),
            Err(Error::Capability { .. })
        ));
        let mut n = FakeNode::ir_video();
        n.open_errors.push_back(errno::EBUSY);
        sys.add_node(&video2(), n);
        let e = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap_err();
        assert!(matches!(e, Error::Busy { .. }));
        assert_eq!(e.reason(), "camera_busy");
        let mut n = FakeNode::ir_video();
        n.open_errors.push_back(errno::ENOENT);
        sys.add_node(&video2(), n);
        let e = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap_err();
        assert!(matches!(e, Error::Sys { what: "open", .. }));
        assert_eq!(e.reason(), "camera_error");
    }

    #[test]
    fn silent_format_change_is_camera_format() {
        let sys = FakeSys::new();
        let mut n = FakeNode::ir_video();
        n.sfmt_pix_override = Some((640, 480, V4L2_PIX_FMT_GREY));
        sys.add_node(&video2(), n);
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        let e = s.set_pix_format(640, 360, V4L2_PIX_FMT_GREY).unwrap_err();
        assert!(
            matches!(e, Error::Format { ref wanted, ref got, .. } if wanted == "640x360 GREY" && got == "640x480 GREY")
        );
        assert_eq!(e.reason(), "camera_format");
        let mut n = FakeNode::ir_video();
        n.sfmt_pix_override = Some((640, 360, V4L2_PIX_FMT_MJPEG));
        sys.add_node(&video2(), n);
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        assert!(matches!(
            s.set_pix_format(640, 360, V4L2_PIX_FMT_GREY),
            Err(Error::Format { .. })
        ));
        // Meta: UVCH instead of UVCM (kernel < 6.17).
        let video3 = PathBuf::from("/dev/video3");
        let mut n = FakeNode::ir_meta();
        n.sfmt_meta_override = Some(V4L2_META_FMT_UVC);
        sys.add_node(&video3, n);
        let mut s = Stream::open(sys.clone(), &video3, Kind::Meta).unwrap();
        let e = s.set_meta_format(V4L2_META_FMT_UVC_MSXU_1_5).unwrap_err();
        assert!(matches!(e, Error::Format { ref got, .. } if got.contains("UVCH")));
        // Exact acceptance works and reads back.
        sys.add_node(&video3, FakeNode::ir_meta());
        let mut s = Stream::open(sys.clone(), &video3, Kind::Meta).unwrap();
        s.set_meta_format(V4L2_META_FMT_UVC_MSXU_1_5).unwrap();
        assert_eq!(s.format_description(), "meta 'UVCM' buffersize=1024");
    }

    #[test]
    fn ioctl_failures_and_short_reqbufs_surface_and_teardown_is_complete() {
        let sys = FakeSys::new();
        let mut n = FakeNode::ir_video();
        n.reqbufs_count_override = Some(3);
        sys.add_node(&video2(), n);
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        s.start(8).unwrap();
        assert_eq!(s.buffers(), 3, "the driver's count wins, as in the C++");
        s.stop();
        s.stop(); // idempotent
        assert_eq!(sys.live_mappings(), 0);
        assert_eq!(s.fd(), -1);

        let sys = FakeSys::new();
        let mut n = FakeNode::ir_video();
        n.fail_streamon = Some(errno::EIO);
        sys.add_node(&video2(), n);
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        let e = s.start(4).unwrap_err();
        assert!(
            matches!(
                e,
                Error::Sys {
                    what: "VIDIOC_STREAMON",
                    ..
                }
            ),
            "{e}"
        );
        assert!(!s.streaming());
        drop(s);
        // No STREAMOFF was sent (never streaming) but mappings were released.
        let log = sys.log_for(&video2());
        assert!(!log.iter().any(|l| l == "VIDIOC_STREAMOFF"));
        assert_eq!(log.iter().filter(|l| l.starts_with("munmap")).count(), 4);
        assert_eq!(sys.live_mappings(), 0);

        let mut n = FakeNode::ir_video();
        n.fail_mmap = true;
        sys.add_node(&video2(), n);
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        assert!(matches!(s.start(4), Err(Error::Sys { what: "mmap", .. })));
    }

    #[test]
    fn buffer_is_clipped_to_the_mapping_and_only_dequeued_indices_are_readable() {
        let sys = FakeSys::new();
        sys.add_node(&video2(), FakeNode::ir_video());
        let mut s = Stream::open(sys.clone(), &video2(), Kind::Video).unwrap();
        s.start(2).unwrap();
        // Queued (driver-owned) buffers are never readable, known index or not.
        let queued = RawFrame {
            index: 0,
            bytesused: u32::MAX,
            ..RawFrame::default()
        };
        assert!(s.buffer(&queued).is_err());
        let unknown = RawFrame {
            index: 9,
            ..RawFrame::default()
        };
        assert!(s.buffer(&unknown).is_err());
        assert!(s.requeue(&unknown).is_err());
        // Dequeued: readable, and a bogus bytesused is clipped to the mapping.
        sys.deliver(&video2(), 0.0, 1, 0, vec![1u8; 10]);
        let f = s.dequeue().unwrap().unwrap();
        assert_eq!(f.index, 0);
        let lying = RawFrame {
            bytesused: u32::MAX,
            ..f
        };
        assert_eq!(s.buffer(&lying).unwrap().len(), 230_400);
        assert_eq!(s.buffer(&f).unwrap(), &[1u8; 10][..]);
        s.requeue(&f).unwrap();
        assert!(s.buffer(&f).is_err());
    }

    #[test]
    fn open_pinned_checks_bus_info_and_device_number() {
        let sys = FakeSys::new();
        sys.add_node(&video2(), FakeNode::ir_video());
        let ok = Identity {
            bus_info: "usb-0000:00:14.0-9".into(),
            rdev: (81, 2),
        };
        let s = Stream::open_pinned(sys.clone(), &video2(), Kind::Video, &ok).unwrap();
        assert_eq!(s.bus_info(), "usb-0000:00:14.0-9");
        drop(s);
        // Another USB device with the same driver: refused, fd closed.
        let other = Identity {
            bus_info: "usb-0000:00:14.0-4".into(),
            ..ok.clone()
        };
        let e = Stream::open_pinned(sys.clone(), &video2(), Kind::Video, &other).unwrap_err();
        assert!(matches!(e, Error::Mismatch { .. }), "{e}");
        assert_eq!(e.reason(), "camera_mismatch");
        assert!(e.to_string().contains("usb-0000:00:14.0-4"));
        assert_eq!(sys.open_fds(), 0);
        // Same device, renumbered node (the class node said 81:2, the fd is 81:12).
        let mut n = FakeNode::ir_video();
        n.rdev = (81, 12);
        sys.add_node(&video2(), n);
        let e = Stream::open_pinned(sys.clone(), &video2(), Kind::Video, &ok).unwrap_err();
        assert!(
            matches!(e, Error::Mismatch { ref found, .. } if found == &["81:12".to_string()]),
            "{e}"
        );
        assert_eq!(sys.open_fds(), 0);
        // Identity derivation from the discovery result.
        let node = Node {
            rdev: Some((81, 3)),
            ..Node::default()
        };
        assert_eq!(
            Identity::of(
                Path::new("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9"),
                &node
            ),
            Some(Identity {
                bus_info: "usb-0000:00:14.0-9".into(),
                rdev: (81, 3)
            })
        );
        assert_eq!(
            Identity::of(
                Path::new("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9"),
                &Node::default()
            ),
            None
        );
    }
}
