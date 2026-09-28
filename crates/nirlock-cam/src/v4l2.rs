//! V4L2 ABI definitions used by the capture path, mirroring
//! `<linux/videodev2.h>` on x86_64, plus the **only** ioctl entry point of
//! the crate: [`ioctl`], which takes a closed enum of the nine requests the
//! daemon is allowed to issue (DESIGN §2.3 step 3, ADR-0005). There is no
//! other `ioctl` call in `nirlock-cam` (the crate denies `unsafe_code`
//! except on the items listed by the source guard in `lib.rs`, which also
//! counts the call sites) and
//! no way to express a control query, a parameter write or an enumeration
//! through this function.
//!
//! Every struct is `#[repr(C)]` and its size is asserted against the
//! kernel's (`v4l2_buffer` 88, `v4l2_format` 208, `v4l2_requestbuffers` 20,
//! `v4l2_capability` 104, DESIGN §9.2) so a wrong field or padding cannot
//! compile; the ioctl numbers are derived from those sizes with the
//! `_IOC` encoding and cross-checked against the values the C headers give
//! (`fuprobe`'s build).

#![allow(non_camel_case_types)]

use std::ffi::{c_long, c_void};
use std::os::fd::RawFd;

/// `struct timeval` (glibc, x86_64: two 64-bit fields).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct timeval {
    pub tv_sec: c_long,
    pub tv_usec: c_long,
}

/// `struct v4l2_timecode`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_timecode {
    pub type_: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

/// The `m` union of `v4l2_buffer` (`offset` / `userptr` / `planes` / `fd`).
///
/// The fields are private and every constructor writes the full 8 bytes
/// through `raw`, so reading `raw` is always initialised memory; that is
/// what makes the `Debug` impl and the accessors sound. Safe code cannot
/// build a partially initialised value (`{ fd: 0 }` would leave 4 padding
/// bytes undefined). The kernel writes the whole struct on `DQBUF`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_buffer_m {
    raw: u64,
    offset: u32,
    userptr: c_long,
    planes: *mut v4l2_plane,
    fd: i32,
}

impl v4l2_buffer_m {
    /// `V4L2_MEMORY_MMAP`: `offset` (the low 4 bytes on little-endian, the
    /// rest zero).
    pub const fn mmap(offset: u32) -> Self {
        Self { raw: offset as u64 }
    }

    /// `V4L2_MEMORY_USERPTR`.
    pub const fn userptr(ptr: c_long) -> Self {
        Self { raw: ptr as u64 }
    }

    /// `V4L2_MEMORY_DMABUF`: `fd` (sign bits are not extended past 4 bytes).
    pub const fn dmabuf(fd: i32) -> Self {
        Self {
            raw: fd as u32 as u64,
        }
    }

    /// Multi-planar: a pointer to the planes array.
    pub fn planes(ptr: *mut v4l2_plane) -> Self {
        Self {
            raw: ptr as usize as u64,
        }
    }

    /// All 8 bytes, regardless of which variant is meaningful.
    #[allow(unsafe_code)]
    pub fn raw(&self) -> u64 {
        // SAFETY: every constructor (`mmap`, `userptr`, `dmabuf`, `planes`,
        // `Default`) writes all 8 bytes through `raw`, the fields are
        // private so no other initialisation exists, and the kernel fills
        // the whole struct on ioctl return. Reading `raw` is therefore
        // always initialised memory; the value is a plain integer.
        unsafe { self.raw }
    }

    /// The `offset` variant (`V4L2_MEMORY_MMAP`).
    pub fn offset(&self) -> u32 {
        // Little-endian: the low 4 bytes are `offset`.
        self.raw() as u32
    }

    /// The `fd` variant (`V4L2_MEMORY_DMABUF`).
    pub fn fd(&self) -> i32 {
        self.raw() as u32 as i32
    }

    /// The `userptr` variant.
    pub fn userptr_value(&self) -> c_long {
        self.raw() as c_long
    }
}

impl Default for v4l2_buffer_m {
    fn default() -> Self {
        Self { raw: 0 }
    }
}

impl std::fmt::Debug for v4l2_buffer_m {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "m({:#x})", self.raw())
    }
}

/// The anonymous trailing union of `v4l2_buffer` (`request_fd` / `reserved`).
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_buffer_tail {
    pub request_fd: i32,
    pub reserved: u32,
}

impl Default for v4l2_buffer_tail {
    fn default() -> Self {
        Self { reserved: 0 }
    }
}

impl std::fmt::Debug for v4l2_buffer_tail {
    #[allow(unsafe_code)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: both variants are 4-byte plain integers occupying the same
        // storage; reading `reserved` is always valid initialised memory.
        let raw = unsafe { self.reserved };
        write!(f, "tail({raw:#x})")
    }
}

/// The `m` union of `v4l2_plane`. Same construction as [`v4l2_buffer_m`]:
/// private fields, every constructor writes all 8 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_plane_m {
    raw: u64,
    mem_offset: u32,
    userptr: c_long,
    fd: i32,
}

impl v4l2_plane_m {
    pub const fn mem_offset(off: u32) -> Self {
        Self { raw: off as u64 }
    }

    pub const fn userptr(ptr: c_long) -> Self {
        Self { raw: ptr as u64 }
    }

    pub const fn dmabuf(fd: i32) -> Self {
        Self {
            raw: fd as u32 as u64,
        }
    }

    #[allow(unsafe_code)]
    pub fn raw(&self) -> u64 {
        // SAFETY: as for `v4l2_buffer_m::raw`: private fields, every
        // constructor and `Default` write the full 8 bytes via `raw`, the
        // kernel fills whole planes; reading the integer is always
        // initialised memory.
        unsafe { self.raw }
    }
}

impl Default for v4l2_plane_m {
    fn default() -> Self {
        Self { raw: 0 }
    }
}

impl std::fmt::Debug for v4l2_plane_m {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "m({:#x})", self.raw())
    }
}

/// `struct v4l2_plane` (only referenced through a pointer here).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_plane {
    pub bytesused: u32,
    pub length: u32,
    pub m: v4l2_plane_m,
    pub data_offset: u32,
    pub reserved: [u32; 11],
}

/// `struct v4l2_buffer`, the `VIDIOC_QBUF` / `VIDIOC_DQBUF` argument.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: timeval,
    pub timecode: v4l2_timecode,
    pub sequence: u32,
    pub memory: u32,
    pub m: v4l2_buffer_m,
    pub length: u32,
    pub reserved2: u32,
    pub tail: v4l2_buffer_tail,
}

/// `V4L2_BUF_FLAG_ERROR`: the buffer's contents are not usable.
pub const V4L2_BUF_FLAG_ERROR: u32 = 0x0000_0040;
/// `V4L2_PIX_FMT_GREY` (`'GREY'`).
pub const V4L2_PIX_FMT_GREY: u32 = u32::from_le_bytes(*b"GREY");
/// `V4L2_META_FMT_UVC` (`'UVCH'`), the uvcvideo metadata format.
pub const V4L2_META_FMT_UVC: u32 = u32::from_le_bytes(*b"UVCH");

/// `V4L2_META_FMT_UVC_MSXU_1_5` (`'UVCM'`): the Microsoft UVC 1.5 extension
/// metadata format (kernel >= 6.17) that carries `FrameIllumination`.
pub const V4L2_META_FMT_UVC_MSXU_1_5: u32 = u32::from_le_bytes(*b"UVCM");
/// `V4L2_PIX_FMT_MJPEG` (`'MJPG'`), the RGB node's format (ADR-0006).
pub const V4L2_PIX_FMT_MJPEG: u32 = u32::from_le_bytes(*b"MJPG");

pub const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const V4L2_BUF_TYPE_META_CAPTURE: u32 = 13;
pub const V4L2_MEMORY_MMAP: u32 = 1;
pub const V4L2_FIELD_ANY: u32 = 0;
pub const V4L2_CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
pub const V4L2_CAP_META_CAPTURE: u32 = 0x0080_0000;
pub const V4L2_CAP_STREAMING: u32 = 0x0400_0000;

/// FourCC as text (`"GREY"`), for messages.
pub fn fourcc_str(f: u32) -> String {
    f.to_le_bytes()
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// `struct v4l2_capability` (`VIDIOC_QUERYCAP` output, 104 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

/// A NUL-terminated fixed array as `&str` (lossy, up to the first NUL).
pub fn cstr_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl v4l2_capability {
    pub fn driver_str(&self) -> String {
        cstr_field(&self.driver)
    }

    pub fn card_str(&self) -> String {
        cstr_field(&self.card)
    }

    pub fn bus_info_str(&self) -> String {
        cstr_field(&self.bus_info)
    }
}

/// `struct v4l2_pix_format` (48 bytes). The `ycbcr_enc`/`hsv_enc` union is
/// a plain `u32` here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_pix_format {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

/// `struct v4l2_meta_format` (20 bytes since 6.11; only the first two
/// fields matter for `UVCM`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_meta_format {
    pub dataformat: u32,
    pub buffersize: u32,
    pub width: u32,
    pub height: u32,
    pub bytesperline: u32,
}

/// The `fmt` union of `v4l2_format`: 200 bytes, 8-aligned (the kernel's
/// union holds a `v4l2_window` with a pointer). Private fields; every
/// constructor zeroes all 200 bytes through `raw`, so reading any variant
/// (all of them plain integers) is always initialised memory.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub union v4l2_format_fmt {
    raw: [u8; 200],
    pix: v4l2_pix_format,
    meta: v4l2_meta_format,
}

impl Default for v4l2_format_fmt {
    fn default() -> Self {
        Self { raw: [0; 200] }
    }
}

impl std::fmt::Debug for v4l2_format_fmt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fmt(..)")
    }
}

/// `struct v4l2_format` (`VIDIOC_G_FMT` / `VIDIOC_S_FMT`, 208 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_format {
    pub type_: u32,
    pub fmt: v4l2_format_fmt,
}

impl v4l2_format {
    /// Zeroed, with `type` set (what every `G_FMT` starts from).
    pub fn new(buf_type: u32) -> Self {
        Self {
            type_: buf_type,
            fmt: v4l2_format_fmt::default(),
        }
    }

    #[allow(unsafe_code)]
    pub fn pix(&self) -> &v4l2_pix_format {
        // SAFETY: the union is always fully initialised (constructed via
        // `raw` zeroes or filled by the kernel) and `v4l2_pix_format` is
        // plain integers with no invalid bit pattern.
        unsafe { &self.fmt.pix }
    }

    #[allow(unsafe_code)]
    pub fn pix_mut(&mut self) -> &mut v4l2_pix_format {
        // SAFETY: as in `pix`; writing integers through the variant keeps
        // the union fully initialised.
        unsafe { &mut self.fmt.pix }
    }

    #[allow(unsafe_code)]
    pub fn meta(&self) -> &v4l2_meta_format {
        // SAFETY: as in `pix`; `v4l2_meta_format` is plain integers.
        unsafe { &self.fmt.meta }
    }

    #[allow(unsafe_code)]
    pub fn meta_mut(&mut self) -> &mut v4l2_meta_format {
        // SAFETY: as in `pix_mut`.
        unsafe { &mut self.fmt.meta }
    }
}

/// `struct v4l2_requestbuffers` (`VIDIOC_REQBUFS`, 20 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

// ---------------------------------------------------------------- ioctls

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const V4L2_IOC_TYPE: u32 = b'V' as u32;

/// `_IOC(dir, 'V', nr, size)` from `<asm-generic/ioctl.h>`.
const fn ioc(dir: u32, nr: u32, size: usize) -> u64 {
    assert!(size < (1 << IOC_SIZEBITS));
    ((dir << IOC_DIRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
        | (V4L2_IOC_TYPE << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)) as u64
}
const fn ior(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ, nr, size)
}
const fn iow(nr: u32, size: usize) -> u64 {
    ioc(IOC_WRITE, nr, size)
}
const fn iowr(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ | IOC_WRITE, nr, size)
}

/// `VIDIOC_QUERYCAP = _IOR('V', 0, struct v4l2_capability)`.
pub const VIDIOC_QUERYCAP: u64 = ior(0, size_of::<v4l2_capability>());
/// `VIDIOC_G_FMT = _IOWR('V', 4, struct v4l2_format)`.
pub const VIDIOC_G_FMT: u64 = iowr(4, size_of::<v4l2_format>());
/// `VIDIOC_S_FMT = _IOWR('V', 5, struct v4l2_format)`.
pub const VIDIOC_S_FMT: u64 = iowr(5, size_of::<v4l2_format>());
/// `VIDIOC_REQBUFS = _IOWR('V', 8, struct v4l2_requestbuffers)`.
pub const VIDIOC_REQBUFS: u64 = iowr(8, size_of::<v4l2_requestbuffers>());
/// `VIDIOC_QUERYBUF = _IOWR('V', 9, struct v4l2_buffer)`.
pub const VIDIOC_QUERYBUF: u64 = iowr(9, size_of::<v4l2_buffer>());
/// `VIDIOC_QBUF = _IOWR('V', 15, struct v4l2_buffer)`.
pub const VIDIOC_QBUF: u64 = iowr(15, size_of::<v4l2_buffer>());
/// `VIDIOC_DQBUF = _IOWR('V', 17, struct v4l2_buffer)`.
pub const VIDIOC_DQBUF: u64 = iowr(17, size_of::<v4l2_buffer>());
/// `VIDIOC_STREAMON = _IOW('V', 18, int)`.
pub const VIDIOC_STREAMON: u64 = iow(18, size_of::<i32>());
/// `VIDIOC_STREAMOFF = _IOW('V', 19, int)`.
pub const VIDIOC_STREAMOFF: u64 = iow(19, size_of::<i32>());

/// The closed set of requests `nirlock-cam` can send to a video node. Each
/// variant carries the (correctly typed) argument, so the request number
/// and the argument can never disagree, and nothing outside this enum can
/// reach [`ioctl`].
#[derive(Debug)]
pub enum Request<'a> {
    QueryCap(&'a mut v4l2_capability),
    GFmt(&'a mut v4l2_format),
    SFmt(&'a mut v4l2_format),
    ReqBufs(&'a mut v4l2_requestbuffers),
    QueryBuf(&'a mut v4l2_buffer),
    QBuf(&'a mut v4l2_buffer),
    DQBuf(&'a mut v4l2_buffer),
    /// Argument: the buffer type.
    StreamOn(&'a mut u32),
    /// Argument: the buffer type.
    StreamOff(&'a mut u32),
}

impl Request<'_> {
    /// The `VIDIOC_*` number of this request.
    pub fn number(&self) -> u64 {
        match self {
            Request::QueryCap(_) => VIDIOC_QUERYCAP,
            Request::GFmt(_) => VIDIOC_G_FMT,
            Request::SFmt(_) => VIDIOC_S_FMT,
            Request::ReqBufs(_) => VIDIOC_REQBUFS,
            Request::QueryBuf(_) => VIDIOC_QUERYBUF,
            Request::QBuf(_) => VIDIOC_QBUF,
            Request::DQBuf(_) => VIDIOC_DQBUF,
            Request::StreamOn(_) => VIDIOC_STREAMON,
            Request::StreamOff(_) => VIDIOC_STREAMOFF,
        }
    }

    /// The request's name, for error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Request::QueryCap(_) => "VIDIOC_QUERYCAP",
            Request::GFmt(_) => "VIDIOC_G_FMT",
            Request::SFmt(_) => "VIDIOC_S_FMT",
            Request::ReqBufs(_) => "VIDIOC_REQBUFS",
            Request::QueryBuf(_) => "VIDIOC_QUERYBUF",
            Request::QBuf(_) => "VIDIOC_QBUF",
            Request::DQBuf(_) => "VIDIOC_DQBUF",
            Request::StreamOn(_) => "VIDIOC_STREAMON",
            Request::StreamOff(_) => "VIDIOC_STREAMOFF",
        }
    }

    fn arg(&mut self) -> *mut c_void {
        match self {
            Request::QueryCap(a) => (&raw mut **a).cast(),
            Request::GFmt(a) | Request::SFmt(a) => (&raw mut **a).cast(),
            Request::ReqBufs(a) => (&raw mut **a).cast(),
            Request::QueryBuf(a) | Request::QBuf(a) | Request::DQBuf(a) => (&raw mut **a).cast(),
            Request::StreamOn(a) | Request::StreamOff(a) => (&raw mut **a).cast(),
        }
    }
}

/// The one ioctl entry point of the crate. `EINTR` is retried (as
/// `xioctl` in `v4l2cap.cpp`; the daemon has no interrupt flag, a signal
/// that must stop it kills the process and the kernel then releases the
/// stream with the fds). Any other failure is returned as the OS error.
#[allow(unsafe_code)]
pub fn ioctl(fd: RawFd, mut req: Request<'_>) -> std::io::Result<()> {
    let number = req.number();
    let arg = req.arg();
    loop {
        // SAFETY: `fd` is a file descriptor this crate opened; `number` is
        // one of the nine `VIDIOC_*` constants above and `arg` points to
        // the live, exclusively borrowed, correctly sized and `repr(C)`
        // struct that the enum variant pairs with that number, so the
        // kernel reads/writes exactly `_IOC_SIZE(number)` bytes of valid
        // memory for the duration of the call.
        let r = unsafe { libc::ioctl(fd, number as libc::c_ulong, arg) };
        if r >= 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

// ABI size checks against <linux/videodev2.h> on x86_64 (DESIGN §9.2).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const _: () = {
    assert!(std::mem::size_of::<timeval>() == 16);
    assert!(std::mem::size_of::<v4l2_timecode>() == 16);
    assert!(std::mem::size_of::<v4l2_buffer_m>() == 8);
    assert!(std::mem::size_of::<v4l2_plane_m>() == 8);
    assert!(std::mem::size_of::<v4l2_plane>() == 64);
    assert!(std::mem::size_of::<v4l2_buffer>() == 88);
    assert!(std::mem::align_of::<v4l2_buffer>() == 8);
    assert!(std::mem::size_of::<v4l2_capability>() == 104);
    assert!(std::mem::size_of::<v4l2_pix_format>() == 48);
    assert!(std::mem::size_of::<v4l2_meta_format>() == 20);
    assert!(std::mem::size_of::<v4l2_format_fmt>() == 200);
    assert!(std::mem::size_of::<v4l2_format>() == 208);
    assert!(std::mem::align_of::<v4l2_format>() == 8);
    assert!(std::mem::size_of::<v4l2_requestbuffers>() == 20);
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn v4l2_buffer_is_88_bytes_with_kernel_offsets() {
        assert_eq!(size_of::<v4l2_buffer>(), 88);
        assert_eq!(offset_of!(v4l2_buffer, timestamp), 24);
        assert_eq!(offset_of!(v4l2_buffer, timecode), 40);
        assert_eq!(offset_of!(v4l2_buffer, sequence), 56);
        assert_eq!(offset_of!(v4l2_buffer, memory), 60);
        assert_eq!(offset_of!(v4l2_buffer, m), 64);
        assert_eq!(offset_of!(v4l2_buffer, length), 72);
        assert_eq!(offset_of!(v4l2_buffer, reserved2), 76);
        assert_eq!(offset_of!(v4l2_buffer, tail), 80);
    }

    #[test]
    fn fourcc_constants() {
        assert_eq!(V4L2_PIX_FMT_GREY, 0x5945_5247);
        assert_eq!(V4L2_META_FMT_UVC, 0x4843_5655);
        assert_eq!(V4L2_META_FMT_UVC_MSXU_1_5, 0x4d43_5655);
        assert_eq!(V4L2_PIX_FMT_MJPEG, 0x4750_4a4d);
        assert_eq!(V4L2_BUF_FLAG_ERROR, 0x40);
        assert_eq!(fourcc_str(V4L2_PIX_FMT_GREY), "GREY");
        assert_eq!(fourcc_str(V4L2_META_FMT_UVC_MSXU_1_5), "UVCM");
        assert_eq!(fourcc_str(0x0102_4d4a), "JM??");
    }

    /// Values printed by a C program including `<linux/videodev2.h>` on
    /// this machine (the same headers fuprobe was built with).
    #[test]
    fn ioctl_numbers_match_the_c_headers() {
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_G_FMT, 0xc0d0_5604);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xc058_5609);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        let mut c = v4l2_capability::default();
        assert_eq!(Request::QueryCap(&mut c).number(), VIDIOC_QUERYCAP);
        let mut f = v4l2_format::new(V4L2_BUF_TYPE_META_CAPTURE);
        assert_eq!(Request::GFmt(&mut f).number(), VIDIOC_G_FMT);
        assert_eq!(Request::SFmt(&mut f).number(), VIDIOC_S_FMT);
        let mut r = v4l2_requestbuffers::default();
        assert_eq!(Request::ReqBufs(&mut r).number(), VIDIOC_REQBUFS);
        let mut b = v4l2_buffer::default();
        assert_eq!(Request::QueryBuf(&mut b).number(), VIDIOC_QUERYBUF);
        assert_eq!(Request::QBuf(&mut b).number(), VIDIOC_QBUF);
        assert_eq!(Request::DQBuf(&mut b).number(), VIDIOC_DQBUF);
        let mut t = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        assert_eq!(Request::StreamOn(&mut t).number(), VIDIOC_STREAMON);
        assert_eq!(Request::StreamOff(&mut t).number(), VIDIOC_STREAMOFF);
        assert_eq!(Request::StreamOff(&mut t).name(), "VIDIOC_STREAMOFF");
        // The argument pointer is the struct itself.
        let mut req = Request::QueryBuf(&mut b);
        assert_eq!(req.arg(), (&raw mut b).cast());
    }

    #[test]
    fn capability_and_format_layouts_match_the_kernel() {
        assert_eq!(size_of::<v4l2_capability>(), 104);
        assert_eq!(offset_of!(v4l2_capability, card), 16);
        assert_eq!(offset_of!(v4l2_capability, bus_info), 48);
        assert_eq!(offset_of!(v4l2_capability, version), 80);
        assert_eq!(offset_of!(v4l2_capability, capabilities), 84);
        assert_eq!(offset_of!(v4l2_capability, device_caps), 88);
        assert_eq!(offset_of!(v4l2_capability, reserved), 92);
        assert_eq!(size_of::<v4l2_format>(), 208);
        assert_eq!(offset_of!(v4l2_format, fmt), 8);
        assert_eq!(offset_of!(v4l2_pix_format, pixelformat), 8);
        assert_eq!(offset_of!(v4l2_pix_format, sizeimage), 20);
        assert_eq!(offset_of!(v4l2_pix_format, xfer_func), 44);
        assert_eq!(offset_of!(v4l2_meta_format, buffersize), 4);
        assert_eq!(size_of::<v4l2_requestbuffers>(), 20);
        assert_eq!(offset_of!(v4l2_requestbuffers, capabilities), 12);
        assert_eq!(offset_of!(v4l2_requestbuffers, flags), 16);
        let mut f = v4l2_format::new(V4L2_BUF_TYPE_VIDEO_CAPTURE);
        f.pix_mut().width = 640;
        f.meta_mut().buffersize = 7; // aliases pix.height
        assert_eq!(f.pix().height, 7);
        assert_eq!(f.meta().dataformat, 640);
        assert!(format!("{f:?}").contains("fmt(..)"));
        let mut c = v4l2_capability::default();
        c.driver[..8].copy_from_slice(b"uvcvideo");
        c.bus_info[..18].copy_from_slice(b"usb-0000:00:14.0-9");
        assert_eq!(c.driver_str(), "uvcvideo");
        assert_eq!(c.bus_info_str(), "usb-0000:00:14.0-9");
        assert_eq!(c.card_str(), "");
        assert_eq!(cstr_field(b"abc"), "abc");
    }

    #[test]
    fn ioctl_on_a_bad_fd_returns_the_os_error() {
        let mut c = v4l2_capability::default();
        let e = ioctl(-1, Request::QueryCap(&mut c)).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(crate::sys::errno::EBADF));
    }

    #[test]
    fn default_is_zeroed() {
        let b = v4l2_buffer::default();
        assert_eq!(b.index, 0);
        assert!(format!("{b:?}").contains("m(0x0)"));
        assert_eq!(v4l2_plane::default().m.raw(), 0);
    }

    #[test]
    fn union_constructors_write_the_full_width() {
        // A 4-byte variant never leaves the upper bytes undefined.
        let m = v4l2_buffer_m::dmabuf(-1);
        assert_eq!(m.raw(), 0xffff_ffff);
        assert_eq!(m.fd(), -1);
        assert_eq!(format!("{m:?}"), "m(0xffffffff)");
        let m = v4l2_buffer_m::mmap(0x1000);
        assert_eq!((m.raw(), m.offset()), (0x1000, 0x1000));
        let m = v4l2_buffer_m::userptr(-2);
        assert_eq!(m.userptr_value(), -2);
        assert_eq!(m.raw(), u64::MAX - 1);
        let p = v4l2_plane_m::dmabuf(-1);
        assert_eq!(p.raw(), 0xffff_ffff);
        assert_eq!(v4l2_plane_m::mem_offset(7).raw(), 7);
    }
}
