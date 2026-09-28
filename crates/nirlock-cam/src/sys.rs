//! The system-call seam of the capture path.
//!
//! Everything `Stream`, `IrCapture` and the RGB thread need from the OS
//! goes through [`Sys`]: `open`, the single V4L2 [`ioctl`](crate::v4l2::ioctl)
//! entry point, `mmap`/`munmap`, `poll`, `fstat` (device number), `close`,
//! the monotonic clock and a sleep. [`RealSys`] is the production
//! implementation; the tests provide a fake that scripts buffers, timings
//! and failures (`S_FMT` silently changed, `EBUSY`, delayed metadata,
//! flagged buffers) without a camera.
//!
//! `Sys` values are cloned into each `Stream` (the real one is a ZST; the
//! fake is a shared handle) so a `Stream` can always tear itself down in
//! `Drop`.
//!
//! This module and `v4l2.rs` are the only places of the crate that name
//! `libc` (the source guard in `lib.rs` enforces it); the errno values the
//! rest of the crate needs are re-exported here as plain constants.

use std::os::fd::RawFd;
use std::path::Path;

use crate::v4l2::{self, Request};

/// The errno values the crate inspects, as plain integers.
pub mod errno {
    pub const EAGAIN: i32 = libc::EAGAIN;
    pub const EBADF: i32 = libc::EBADF;
    pub const EBUSY: i32 = libc::EBUSY;
    pub const EFAULT: i32 = libc::EFAULT;
    pub const EINTR: i32 = libc::EINTR;
    pub const EINVAL: i32 = libc::EINVAL;
    pub const EIO: i32 = libc::EIO;
    pub const ENOENT: i32 = libc::ENOENT;
    pub const ENOMEM: i32 = libc::ENOMEM;
}

/// `struct pollfd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollFd {
    pub fd: RawFd,
    pub events: i16,
    pub revents: i16,
}

pub const POLLIN: i16 = libc::POLLIN;
pub const POLLERR: i16 = libc::POLLERR;
pub const POLLHUP: i16 = libc::POLLHUP;
pub const POLLNVAL: i16 = libc::POLLNVAL;

pub trait Sys: Clone + Send + 'static {
    /// `open(dev, O_RDWR | O_NONBLOCK | O_CLOEXEC)`.
    fn open(&mut self, dev: &Path) -> std::io::Result<RawFd>;
    /// One of the nine allowed V4L2 requests.
    fn ioctl(&mut self, fd: RawFd, req: Request<'_>) -> std::io::Result<()>;
    /// `mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, offset)`.
    fn mmap(&mut self, fd: RawFd, len: usize, offset: u32) -> std::io::Result<*mut u8>;
    /// `munmap` of a region returned by [`Sys::mmap`].
    fn munmap(&mut self, ptr: *mut u8, len: usize);
    /// `poll(2)`; returns the number of ready descriptors (0 on timeout).
    /// `EINTR` is retried with the remaining time, so a signal never cuts
    /// a wait (the 25 ms metadata grace, for one) short.
    fn poll(&mut self, fds: &mut [PollFd], timeout_ms: i32) -> std::io::Result<usize>;
    /// `fstat(fd).st_rdev` as `(major, minor)`: the character device the
    /// descriptor really refers to (compared with the sysfs `dev` attribute
    /// of the node found at discovery).
    fn fstat_rdev(&mut self, fd: RawFd) -> std::io::Result<(u32, u32)>;
    fn close(&mut self, fd: RawFd);
    /// Seconds on `CLOCK_MONOTONIC` (uvcvideo stamps buffers with the same
    /// clock, so buffer timestamps and arrival times are comparable).
    fn mono_now(&mut self) -> f64;
    fn sleep_ms(&mut self, ms: u64);
}

/// The production [`Sys`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RealSys;

impl Sys for RealSys {
    fn open(&mut self, dev: &Path) -> std::io::Result<RawFd> {
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(dev)?;
        // The `Stream` owns the descriptor from here on and closes it in
        // `stop()`; `std::fs::File` must not close it on drop.
        Ok(std::os::fd::IntoRawFd::into_raw_fd(f))
    }

    fn ioctl(&mut self, fd: RawFd, req: Request<'_>) -> std::io::Result<()> {
        v4l2::ioctl(fd, req)
    }

    #[allow(unsafe_code)]
    fn mmap(&mut self, fd: RawFd, len: usize, offset: u32) -> std::io::Result<*mut u8> {
        // SAFETY: a fresh anonymous-address mapping of a V4L2 buffer the
        // driver exported through QUERYBUF (`len`/`offset` come from it);
        // MAP_SHARED on a device fd creates no aliasing with Rust-owned
        // memory. The mapping is released with `munmap` in `Stream::stop`.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                libc::off_t::from(offset),
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(p.cast())
    }

    #[allow(unsafe_code)]
    fn munmap(&mut self, ptr: *mut u8, len: usize) {
        // SAFETY: `ptr`/`len` are exactly what `mmap` above returned and
        // the caller (`Stream::stop`) drops every reference into the
        // region before calling this.
        unsafe {
            libc::munmap(ptr.cast(), len);
        }
    }

    #[allow(unsafe_code)]
    fn poll(&mut self, fds: &mut [PollFd], timeout_ms: i32) -> std::io::Result<usize> {
        let mut raw: Vec<libc::pollfd> = fds
            .iter()
            .map(|f| libc::pollfd {
                fd: f.fd,
                events: f.events,
                revents: 0,
            })
            .collect();
        let started = std::time::Instant::now();
        let mut left = timeout_ms;
        let r = loop {
            // SAFETY: `raw` is a live, correctly sized array of `pollfd` for
            // the duration of the call.
            let r = unsafe { libc::poll(raw.as_mut_ptr(), raw.len() as libc::nfds_t, left) };
            if r >= 0 {
                break r;
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINTR) {
                return Err(e);
            }
            if timeout_ms < 0 {
                continue; // infinite wait: just retry
            }
            let elapsed = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
            left = timeout_ms - elapsed;
            if left <= 0 {
                return Ok(0);
            }
        };
        for (f, r) in fds.iter_mut().zip(raw.iter()) {
            f.revents = r.revents;
        }
        Ok(r as usize)
    }

    #[allow(unsafe_code)]
    fn fstat_rdev(&mut self, fd: RawFd) -> std::io::Result<(u32, u32)> {
        // SAFETY: `st` is a valid, writable, zero-initialised `stat` (plain
        // integers, every bit pattern valid) that the kernel fills; `fd` is
        // a descriptor this crate opened.
        let st = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &raw mut st) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            st
        };
        let rdev = st.st_rdev;
        // `major`/`minor` are the glibc `gnu_dev_*` bit manipulations.
        Ok((libc::major(rdev), libc::minor(rdev)))
    }

    #[allow(unsafe_code)]
    fn close(&mut self, fd: RawFd) {
        // SAFETY: `fd` was returned by `open` above and is closed exactly
        // once (`Stream::stop` sets it to -1 afterwards).
        unsafe {
            libc::close(fd);
        }
    }

    fn mono_now(&mut self) -> f64 {
        mono_now()
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Seconds on `CLOCK_MONOTONIC` (port of `fu::mono_now`).
#[allow(unsafe_code)]
pub fn mono_now() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec`; CLOCK_MONOTONIC always
    // exists on Linux.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts);
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// `umask(077)`: every file the capture tools create is private to the
/// user (`nirlockctl record`, DESIGN §2.10). Returns the previous mask.
#[allow(unsafe_code)]
pub fn umask_private() -> u32 {
    // SAFETY: `umask` is always safe to call; it only changes the process
    // creation mask.
    unsafe { libc::umask(0o077) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_now_is_monotonic_and_matches_the_kernel_clock() {
        let a = mono_now();
        let b = RealSys.mono_now();
        assert!(b >= a);
        assert!(a > 0.0);
    }

    #[test]
    fn real_open_of_a_missing_node_fails_and_poll_times_out() {
        let mut s = RealSys;
        assert!(s.open(Path::new("/nonexistent/video99")).is_err());
        let mut fds = [];
        assert_eq!(s.poll(&mut fds, 1).unwrap(), 0);
        // A closed fd reports POLLNVAL without failing the call.
        let mut fds = [PollFd {
            fd: 1_000_000,
            events: POLLIN,
            revents: 0,
        }];
        assert_eq!(s.poll(&mut fds, 1).unwrap(), 1);
        assert_ne!(fds[0].revents & POLLNVAL, 0);
        assert!(s.mmap(-1, 4096, 0).is_err());
        assert_eq!(
            s.fstat_rdev(-1).unwrap_err().raw_os_error(),
            Some(errno::EBADF)
        );
        s.sleep_ms(0);
        assert_eq!(libc::POLLIN, POLLIN);
    }

    #[test]
    fn fstat_rdev_of_a_character_device_matches_sysfs() {
        // /dev/null is 1:3 everywhere.
        let f = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
        assert_eq!(RealSys.fstat_rdev(fd).unwrap(), (1, 3));
        // A regular file has rdev 0:0 (not a device).
        let f = std::fs::File::open("/proc/self/exe").unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
        assert_eq!(RealSys.fstat_rdev(fd).unwrap(), (0, 0));
    }

    #[test]
    fn errno_constants_are_the_platform_ones() {
        assert_eq!(errno::EBUSY, libc::EBUSY);
        assert_eq!(errno::EAGAIN, libc::EAGAIN);
        assert_eq!(errno::EINTR, libc::EINTR);
        assert_eq!(errno::ENOENT, libc::ENOENT);
    }
}
