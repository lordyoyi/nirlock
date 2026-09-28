//! A scripted [`Sys`] for tests without a camera: virtual monotonic clock,
//! per-node buffer queues, deliveries scheduled at a virtual time, and
//! knobs for every failure the capture path must handle (`EBUSY`, a driver
//! that silently changes `S_FMT`, flagged buffers, short `REQBUFS`).
//!
//! `poll` advances the virtual clock to the earliest scheduled delivery
//! (or the timeout), so a test that schedules a metadata buffer 20 ms after
//! its video frame exercises the real 25 ms grace poll deterministically.
//!
//! Fault model of `EBUSY`: uvcvideo never fails `open(2)` with it; the
//! busy condition surfaces on the first privileged ioctl (`S_FMT`,
//! `REQBUFS`) of a second opener (`uvc_acquire_privileges`, and
//! `vb2_is_busy` on the metadata node). The knobs `sfmt_errors` /
//! `reqbufs_errors` model that; `open_errors` is kept for the degenerate
//! case and for non-busy open failures (`ENOENT`).
//!
//! Mappings are raw heap blocks (`Box::into_raw`) written through the raw
//! pointer that `mmap` handed out, so the `Stream` slices and the fake's
//! deliveries never form conflicting Rust references (Miri-clean).

use std::collections::{HashMap, VecDeque};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::sys::{POLLIN, POLLNVAL, PollFd, Sys, errno};
use crate::v4l2::{
    Request, V4L2_BUF_TYPE_META_CAPTURE, V4L2_CAP_META_CAPTURE, V4L2_CAP_STREAMING,
    V4L2_CAP_VIDEO_CAPTURE, V4L2_META_FMT_UVC, V4L2_PIX_FMT_GREY, V4L2_PIX_FMT_MJPEG,
    v4l2_buffer_m,
};

#[derive(Clone, Debug)]
pub struct Delivery {
    pub at: f64,
    pub sequence: u32,
    pub flags: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Buf {
    len: usize,
    map: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct FakeNode {
    pub driver: String,
    pub bus_info: String,
    pub device_caps: u32,
    /// errno for each successive `open`, then success.
    pub open_errors: VecDeque<i32>,
    /// errno for each successive `S_FMT`, then success (the real `EBUSY`).
    pub sfmt_errors: VecDeque<i32>,
    /// errno for each successive `REQBUFS`, then success.
    pub reqbufs_errors: VecDeque<i32>,
    /// What `fstat` reports for the node (`(81, N)` by default).
    pub rdev: (u32, u32),
    /// What `S_FMT` reads back regardless of the request (silent change).
    pub sfmt_pix_override: Option<(u32, u32, u32)>,
    pub sfmt_meta_override: Option<u32>,
    pub reqbufs_count_override: Option<u32>,
    pub fail_streamon: Option<i32>,
    pub fail_mmap: bool,
    pub buffer_len: usize,
    pub cur_pix: (u32, u32, u32),
    pub cur_meta: u32,
    bufs: Vec<Buf>,
    queue: VecDeque<u32>,
    pending: VecDeque<Delivery>,
    streaming: bool,
    opens: usize,
    /// The thread that opened the node: virtual time never advances past
    /// a buffer that another thread still has to dequeue.
    owner: Option<std::thread::ThreadId>,
}

impl FakeNode {
    fn new(caps: u32, buffer_len: usize) -> Self {
        Self {
            driver: "uvcvideo".into(),
            bus_info: "usb-0000:00:14.0-9".into(),
            device_caps: caps | V4L2_CAP_STREAMING,
            open_errors: VecDeque::new(),
            sfmt_errors: VecDeque::new(),
            reqbufs_errors: VecDeque::new(),
            rdev: (81, 0),
            sfmt_pix_override: None,
            sfmt_meta_override: None,
            reqbufs_count_override: None,
            fail_streamon: None,
            fail_mmap: false,
            buffer_len,
            cur_pix: (640, 360, V4L2_PIX_FMT_GREY),
            cur_meta: V4L2_META_FMT_UVC,
            bufs: Vec::new(),
            queue: VecDeque::new(),
            pending: VecDeque::new(),
            streaming: false,
            opens: 0,
            owner: None,
        }
    }

    pub fn ir_video() -> Self {
        Self::new(V4L2_CAP_VIDEO_CAPTURE, 230_400)
    }

    pub fn ir_meta() -> Self {
        Self::new(V4L2_CAP_META_CAPTURE, 1024)
    }

    pub fn rgb_video() -> Self {
        let mut n = Self::new(V4L2_CAP_VIDEO_CAPTURE, 1280 * 720 * 2);
        n.cur_pix = (1280, 720, V4L2_PIX_FMT_MJPEG);
        n
    }

    fn ready(&self, now: f64) -> bool {
        self.streaming
            && !self.queue.is_empty()
            && self.pending.front().is_some_and(|d| d.at <= now)
    }

    /// Earliest delivery strictly after `now` that could be dequeued.
    fn earliest_after(&self, now: f64) -> Option<f64> {
        if self.streaming && !self.queue.is_empty() {
            self.pending
                .iter()
                .map(|d| d.at)
                .filter(|&at| at > now)
                .fold(None, |m, at| Some(m.map_or(at, |m: f64| m.min(at))))
        } else {
            None
        }
    }
}

#[derive(Default)]
struct State {
    now: f64,
    next_fd: RawFd,
    nodes: HashMap<PathBuf, FakeNode>,
    open: HashMap<RawFd, PathBuf>,
    log: Vec<(PathBuf, String)>,
    /// Live mappings by address: `(len, node, buffer index)`; the memory
    /// is a leaked `Box<[u8]>` reclaimed in `munmap`.
    maps: HashMap<usize, (usize, PathBuf, u32)>,
    sleeps: Vec<u64>,
}

#[derive(Clone)]
pub struct FakeSys(Arc<Mutex<State>>);

fn os(errno: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(errno)
}

impl FakeSys {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            now: 1000.0,
            next_fd: 10,
            ..State::default()
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers (or replaces) a node. Deliveries scheduled earlier are
    /// dropped with it. The default `rdev` is `(81, N)` for `/dev/videoN`.
    pub fn add_node(&self, dev: &Path, mut node: FakeNode) {
        if node.rdev == (81, 0)
            && let Some(n) = dev
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("video"))
                .and_then(|n| n.parse::<u32>().ok())
        {
            node.rdev = (81, n);
        }
        self.lock().nodes.insert(dev.to_path_buf(), node);
    }

    /// Schedules a buffer for `dev` at virtual time `now + at_rel` seconds.
    pub fn deliver(&self, dev: &Path, at_rel: f64, sequence: u32, flags: u32, data: Vec<u8>) {
        let mut st = self.lock();
        let at = st.now + at_rel;
        if let Some(n) = st.nodes.get_mut(dev) {
            n.pending.push_back(Delivery {
                at,
                sequence,
                flags,
                data,
            });
        }
    }

    pub fn now(&self) -> f64 {
        self.lock().now
    }

    pub fn sleeps(&self) -> Vec<u64> {
        self.lock().sleeps.clone()
    }

    pub fn is_open(&self, fd: RawFd) -> bool {
        self.lock().open.contains_key(&fd)
    }

    pub fn open_fds(&self) -> usize {
        self.lock().open.len()
    }

    pub fn open_count(&self, dev: &Path) -> usize {
        self.lock().nodes.get(dev).map_or(0, |n| n.opens)
    }

    pub fn live_mappings(&self) -> usize {
        self.lock().maps.len()
    }

    pub fn pending(&self, dev: &Path) -> usize {
        self.lock().nodes.get(dev).map_or(0, |n| n.pending.len())
    }

    pub fn streaming(&self, dev: &Path) -> bool {
        self.lock().nodes.get(dev).is_some_and(|n| n.streaming)
    }

    /// The calls made on `dev`, in order.
    pub fn log_for(&self, dev: &Path) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .filter(|(d, _)| d == dev)
            .map(|(_, s)| s.clone())
            .collect()
    }

    /// Every call, as `"/dev/videoN: what"`, in order.
    pub fn log(&self) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .map(|(d, s)| format!("{}: {s}", d.display()))
            .collect()
    }
}

impl Default for FakeSys {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    fn node_of(&mut self, fd: RawFd) -> std::io::Result<(&mut FakeNode, PathBuf)> {
        let dev = self
            .open
            .get(&fd)
            .cloned()
            .ok_or_else(|| os(errno::EBADF))?;
        let n = self.nodes.get_mut(&dev).ok_or_else(|| os(errno::EBADF))?;
        Ok((n, dev))
    }

    fn logn(&mut self, dev: &Path, what: String) {
        self.log.push((dev.to_path_buf(), what));
    }
}

impl Sys for FakeSys {
    fn open(&mut self, dev: &Path) -> std::io::Result<RawFd> {
        let mut st = self.lock();
        st.logn(dev, "open".into());
        let Some(n) = st.nodes.get_mut(dev) else {
            return Err(os(errno::ENOENT));
        };
        n.opens += 1;
        if let Some(e) = n.open_errors.pop_front() {
            return Err(os(e));
        }
        n.owner = Some(std::thread::current().id());
        let fd = st.next_fd;
        st.next_fd += 1;
        st.open.insert(fd, dev.to_path_buf());
        Ok(fd)
    }

    #[allow(unsafe_code)]
    fn ioctl(&mut self, fd: RawFd, req: Request<'_>) -> std::io::Result<()> {
        let mut st = self.lock();
        let now = st.now;
        let (n, dev) = st.node_of(fd)?;
        let mut what = req.name().to_string();
        let r = match req {
            Request::QueryCap(c) => {
                let d = n.driver.as_bytes();
                c.driver[..d.len().min(16)].copy_from_slice(&d[..d.len().min(16)]);
                let b = n.bus_info.as_bytes();
                c.bus_info[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
                c.device_caps = n.device_caps;
                c.capabilities = n.device_caps | 0x8000_0000;
                Ok(())
            }
            Request::GFmt(f) => {
                if f.type_ == V4L2_BUF_TYPE_META_CAPTURE {
                    let m = f.meta_mut();
                    m.dataformat = n.cur_meta;
                    m.buffersize = 1024;
                } else {
                    let p = f.pix_mut();
                    (p.width, p.height, p.pixelformat) = n.cur_pix;
                    p.sizeimage = n.buffer_len as u32;
                }
                Ok(())
            }
            Request::SFmt(f) => {
                if let Some(e) = n.sfmt_errors.pop_front() {
                    st.logn(&dev, what);
                    return Err(os(e));
                }
                if f.type_ == V4L2_BUF_TYPE_META_CAPTURE {
                    let m = f.meta_mut();
                    n.cur_meta = n.sfmt_meta_override.unwrap_or(m.dataformat);
                    m.dataformat = n.cur_meta;
                    m.buffersize = 1024;
                } else {
                    let p = f.pix_mut();
                    n.cur_pix = n
                        .sfmt_pix_override
                        .unwrap_or((p.width, p.height, p.pixelformat));
                    (p.width, p.height, p.pixelformat) = n.cur_pix;
                    p.sizeimage = n.buffer_len as u32;
                }
                Ok(())
            }
            Request::ReqBufs(r) => {
                what = format!("{what} {}", r.count);
                if let Some(e) = n.reqbufs_errors.pop_front() {
                    st.logn(&dev, what);
                    return Err(os(e));
                }
                r.count = n.reqbufs_count_override.unwrap_or(r.count);
                n.bufs = (0..r.count)
                    .map(|_| Buf {
                        len: n.buffer_len,
                        map: None,
                    })
                    .collect();
                n.queue.clear();
                Ok(())
            }
            Request::QueryBuf(b) => {
                what = format!("{what} {}", b.index);
                match n.bufs.get(b.index as usize) {
                    Some(buf) => {
                        b.length = buf.len as u32;
                        b.m = v4l2_buffer_m::mmap(b.index << 16);
                        Ok(())
                    }
                    None => Err(os(errno::EINVAL)),
                }
            }
            Request::QBuf(b) => {
                what = format!("{what} {}", b.index);
                if (b.index as usize) < n.bufs.len() && !n.queue.contains(&b.index) {
                    n.queue.push_back(b.index);
                    Ok(())
                } else {
                    Err(os(errno::EINVAL))
                }
            }
            Request::DQBuf(b) => {
                if !n.streaming {
                    Err(os(errno::EINVAL))
                } else if n.ready(now) {
                    let idx = n.queue.pop_front().unwrap_or(0);
                    let d = n.pending.pop_front().unwrap_or(Delivery {
                        at: now,
                        sequence: 0,
                        flags: 0,
                        data: Vec::new(),
                    });
                    b.index = idx;
                    b.bytesused = d.data.len() as u32;
                    b.sequence = d.sequence;
                    b.flags = d.flags;
                    b.timestamp.tv_sec = d.at.floor() as i64;
                    b.timestamp.tv_usec = ((d.at - d.at.floor()) * 1e6).round() as i64;
                    let map = n.bufs.get(idx as usize).and_then(|x| x.map);
                    if let Some(ptr) = map
                        && let Some(&(len, _, _)) = st.maps.get(&ptr)
                    {
                        let k = d.data.len().min(len);
                        // SAFETY: `ptr`/`len` is a live block handed out by
                        // `mmap` (removed from `maps` only in `munmap`);
                        // `k <= len` and `k <= d.data.len()`; the write goes
                        // through the raw pointer, never through a Rust
                        // reference, so it does not conflict with the
                        // `&[u8]` a `Stream` may build over the same block.
                        unsafe {
                            std::ptr::copy_nonoverlapping(d.data.as_ptr(), ptr as *mut u8, k);
                        }
                    }
                    Ok(())
                } else {
                    Err(os(errno::EAGAIN))
                }
            }
            Request::StreamOn(_) => match n.fail_streamon {
                Some(e) => Err(os(e)),
                None => {
                    n.streaming = true;
                    Ok(())
                }
            },
            Request::StreamOff(_) => {
                n.streaming = false;
                n.queue.clear();
                Ok(())
            }
        };
        st.logn(&dev, what);
        r
    }

    fn mmap(&mut self, fd: RawFd, len: usize, offset: u32) -> std::io::Result<*mut u8> {
        let mut st = self.lock();
        let dev = st.open.get(&fd).cloned().ok_or_else(|| os(errno::EBADF))?;
        let idx = offset >> 16;
        st.logn(&dev, format!("mmap {idx}"));
        let n = st.nodes.get_mut(&dev).ok_or_else(|| os(errno::EBADF))?;
        if n.fail_mmap {
            return Err(os(errno::EIO));
        }
        let mem: Box<[u8]> = vec![0u8; len].into_boxed_slice();
        // The block is owned by `maps` as a raw pointer until `munmap`.
        let ptr = Box::into_raw(mem).cast::<u8>();
        if let Some(b) = n.bufs.get_mut(idx as usize) {
            b.map = Some(ptr as usize);
        }
        st.maps.insert(ptr as usize, (len, dev, idx));
        Ok(ptr)
    }

    #[allow(unsafe_code)]
    fn munmap(&mut self, ptr: *mut u8, _len: usize) {
        let mut st = self.lock();
        if let Some((len, dev, idx)) = st.maps.remove(&(ptr as usize)) {
            // SAFETY: `ptr`/`len` are exactly the `Box<[u8]>` leaked in
            // `mmap` (the map entry proves it was never freed); the
            // `Stream` calling us has dropped every slice into it.
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
            }
            st.logn(&dev, format!("munmap {idx}"));
            if let Some(n) = st.nodes.get_mut(&dev)
                && let Some(b) = n.bufs.get_mut(idx as usize)
            {
                b.map = None;
            }
        }
    }

    fn poll(&mut self, fds: &mut [PollFd], timeout_ms: i32) -> std::io::Result<usize> {
        fn mark(st: &State, fds: &mut [PollFd]) -> usize {
            let mut count = 0;
            for f in fds.iter_mut() {
                f.revents = 0;
                match st.open.get(&f.fd).and_then(|d| st.nodes.get(d)) {
                    None => {
                        f.revents = POLLNVAL;
                        count += 1;
                    }
                    Some(n) if f.events & POLLIN != 0 && n.ready(st.now) => {
                        f.revents = POLLIN;
                        count += 1;
                    }
                    Some(_) => {}
                }
            }
            count
        }
        let me = std::thread::current().id();
        let started = std::time::Instant::now();
        let mut target: Option<f64> = None;
        loop {
            let mut st = self.lock();
            let target = *target.get_or_insert(st.now + f64::from(timeout_ms.max(0)) * 1e-3);
            let count = mark(&st, fds);
            if count > 0 || timeout_ms <= 0 || st.now >= target {
                return Ok(count);
            }
            // A buffer is ready on a node another thread owns: it must be
            // dequeued there before virtual time can move on. Wait in real
            // time (bounded, so a broken test cannot hang).
            let now = st.now;
            let blocked = st
                .nodes
                .values()
                .any(|n| n.owner.is_some_and(|o| o != me) && n.ready(now));
            if blocked {
                drop(st);
                if started.elapsed() > std::time::Duration::from_secs(5) {
                    return Ok(0);
                }
                std::thread::sleep(std::time::Duration::from_micros(100));
                continue;
            }
            // Advance to the next event of any node (never past one), or
            // to the timeout; then look again.
            let earliest = st
                .nodes
                .values()
                .filter_map(|n| n.earliest_after(now))
                .fold(f64::INFINITY, f64::min);
            st.now = earliest.min(target);
        }
    }

    fn fstat_rdev(&mut self, fd: RawFd) -> std::io::Result<(u32, u32)> {
        let mut st = self.lock();
        let (n, _) = st.node_of(fd)?;
        Ok(n.rdev)
    }

    fn close(&mut self, fd: RawFd) {
        let mut st = self.lock();
        if let Some(dev) = st.open.remove(&fd) {
            st.logn(&dev, "close".into());
        }
    }

    fn mono_now(&mut self) -> f64 {
        self.lock().now
    }

    fn sleep_ms(&mut self, ms: u64) {
        let mut st = self.lock();
        st.sleeps.push(ms);
        st.now += ms as f64 * 1e-3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4l2::v4l2_capability;

    #[test]
    fn unknown_fds_and_paths_fail_like_the_kernel() {
        let mut s = FakeSys::new();
        assert_eq!(
            s.open(Path::new("/dev/video7")).unwrap_err().raw_os_error(),
            Some(errno::ENOENT)
        );
        let mut c = v4l2_capability::default();
        assert_eq!(
            s.ioctl(42, Request::QueryCap(&mut c))
                .unwrap_err()
                .raw_os_error(),
            Some(errno::EBADF)
        );
        assert!(s.mmap(42, 16, 0).is_err());
        let mut fds = [PollFd {
            fd: 42,
            events: POLLIN,
            revents: 0,
        }];
        assert_eq!(s.poll(&mut fds, 10).unwrap(), 1);
        assert_eq!(fds[0].revents, POLLNVAL);
        s.close(42);
        assert_eq!(s.open_fds(), 0);
        assert_eq!(FakeSys::default().now(), 1000.0);
    }

    #[test]
    fn poll_advances_the_virtual_clock_to_the_next_delivery_or_timeout() {
        let mut s = FakeSys::new();
        let dev = PathBuf::from("/dev/video2");
        s.add_node(&dev, FakeNode::ir_video());
        let fd = s.open(&dev).unwrap();
        let t0 = s.now();
        let mut fds = [PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        // Not streaming: a poll just times out.
        assert_eq!(s.poll(&mut fds, 100).unwrap(), 0);
        assert!((s.now() - t0 - 0.1).abs() < 1e-9);
        let mut req = crate::v4l2::v4l2_requestbuffers {
            count: 2,
            ..Default::default()
        };
        s.ioctl(fd, Request::ReqBufs(&mut req)).unwrap();
        for i in 0..2 {
            let mut b = crate::v4l2::v4l2_buffer {
                index: i,
                ..Default::default()
            };
            s.ioctl(fd, Request::QBuf(&mut b)).unwrap();
        }
        let mut t = 1;
        s.ioctl(fd, Request::StreamOn(&mut t)).unwrap();
        s.deliver(&dev, 0.05, 3, 0, vec![1, 2, 3]);
        let t1 = s.now();
        assert_eq!(s.poll(&mut fds, 1000).unwrap(), 1);
        assert!((s.now() - t1 - 0.05).abs() < 1e-9, "jumped to the delivery");
        let mut b = crate::v4l2::v4l2_buffer::default();
        s.ioctl(fd, Request::DQBuf(&mut b)).unwrap();
        assert_eq!((b.sequence, b.bytesused, b.index), (3, 3, 0));
        assert_eq!(
            s.ioctl(fd, Request::DQBuf(&mut b))
                .unwrap_err()
                .raw_os_error(),
            Some(errno::EAGAIN)
        );
        s.sleep_ms(150);
        assert_eq!(s.sleeps(), vec![150]);
        assert!(s.streaming(&dev));
        assert_eq!(s.pending(&dev), 0);
        assert_eq!(s.open_count(&dev), 1);
        assert!(s.log().iter().any(|l| l == "/dev/video2: VIDIOC_STREAMON"));
        assert_eq!(s.fstat_rdev(fd).unwrap(), (81, 2));
        assert_eq!(
            s.fstat_rdev(99).unwrap_err().raw_os_error(),
            Some(errno::EBADF)
        );
    }

    #[test]
    fn sfmt_and_reqbufs_error_knobs_fire_once_each_then_succeed() {
        let mut s = FakeSys::new();
        let dev = PathBuf::from("/dev/video2");
        let mut n = FakeNode::ir_video();
        n.sfmt_errors.push_back(errno::EBUSY);
        n.reqbufs_errors.push_back(errno::EBUSY);
        s.add_node(&dev, n);
        let fd = s.open(&dev).unwrap();
        let mut f = crate::v4l2::v4l2_format::new(1);
        assert_eq!(
            s.ioctl(fd, Request::SFmt(&mut f))
                .unwrap_err()
                .raw_os_error(),
            Some(errno::EBUSY)
        );
        s.ioctl(fd, Request::SFmt(&mut f)).unwrap();
        let mut r = crate::v4l2::v4l2_requestbuffers {
            count: 2,
            ..Default::default()
        };
        assert_eq!(
            s.ioctl(fd, Request::ReqBufs(&mut r))
                .unwrap_err()
                .raw_os_error(),
            Some(errno::EBUSY)
        );
        s.ioctl(fd, Request::ReqBufs(&mut r)).unwrap();
        assert_eq!(
            s.log_for(&dev),
            [
                "open",
                "VIDIOC_S_FMT",
                "VIDIOC_S_FMT",
                "VIDIOC_REQBUFS 2",
                "VIDIOC_REQBUFS 2"
            ]
        );
    }
}
