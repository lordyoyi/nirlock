//! IR video node + its metadata node, paired by V4L2 sequence number
//! (port of `fu::IrCapture`, `phase0/v4l2cap.cpp` 411–506), the RGB
//! assist thread (`RgbThread`, `phase0/main.cpp` 394–471) and the
//! [`FrameSource`] trait the daemon's request engine consumes (the replay
//! source of DESIGN §10 implements the same trait).
//!
//! Open order (DESIGN §2.3 step 4, copied from `cmd_latency` +
//! `IrCapture::open`), in this exact sequence:
//! 1. `rgb.start()` — the RGB thread starts without waiting.
//! 2. `t0 = mono_now()` — immediately before the `open()` of the video node.
//! 3. `open(ir video)`.
//! 4. `open(ir meta)`. `open_ms` ends here.
//! 5. `S_FMT GREY 640x360` on video.
//! 6. `S_FMT UVCM` on meta.
//! 7. Meta: `REQBUFS 32`, `QUERYBUF/QBUF ×32`, `STREAMON` **first** (else
//!    the first, always lit, frame has no metadata buffer to land in).
//! 8. Video: `REQBUFS 8`, `QUERYBUF/QBUF ×8`, `STREAMON`. `streamon_ms` ends.
//!
//! Steps 3–8 are one retry unit: uvcvideo reports a node held by another
//! application not at `open(2)` but on the first privileged ioctl
//! (`S_FMT` / `REQBUFS`, `uvc_acquire_privileges`; `vb2_is_busy` on the
//! metadata node), so `Error::Busy` from any of those steps drops both
//! nodes, sleeps 150 ms and repeats the whole unit, three times, before
//! `unavailable camera_busy`.
//!
//! Every node is opened with [`Stream::open_pinned`]: `bus_info` and the
//! device number must be those of the node discovered under the pinned USB
//! sysfs path (DESIGN §2.3 step 1).
//!
//! Close order: signal the RGB thread, `STREAMOFF` + close the IR video
//! and metadata nodes, then join the RGB thread (the IR emitter must stop
//! within the < 70 ms budget of §2.3 step 5 regardless of the RGB poll).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::discover::Camera;
use crate::hw::HwProfile;
use crate::stream::{Identity, Kind, Stream};
use crate::sys::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd, Sys, errno};
use crate::uvcm::{MetaInfo, parse_uvcm};
use crate::v4l2::V4L2_BUF_FLAG_ERROR;
use crate::{Error, Result};

/// Grace period, on the meta fd only, before a video frame whose metadata
/// has not arrived yet is declared unlabelled (`v4l2cap.cpp` 492).
pub const META_GRACE_MS: i32 = 25;
/// Metadata buffers requested (`v4l2cap.cpp` 425).
pub const META_BUFS: u32 = 32;
/// Video buffers requested by the daemon (`cmd_latency`, `main.cpp` 1305).
pub const VIDEO_BUFS: u32 = 8;
/// RGB buffers (`RgbThread::run`, `main.cpp` 426).
pub const RGB_BUFS: u32 = 4;
/// IR `EBUSY`: retries × delay (DESIGN §2.3 step 4).
pub const BUSY_RETRIES: u32 = 3;
pub const BUSY_RETRY_MS: u64 = 150;
/// RGB thread poll timeout: how fast `stop()` is noticed (frames arrive
/// every 33 ms, so the timeout only bounds the reaction to `stop`).
pub const RGB_POLL_MS: i32 = 20;

/// One IR frame, copied out of the mmap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IrFrame {
    /// GREY, `width * height`, zero padded when short.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u32,
    /// Seconds, monotonic (buffer timestamp).
    pub v4l2_ts: f64,
    /// Seconds, monotonic, taken right after `DQBUF`.
    pub arrival: f64,
    pub bytesused: u32,
    /// `V4L2_BUF_FLAG_ERROR`, or `bytesused != profile.ir.bytes`. With
    /// `nodrop=1` incomplete frames ARE delivered. The frame MUST NOT be
    /// used for statistics, detection, enrollment or as the dark half of a
    /// difference pair.
    pub buf_error: bool,
    /// Paired by sequence number.
    pub meta: MetaInfo,
}

impl IrFrame {
    pub fn usable(&self) -> bool {
        !self.buf_error
    }

    /// The label of record: `meta.lit`, never a brightness guess.
    pub fn label(&self) -> Option<bool> {
        self.meta.lit
    }
}

/// Start-up marks (seconds on `CLOCK_MONOTONIC`) and first-frame marks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timing {
    /// Immediately before the `open()` of the video node.
    pub t0: f64,
    /// After both IR nodes are open.
    pub t_open_done: f64,
    /// After the video `STREAMON`.
    pub t_streamon_done: f64,
    /// Arrival of the first delivered video frame.
    pub first_frame: Option<f64>,
    /// Arrival of the first usable frame labelled lit.
    pub first_lit: Option<f64>,
}

impl Timing {
    pub fn rel_ms(&self, t: f64) -> f64 {
        (t - self.t0) * 1e3
    }

    pub fn open_ms(&self) -> f64 {
        self.rel_ms(self.t_open_done)
    }

    pub fn streamon_ms(&self) -> f64 {
        self.rel_ms(self.t_streamon_done)
    }

    pub fn first_frame_ms(&self) -> Option<f64> {
        self.first_frame.map(|t| self.rel_ms(t))
    }

    pub fn first_lit_ms(&self) -> Option<f64> {
        self.first_lit.map(|t| self.rel_ms(t))
    }
}

/// State of the RGB exposure lever (ADR-0006). The value is provisional
/// while the thread runs (`Pending` until it has opened and started the
/// node) and final after [`FrameSource::close`], which also downgrades a
/// `Streaming` thread that delivered no frame at all to `Failed` (as
/// fuprobe's `RgbThread::failed()`: a run only counts if RGB really ran).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RgbAssist {
    /// Not requested.
    Disabled,
    /// Requested but the profile's RGB node was not discovered.
    NoNode,
    /// Thread started, no decision yet (before `open`/`S_FMT`/`STREAMON`).
    Pending,
    /// The RGB node is streaming (or streamed until `close`).
    Streaming,
    /// `EBUSY` from `open`, `S_FMT`, `REQBUFS` or `STREAMON` (a browser
    /// holds the RGB node): IR-only, reported in the result and the journal.
    Denied,
    /// Any other failure in the RGB thread (IR continues).
    Failed(String),
}

impl RgbAssist {
    /// The `rgb_assist` value of the `result` / journal.
    pub fn as_str(&self) -> &'static str {
        match self {
            RgbAssist::Disabled => "disabled",
            RgbAssist::NoNode => "no_node",
            RgbAssist::Pending => "pending",
            RgbAssist::Streaming => "streaming",
            RgbAssist::Denied => "denied",
            RgbAssist::Failed(_) => "failed",
        }
    }
}

/// Counters of the RGB thread, read after `stop()` (`frames` may be
/// polled while running).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RgbStats {
    pub frames: u64,
    pub buf_errors: u64,
    /// Arrival (monotonic seconds) of the first RGB frame.
    pub first_frame: Option<f64>,
    pub last_frame: Option<f64>,
    pub status: Option<RgbAssist>,
    pub error: String,
}

impl RgbStats {
    pub fn fps(&self) -> Option<f64> {
        match (self.first_frame, self.last_frame) {
            (Some(a), Some(b)) if self.frames > 1 && b > a => {
                Some((self.frames - 1) as f64 / (b - a))
            }
            _ => None,
        }
    }
}

/// Streams the RGB node (MJPG 1280x720) on its own thread: frames are
/// dequeued and requeued without decoding (we only want the concurrent
/// USB/ISP load). Dropping it stops and joins the thread.
pub struct RgbThread {
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<RgbStats>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RgbThread {
    /// Starts the thread; `expect` is the pin of the RGB node (`None`
    /// skips the identity check; the daemon always passes it).
    pub fn start<S: Sys>(
        sys: S,
        dev: &Path,
        expect: Option<Identity>,
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(RgbStats::default()));
        let dev = dev.to_path_buf();
        let (stop2, stats2) = (Arc::clone(&stop), Arc::clone(&stats));
        let thread = std::thread::Builder::new()
            .name("nirlock-rgb".into())
            .spawn(move || {
                Self::run(
                    sys,
                    &dev,
                    expect.as_ref(),
                    width,
                    height,
                    fourcc,
                    &stop2,
                    &stats2,
                );
            })
            .ok();
        if thread.is_none() {
            let mut st = stats.lock().unwrap_or_else(|e| e.into_inner());
            st.status = Some(RgbAssist::Failed("thread spawn failed".into()));
        }
        Self {
            stop,
            stats,
            thread,
        }
    }

    /// `open` + `S_FMT` + `REQBUFS/QBUF/STREAMON` of the RGB node, aborted
    /// between steps if `stop` was already requested (an `IrCapture::open`
    /// that failed fast must not wake the RGB sensor for nothing).
    fn open_rgb<S: Sys>(
        sys: &S,
        dev: &Path,
        expect: Option<&Identity>,
        width: u32,
        height: u32,
        fourcc: u32,
        stop: &AtomicBool,
    ) -> Result<Option<Stream<S>>> {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let mut s = match expect {
            Some(id) => Stream::open_pinned(sys.clone(), dev, Kind::Video, id)?,
            None => Stream::open(sys.clone(), dev, Kind::Video)?,
        };
        s.set_pix_format(width, height, fourcc)?;
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        s.start(RGB_BUFS)?;
        Ok(Some(s))
    }

    #[allow(clippy::too_many_arguments)]
    fn run<S: Sys>(
        mut sys: S,
        dev: &Path,
        expect: Option<&Identity>,
        width: u32,
        height: u32,
        fourcc: u32,
        stop: &AtomicBool,
        stats: &Mutex<RgbStats>,
    ) {
        let set_status = |s: RgbAssist, err: String| {
            let mut st = stats.lock().unwrap_or_else(|e| e.into_inner());
            st.status = Some(s);
            st.error = err;
        };
        // Classified by error type, not by call site: uvcvideo raises the
        // busy condition at S_FMT/REQBUFS, not at open(2).
        let mut s = match Self::open_rgb(&sys, dev, expect, width, height, fourcc, stop) {
            Ok(Some(s)) => s,
            Ok(None) => return, // stopped before streaming: no verdict
            Err(e @ Error::Busy { .. }) => return set_status(RgbAssist::Denied, e.to_string()),
            Err(e) => return set_status(RgbAssist::Failed(e.to_string()), e.to_string()),
        };
        set_status(RgbAssist::Streaming, String::new());
        while !stop.load(Ordering::Relaxed) {
            let mut p = [PollFd {
                fd: s.fd(),
                events: POLLIN,
                revents: 0,
            }];
            match sys.poll(&mut p, RGB_POLL_MS) {
                Ok(0) => continue,
                Ok(_) => {}
                // The C++ loop tolerated every poll failure (`<= 0` →
                // continue). A bad descriptor or argument is fatal here (the
                // node is gone or this is a bug); anything else (ENOMEM) is
                // transient and keeps the lever running.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(errno::EBADF | errno::EINVAL | errno::EFAULT)
                    ) =>
                {
                    return set_status(RgbAssist::Failed(format!("poll: {e}")), e.to_string());
                }
                Err(_) => continue,
            }
            if p[0].revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                let msg = format!("{}: RGB node reported an error", dev.display());
                return set_status(RgbAssist::Failed(msg.clone()), msg);
            }
            let f = match s.dequeue() {
                Ok(Some(f)) => f,
                Ok(None) => continue,
                Err(e) => return set_status(RgbAssist::Failed(e.to_string()), e.to_string()),
            };
            let now = sys.mono_now();
            {
                let mut st = stats.lock().unwrap_or_else(|e| e.into_inner());
                if st.frames == 0 {
                    st.first_frame = Some(now);
                }
                st.last_frame = Some(now);
                st.frames += 1;
                if f.flags & V4L2_BUF_FLAG_ERROR != 0 {
                    st.buf_errors += 1;
                }
            }
            if let Err(e) = s.requeue(&f) {
                return set_status(RgbAssist::Failed(e.to_string()), e.to_string());
            }
        }
        // `s` drops here: STREAMOFF + munmap + close.
    }

    /// Current status (`None` until the thread reached a decision).
    pub fn status(&self) -> Option<RgbAssist> {
        self.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    pub fn stats(&self) -> RgbStats {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Asks the thread to stop without waiting for it.
    pub fn signal_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Signals the thread and joins it. Afterwards `status()` is final: a
    /// thread that never reached a verdict, or streamed without delivering
    /// a single frame, is `Failed`.
    pub fn stop(&mut self) {
        self.signal_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            let mut st = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            match st.status {
                None => {
                    st.status = Some(RgbAssist::Failed(
                        "RGB thread stopped before streaming".into(),
                    ));
                    st.error = "RGB thread stopped before streaming".into();
                }
                Some(RgbAssist::Streaming) if st.frames == 0 => {
                    st.status = Some(RgbAssist::Failed("no RGB frames".into()));
                    st.error = "no RGB frames".into();
                }
                _ => {}
            }
        }
    }
}

impl Drop for RgbThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the request engine needs from a frame source: the V4L2 capture
/// now, a recorded-session replay later (`nirlockd --camera replay:`).
pub trait FrameSource {
    /// Blocks up to `timeout_s` for the next video frame; `Ok(None)` on
    /// timeout.
    fn next(&mut self, timeout_s: f64) -> Result<Option<IrFrame>>;
    fn timing(&self) -> Timing;
    fn rgb_assist(&self) -> RgbAssist;
    /// Releases the camera (signal the RGB thread, STREAMOFF both IR
    /// nodes, join the RGB thread). Also done on drop. `rgb_assist()` is
    /// final only after this.
    fn close(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    pub video_bufs: u32,
    pub meta_bufs: u32,
    /// Start the RGB lever (`rgb_assist = always`).
    pub rgb: bool,
    pub busy_retries: u32,
    pub busy_retry_ms: u64,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            video_bufs: VIDEO_BUFS,
            meta_bufs: META_BUFS,
            rgb: false,
            busy_retries: BUSY_RETRIES,
            busy_retry_ms: BUSY_RETRY_MS,
        }
    }
}

pub struct IrCapture<S: Sys> {
    sys: S,
    video: Stream<S>,
    meta: Stream<S>,
    meta_by_seq: BTreeMap<u32, MetaInfo>,
    timing: Timing,
    rgb: Option<RgbThread>,
    rgb_status: RgbAssist,
    /// The thread's counters, kept after `close`.
    rgb_final: Option<RgbStats>,
    width: u32,
    height: u32,
    bytes: u32,
}

/// Steps 3–8 of the open order as one retryable unit.
struct IrUnit<'a> {
    cam: &'a Camera,
    profile: &'a HwProfile,
    opts: &'a OpenOptions,
    video_id: &'a Identity,
    meta_id: &'a Identity,
    ir_fourcc: u32,
    meta_fourcc: u32,
}

impl IrUnit<'_> {
    /// Returns `(video, meta, t_open_done)`; on any error both streams are
    /// dropped (STREAMOFF if needed, unmap, close) before returning.
    fn open<S: Sys>(&self, sys: &mut S) -> Result<(Stream<S>, Stream<S>, f64)> {
        // 3./4. open video, then meta.
        let mut video = Stream::open_pinned(
            sys.clone(),
            &self.cam.ir_cap.dev,
            Kind::Video,
            self.video_id,
        )?;
        let mut meta =
            Stream::open_pinned(sys.clone(), &self.cam.ir_meta.dev, Kind::Meta, self.meta_id)?;
        let t_open_done = sys.mono_now();
        // 5./6. formats: video, then meta (the meta format is sticky across
        // opens, so it is set every time).
        video.set_pix_format(
            self.profile.ir.width,
            self.profile.ir.height,
            self.ir_fourcc,
        )?;
        meta.set_meta_format(self.meta_fourcc)?;
        // 7. meta queue live BEFORE the video STREAMON.
        meta.start(self.opts.meta_bufs)?;
        // 8. video.
        video.start(self.opts.video_bufs)?;
        Ok((video, meta, t_open_done))
    }
}

impl<S: Sys> IrCapture<S> {
    /// Opens both IR nodes (and the RGB lever if asked) in the normative
    /// order. `cam` must carry the meta node (pinning guarantees it).
    pub fn open(cam: &Camera, profile: &HwProfile, opts: OpenOptions, mut sys: S) -> Result<Self> {
        if !cam.ir_cap.found() {
            return Err(Error::NotFound {
                vid: profile.match_.vendor.clone(),
                pid: profile.match_.product.clone(),
                root: String::from("(pinned camera)"),
            });
        }
        if !cam.ir_meta.found() {
            return Err(Error::NoMetaNode {
                usb_sysfs: cam.usb_sysfs.display().to_string(),
            });
        }
        let ir_fourcc = profile.ir_fourcc()?;
        let meta_fourcc = profile.meta_fourcc()?;
        let rgb_fourcc = profile.rgb_fourcc()?;
        let pin_of = |node: &crate::discover::Node| {
            Identity::of(&cam.usb_sysfs, node).ok_or_else(|| Error::Mismatch {
                pinned: cam.usb_sysfs.display().to_string(),
                found: vec![format!(
                    "{}: no bus_info/device number derivable at discovery",
                    node.dev.display()
                )],
            })
        };
        let video_id = pin_of(&cam.ir_cap)?;
        let meta_id = pin_of(&cam.ir_meta)?;

        // 1. RGB thread first (ADR-0006: the AE regime is bistable).
        let (rgb, rgb_status) = if !opts.rgb {
            (None, RgbAssist::Disabled)
        } else if !cam.rgb_cap.found() {
            (None, RgbAssist::NoNode)
        } else {
            (
                Some(RgbThread::start(
                    sys.clone(),
                    &cam.rgb_cap.dev,
                    Some(pin_of(&cam.rgb_cap)?),
                    profile.rgb.width,
                    profile.rgb.height,
                    rgb_fourcc,
                )),
                RgbAssist::Pending,
            )
        };

        // 2. t0, immediately before the first open(2). The busy retries
        // below are part of the opening cost the request sees.
        let t0 = sys.mono_now();
        let unit = IrUnit {
            cam,
            profile,
            opts: &opts,
            video_id: &video_id,
            meta_id: &meta_id,
            ir_fourcc,
            meta_fourcc,
        };
        let mut attempt = 0;
        let (video, meta, t_open_done) = loop {
            // 3.–8. as one unit; both nodes are dropped (closed) on failure.
            match unit.open(&mut sys) {
                Err(Error::Busy { .. }) if attempt < opts.busy_retries => {
                    attempt += 1;
                    sys.sleep_ms(opts.busy_retry_ms);
                }
                Err(e) => return Err(e),
                Ok(x) => break x,
            }
        };
        let t_streamon_done = sys.mono_now();
        Ok(Self {
            sys,
            video,
            meta,
            meta_by_seq: BTreeMap::new(),
            timing: Timing {
                t0,
                t_open_done,
                t_streamon_done,
                ..Timing::default()
            },
            rgb,
            rgb_status,
            rgb_final: None,
            width: profile.ir.width,
            height: profile.ir.height,
            bytes: profile.ir.bytes,
        })
    }

    fn drain_meta(&mut self) -> Result<()> {
        if !self.meta.streaming() {
            return Ok(());
        }
        while let Some(m) = self.meta.dequeue()? {
            let raw = self.meta.buffer(&m)?.to_vec();
            let mut mi = parse_uvcm(&raw);
            mi.raw = raw;
            if m.flags & V4L2_BUF_FLAG_ERROR != 0 {
                mi.buf_error = true;
                mi.lit = None; // never trust the label of a flagged metadata buffer
            }
            self.meta_by_seq.insert(m.sequence, mi);
            self.meta.requeue(&m)?;
        }
        Ok(())
    }

    /// Port of `IrCapture::next` (`v4l2cap.cpp` 455–500).
    pub fn next_frame(&mut self, timeout_s: f64) -> Result<Option<IrFrame>> {
        let deadline = self.sys.mono_now() + timeout_s;
        loop {
            let left = deadline - self.sys.mono_now();
            if left <= 0.0 {
                return Ok(None);
            }
            let mut fds = [
                PollFd {
                    fd: self.video.fd(),
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: self.meta.fd(),
                    events: POLLIN,
                    revents: 0,
                },
            ];
            let n = if self.meta.streaming() { 2 } else { 1 };
            let timeout = (left.min(0.25) * 1000.0) as i32 + 1;
            let r = self
                .sys
                .poll(&mut fds[..n], timeout)
                .map_err(|e| Error::from_os("poll", self.video.dev(), e))?;
            if r == 0 {
                continue;
            }
            if fds[0].revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                return Err(Error::Sys {
                    what: "poll",
                    dev: self.video.dev().display().to_string(),
                    source: std::io::Error::other("IR video node reported an error (device gone?)"),
                });
            }
            // A dead metadata stream fails closed (`unavailable
            // camera_error`), it never degrades into "unlabelled frames"
            // (nor into a busy loop until the deadline).
            if n == 2 && fds[1].revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                return Err(Error::Sys {
                    what: "poll",
                    dev: self.meta.dev().display().to_string(),
                    source: std::io::Error::other(
                        "IR metadata node reported an error (device gone?)",
                    ),
                });
            }
            if n == 2 && fds[1].revents & POLLIN != 0 {
                self.drain_meta()?;
            }
            if fds[0].revents & POLLIN == 0 {
                continue;
            }

            let Some(f) = self.video.dequeue()? else {
                continue;
            };
            let arrival = self.sys.mono_now();
            // `bytes` == width*height, validated by the profile.
            let want = self.bytes as usize;
            let mut pixels = vec![0u8; want]; // zero padded only so that the buffer is well defined
            let data = self.video.buffer(&f)?;
            let k = want.min(data.len());
            pixels[..k].copy_from_slice(&data[..k]);
            // nodrop=1 delivers incomplete frames: flag them, callers skip them.
            let buf_error = f.flags & V4L2_BUF_FLAG_ERROR != 0 || f.bytesused != self.bytes;
            self.video.requeue(&f)?;

            // uvcvideo completes the metadata buffer just before the video
            // buffer of the same frame, so it is normally already there.
            // Give it a short grace period anyway, then report absence
            // honestly.
            self.drain_meta()?;
            if !self.meta_by_seq.contains_key(&f.sequence) && self.meta.streaming() {
                let mut mfd = [PollFd {
                    fd: self.meta.fd(),
                    events: POLLIN,
                    revents: 0,
                }];
                let r = self
                    .sys
                    .poll(&mut mfd, META_GRACE_MS)
                    .map_err(|e| Error::from_os("poll", self.meta.dev(), e))?;
                if r > 0 && mfd[0].revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                    return Err(Error::Sys {
                        what: "poll",
                        dev: self.meta.dev().display().to_string(),
                        source: std::io::Error::other(
                            "IR metadata node reported an error (device gone?)",
                        ),
                    });
                }
                if r > 0 {
                    self.drain_meta()?;
                }
            }
            let meta = self
                .meta_by_seq
                .get(&f.sequence)
                .cloned()
                .unwrap_or_default();
            // Prune up to and including this sequence: a metadata buffer
            // arriving later for a delivered frame is discarded, and the
            // map never grows beyond the frames in flight.
            self.meta_by_seq = self.meta_by_seq.split_off(&(f.sequence.saturating_add(1)));
            if f.sequence == u32::MAX {
                self.meta_by_seq.clear();
            }

            let out = IrFrame {
                pixels,
                width: self.width,
                height: self.height,
                sequence: f.sequence,
                v4l2_ts: f.timestamp,
                arrival,
                bytesused: f.bytesused,
                buf_error,
                meta,
            };
            if self.timing.first_frame.is_none() {
                self.timing.first_frame = Some(arrival);
            }
            if self.timing.first_lit.is_none() && out.usable() && out.meta.lit == Some(true) {
                self.timing.first_lit = Some(arrival);
            }
            return Ok(Some(out));
        }
    }

    pub fn video_fd(&self) -> std::os::fd::RawFd {
        self.video.fd()
    }

    pub fn meta_fd(&self) -> std::os::fd::RawFd {
        self.meta.fd()
    }

    pub fn metadata_node_active(&self) -> bool {
        self.meta.streaming()
    }

    /// Metadata entries currently held (frames in flight), for tests/logs.
    pub fn meta_pending(&self) -> usize {
        self.meta_by_seq.len()
    }

    /// The RGB thread's counters (live while running, final after
    /// `close`); `None` when no thread was started.
    pub fn rgb_stats(&self) -> Option<RgbStats> {
        self.rgb
            .as_ref()
            .map(RgbThread::stats)
            .or_else(|| self.rgb_final.clone())
    }

    /// Current formats via `G_FMT` (read-only), for logs.
    pub fn format_descriptions(&mut self) -> (String, String) {
        (
            self.video.format_description(),
            self.meta.format_description(),
        )
    }

    /// The RGB status: the thread's verdict once it has one, else the
    /// state decided at open (`Pending` while the thread has not decided;
    /// final after `close`).
    fn rgb_state(&self) -> RgbAssist {
        match &self.rgb {
            Some(t) => t.status().unwrap_or_else(|| self.rgb_status.clone()),
            None => self.rgb_status.clone(),
        }
    }
}

impl<S: Sys> FrameSource for IrCapture<S> {
    fn next(&mut self, timeout_s: f64) -> Result<Option<IrFrame>> {
        self.next_frame(timeout_s)
    }

    fn timing(&self) -> Timing {
        self.timing
    }

    fn rgb_assist(&self) -> RgbAssist {
        self.rgb_state()
    }

    fn close(&mut self) {
        // Signal first, STREAMOFF the IR nodes (the emitter stops here),
        // join last: the RGB poll never delays the IR teardown.
        if let Some(t) = &self.rgb {
            t.signal_stop();
        }
        self.video.stop();
        self.meta.stop();
        self.meta_by_seq.clear();
        if let Some(mut t) = self.rgb.take() {
            t.stop();
            let st = t.stats();
            if let Some(s) = &st.status {
                self.rgb_status = s.clone();
            }
            self.rgb_final = Some(st);
        }
    }
}

impl<S: Sys> std::fmt::Debug for IrCapture<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrCapture")
            .field("video", &self.video)
            .field("meta", &self.meta)
            .field("timing", &self.timing)
            .field("rgb", &self.rgb_state())
            .field("meta_pending", &self.meta_by_seq.len())
            .finish()
    }
}

impl<S: Sys> Drop for IrCapture<S> {
    fn drop(&mut self) {
        FrameSource::close(self);
    }
}

/// Per-request hard rules of DESIGN §2.4 (fail closed):
/// 1. an unlabelled frame is never used;
/// 2. > 50 % of the *usable* frames unlabelled → `unavailable metadata`;
/// 3. two frames with **consecutive V4L2 `sequence` numbers**, both
///    delivered and labelled, carrying the same label → `unavailable
///    metadata` (a sequence gap between two deliveries makes equal labels
///    legitimate and does not count). "Labelled" is what the rule says: a
///    truncated video buffer (`nodrop=1`) still carries the firmware's
///    strobe state in its metadata and takes part in the alternation
///    check; only unlabelled frames (no metadata, or a flagged metadata
///    buffer, whose label `drain_meta` discards) are skipped.
///
/// Plus the shadow cross-checks (not rules; `audit.jsonl` only): FID
/// parity vs label, and mean(lit) > mean(dark) vs label. The brightness
/// cross-check needs the frame means, which the caller supplies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabelRules {
    pub frames: u32,
    /// Lit/dark over EVERY delivered labelled frame, usable or not: this is
    /// fuprobe's `n_lit`/`n_dark` (`main.cpp` 594-596) and the population
    /// `missing_metadata` is derived from. A truncated frame whose metadata
    /// carried a valid label counts here, as it did there.
    pub lit_all: u32,
    pub dark_all: u32,
    pub usable: u32,
    pub unlabelled_usable: u32,
    pub lit: u32,
    pub dark: u32,
    pub buf_errors: u32,
    pub meta_buf_errors: u32,
    pub meta_parse_errors: u32,
    /// `(seq, label)` of the last delivered labelled frame (usable or not).
    last: Option<(u32, bool)>,
    /// Sequences `seq+1` at which rule 3 fired.
    pub same_label_at: Vec<u32>,
    /// Shadow (a): frames where FID parity disagreed with the label /
    /// frames where both were known, with parity meaning `fid == lit`
    /// relative to the first observed pairing.
    pub fid_compared: u32,
    pub fid_disagree: u32,
    fid_rel: Option<bool>,
    /// Shadow (b): brightness guess vs label (`main.cpp` 589).
    pub bright_compared: u32,
    pub bright_agree: u32,
    prev_mean: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// > 50 % of usable frames unlabelled.
    TooManyUnlabelled { unlabelled: u32, usable: u32 },
    /// Same label on `sequence` and `sequence + 1`.
    SameLabelConsecutive { sequence: u32, label: bool },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::TooManyUnlabelled { unlabelled, usable } => write!(
                f,
                "metadata: {unlabelled} of {usable} usable frames carry no label"
            ),
            Violation::SameLabelConsecutive { sequence, label } => write!(
                f,
                "metadata: sequences {} and {sequence} both labelled {}",
                sequence.wrapping_sub(1),
                if *label { "lit" } else { "dark" }
            ),
        }
    }
}

impl LabelRules {
    /// Feeds one delivered frame (with its mean brightness when the caller
    /// has it, for the shadow cross-check) and returns the rule it
    /// violated, if any. Rule 3 is reported once per offending pair; rule
    /// 2 whenever it holds. Rule 2 keeps holding once the majority tips,
    /// so a caller that only reports (rather than ending the request on the
    /// first violation, as the daemon does) must deduplicate it. `lit`/`dark`/`unlabelled_usable` count usable
    /// frames only (rule 2's population); `frames` and the error counters
    /// count every delivered frame.
    pub fn observe(&mut self, f: &IrFrame, mean: Option<f64>) -> Option<Violation> {
        self.frames += 1;
        if f.meta.buf_error {
            self.meta_buf_errors += 1;
        }
        if f.meta.parse_error {
            self.meta_parse_errors += 1;
        }
        let mut violation = None;
        // Rule 3, the all-frames counters and the FID shadow: every
        // delivered labelled frame.
        if let Some(lit) = f.meta.lit {
            if lit {
                self.lit_all += 1;
            } else {
                self.dark_all += 1;
            }
            if let Some((seq, prev)) = self.last
                && f.sequence == seq.wrapping_add(1)
                && prev == lit
            {
                self.same_label_at.push(f.sequence);
                violation = Some(Violation::SameLabelConsecutive {
                    sequence: f.sequence,
                    label: lit,
                });
            }
            self.last = Some((f.sequence, lit));
            if let Some(fid) = f.meta.fid {
                let rel = *self.fid_rel.get_or_insert(fid == lit);
                self.fid_compared += 1;
                if (fid == lit) != rel {
                    self.fid_disagree += 1;
                }
            }
        }
        if !f.usable() {
            self.buf_errors += 1;
            return violation;
        }
        self.usable += 1;
        match f.meta.lit {
            None => self.unlabelled_usable += 1,
            Some(true) => self.lit += 1,
            Some(false) => self.dark += 1,
        }
        if let Some(m) = mean {
            if let (Some(prev), Some(lit)) = (self.prev_mean, f.meta.lit) {
                self.bright_compared += 1;
                if (m > prev) == lit {
                    self.bright_agree += 1;
                }
            }
            self.prev_mean = Some(m);
        }
        if violation.is_none() && self.unlabelled_usable * 2 > self.usable {
            violation = Some(Violation::TooManyUnlabelled {
                unlabelled: self.unlabelled_usable,
                usable: self.usable,
            });
        }
        violation
    }

    pub fn same_label_violations(&self) -> usize {
        self.same_label_at.len()
    }
}

/// A convenience for callers that own the discovery result: the pinned
/// camera's device paths as strings (journal fields).
pub fn node_paths(cam: &Camera) -> (PathBuf, PathBuf, PathBuf) {
    (
        cam.ir_cap.dev.clone(),
        cam.ir_meta.dev.clone(),
        cam.rgb_cap.dev.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Node;
    use crate::fake::{FakeNode, FakeSys};
    use crate::uvcm::fixtures::{block, frame, item6};
    use crate::v4l2::V4L2_PIX_FMT_MJPEG;

    const FRAME_S: f64 = 1.0 / 15.0;

    fn video2() -> PathBuf {
        PathBuf::from("/dev/video2")
    }
    fn video3() -> PathBuf {
        PathBuf::from("/dev/video3")
    }
    fn video0() -> PathBuf {
        PathBuf::from("/dev/video0")
    }

    fn camera(with_meta: bool, with_rgb: bool) -> Camera {
        let node = |dev: &str, iface: u32, index: u32| Node {
            sysfs: PathBuf::from(format!("/sys/class/video4linux/{}", &dev[5..])),
            dev: PathBuf::from(dev),
            card: "USB Camera: IR Camera".into(),
            iface,
            index,
            rdev: Some((81, dev[10..].parse().unwrap())),
        };
        Camera {
            usb_sysfs: PathBuf::from("/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9"),
            bcd_device: "0103".into(),
            rgb_cap: if with_rgb {
                node("/dev/video0", 0, 0)
            } else {
                Node::default()
            },
            rgb_meta: Node::default(),
            ir_cap: node("/dev/video2", 2, 0),
            ir_meta: if with_meta {
                node("/dev/video3", 2, 1)
            } else {
                Node::default()
            },
            other_devices: Vec::new(),
            refused: Vec::new(),
        }
    }

    fn profile() -> HwProfile {
        HwProfile::builtin().unwrap()
    }

    fn fake() -> FakeSys {
        let sys = FakeSys::new();
        sys.add_node(&video2(), FakeNode::ir_video());
        sys.add_node(&video3(), FakeNode::ir_meta());
        sys.add_node(&video0(), FakeNode::rgb_video());
        sys
    }

    fn pixels(v: u8) -> Vec<u8> {
        vec![v; 230_400]
    }

    /// Schedules video frame `seq` at `t` and its metadata `meta_offset`
    /// seconds later (negative = before, as uvcvideo does).
    fn schedule(sys: &FakeSys, t: f64, seq: u32, lit: bool, meta_offset: Option<f64>) {
        sys.deliver(&video2(), t, seq, 0, pixels(if lit { 200 } else { 50 }));
        if let Some(off) = meta_offset {
            sys.deliver(&video3(), t + off, seq, 0, frame(lit, lit));
        }
    }

    fn open(sys: &FakeSys, opts: OpenOptions) -> IrCapture<FakeSys> {
        IrCapture::open(&camera(true, true), &profile(), opts, sys.clone()).unwrap()
    }

    #[test]
    fn open_order_is_the_normative_one_and_timing_marks_are_taken() {
        let sys = fake();
        let t_before = sys.now();
        let cap = open(&sys, OpenOptions::default());
        let t = cap.timing();
        assert!(t.t0 >= t_before && t.t_open_done >= t.t0 && t.t_streamon_done >= t.t_open_done);
        assert_eq!(t.first_frame, None);
        assert!(cap.metadata_node_active());
        assert_eq!(cap.video_fd(), 10, "video opened first (lowest fd)");
        let log: Vec<String> = sys
            .log()
            .into_iter()
            .filter(|l| !l.starts_with("/dev/video0"))
            .collect();
        let mut expect: Vec<String> = vec![
            "/dev/video2: open".into(),
            "/dev/video2: VIDIOC_QUERYCAP".into(),
            "/dev/video3: open".into(),
            "/dev/video3: VIDIOC_QUERYCAP".into(),
            "/dev/video2: VIDIOC_G_FMT".into(),
            "/dev/video2: VIDIOC_S_FMT".into(),
            "/dev/video3: VIDIOC_G_FMT".into(),
            "/dev/video3: VIDIOC_S_FMT".into(),
            "/dev/video3: VIDIOC_REQBUFS 32".into(),
        ];
        for i in 0..32 {
            expect.push(format!("/dev/video3: VIDIOC_QUERYBUF {i}"));
            expect.push(format!("/dev/video3: mmap {i}"));
            expect.push(format!("/dev/video3: VIDIOC_QBUF {i}"));
        }
        expect.push("/dev/video3: VIDIOC_STREAMON".into());
        expect.push("/dev/video2: VIDIOC_REQBUFS 8".into());
        for i in 0..8 {
            expect.push(format!("/dev/video2: VIDIOC_QUERYBUF {i}"));
            expect.push(format!("/dev/video2: mmap {i}"));
            expect.push(format!("/dev/video2: VIDIOC_QBUF {i}"));
        }
        expect.push("/dev/video2: VIDIOC_STREAMON".into());
        assert_eq!(log, expect);
        assert_eq!(cap.rgb_assist(), RgbAssist::Disabled);
        drop(cap);
        // Teardown: both IR nodes STREAMOFF'd, unmapped and closed.
        for dev in [video2(), video3()] {
            let l = sys.log_for(&dev);
            assert!(l.contains(&"VIDIOC_STREAMOFF".to_string()), "{dev:?}");
            assert_eq!(l.last().unwrap(), "close");
        }
        assert_eq!(sys.open_fds(), 0);
        assert_eq!(sys.live_mappings(), 0);
    }

    #[test]
    fn pairs_normal_frames_and_takes_first_frame_and_first_lit_marks() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        // uvcvideo: meta completes just before the video buffer. First
        // delivered frame is lit with sequence 1, then strict alternation.
        for (i, seq) in (1..=6).enumerate() {
            let lit = seq % 2 == 1;
            schedule(&sys, 0.2 + i as f64 * FRAME_S, seq, lit, Some(-0.0005));
        }
        let mut got = Vec::new();
        while let Some(f) = cap.next(1.0).unwrap() {
            got.push(f);
        }
        assert_eq!(got.len(), 6);
        for (i, f) in got.iter().enumerate() {
            assert_eq!(f.sequence, i as u32 + 1);
            assert_eq!(f.meta.lit, Some(f.sequence % 2 == 1), "seq {}", f.sequence);
            assert_eq!(f.meta.fid, Some(f.sequence % 2 == 1));
            assert!(f.meta.present && !f.meta.buf_error && !f.meta.parse_error);
            assert_eq!(f.meta.raw, frame(f.sequence % 2 == 1, f.sequence % 2 == 1));
            assert!(f.usable());
            assert_eq!(f.label(), f.meta.lit);
            assert_eq!(f.bytesused, 230_400);
            assert_eq!((f.width, f.height), (640, 360));
            assert_eq!(f.pixels[0], if f.sequence % 2 == 1 { 200 } else { 50 });
            assert!((f.arrival - f.v4l2_ts).abs() < 1e-3);
        }
        let t = cap.timing();
        assert!((t.first_frame_ms().unwrap() - 200.0).abs() < 1.0, "{t:?}");
        assert_eq!(t.first_lit_ms(), t.first_frame_ms());
        assert!(t.open_ms() >= 0.0 && t.streamon_ms() >= t.open_ms());
        // Pruning: nothing in flight after each resolved frame.
        assert_eq!(cap.meta_pending(), 0);
        // Timeout without frames returns None after advancing the clock.
        let before = sys.now();
        assert!(cap.next(0.3).unwrap().is_none());
        assert!(sys.now() - before >= 0.3 - 1e-9);
    }

    #[test]
    fn meta_delayed_within_grace_is_labelled_beyond_grace_is_not() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        // seq 1: meta 20 ms late -> the 25 ms grace poll finds it.
        schedule(&sys, 0.2, 1, true, Some(0.020));
        // seq 2: meta 40 ms late -> unlabelled; its meta arrives later and
        // must be discarded (pruned) rather than attached to seq 3.
        schedule(&sys, 0.2 + FRAME_S, 2, false, Some(0.040));
        schedule(&sys, 0.2 + 2.0 * FRAME_S, 3, true, Some(-0.0005));
        let f1 = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f1.sequence, f1.meta.lit), (1, Some(true)));
        assert!(
            (f1.arrival - (1000.0 + 0.2)).abs() < 1e-6,
            "arrival is the DQBUF time"
        );
        let f2 = cap.next(1.0).unwrap().unwrap();
        assert_eq!(
            (f2.sequence, f2.meta.lit, f2.meta.present),
            (2, None, false)
        );
        assert!(f2.usable(), "unlabelled is not a buffer error");
        assert_eq!(f2.meta, MetaInfo::default());
        let f3 = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f3.sequence, f3.meta.lit), (3, Some(true)));
        assert_eq!(cap.meta_pending(), 0, "late meta for seq 2 was pruned");
        let t = cap.timing();
        assert_eq!(t.first_lit, Some(f1.arrival));
    }

    #[test]
    fn grace_poll_touches_only_the_meta_fd() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        schedule(&sys, 0.2, 1, true, Some(0.015));
        // A second video frame ready during the grace window must not be
        // consumed by it (and must not stop the clock either).
        schedule(&sys, 0.2 + 0.010, 2, false, Some(0.010));
        let f1 = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f1.sequence, f1.meta.lit), (1, Some(true)));
        assert!((f1.arrival - 1000.2).abs() < 1e-6);
        assert_eq!(sys.pending(&video2()), 1);
        let f2 = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f2.sequence, f2.meta.lit), (2, Some(false)));
        // The frame waited in the queue while the grace poll for seq 1 ran
        // to 0.215: arrival is the DQBUF time, not the buffer timestamp.
        assert!((f2.v4l2_ts - 1000.21).abs() < 1e-6, "{}", f2.v4l2_ts);
        assert!((f2.arrival - 1000.215).abs() < 1e-6, "{}", f2.arrival);
    }

    #[test]
    fn meta_arriving_early_for_several_frames_is_kept_until_used_then_pruned() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        // Meta for seq 1..3 all arrive before any video frame.
        for seq in 1..=3 {
            sys.deliver(&video3(), 0.1, seq, 0, frame(seq % 2 == 1, seq % 2 == 1));
        }
        // Plus a stale one for seq 0 that no video frame will ever match.
        sys.deliver(&video3(), 0.1, 0, 0, frame(false, false));
        sys.deliver(&video2(), 0.3, 1, 0, pixels(1));
        sys.deliver(&video2(), 0.3 + FRAME_S, 3, 0, pixels(3)); // seq 2 dropped by the kernel
        let f = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f.sequence, f.meta.lit), (1, Some(true)));
        assert_eq!(cap.meta_pending(), 2, "seq 0 and 1 pruned, 2 and 3 kept");
        let f = cap.next(1.0).unwrap().unwrap();
        assert_eq!((f.sequence, f.meta.lit), (3, Some(true)));
        assert_eq!(cap.meta_pending(), 0);
    }

    #[test]
    fn buffer_errors_are_flagged_and_never_labelled_from_a_flagged_meta() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        // Video buffer with V4L2_BUF_FLAG_ERROR.
        sys.deliver(&video2(), 0.2, 1, V4L2_BUF_FLAG_ERROR, pixels(9));
        sys.deliver(&video3(), 0.2, 1, 0, frame(true, true));
        // Short video buffer (nodrop=1), no flag.
        sys.deliver(&video2(), 0.2 + FRAME_S, 2, 0, vec![9u8; 1000]);
        sys.deliver(&video3(), 0.2 + FRAME_S, 2, 0, frame(false, false));
        // Meta buffer flagged: label unknown even though it parses.
        sys.deliver(&video2(), 0.2 + 2.0 * FRAME_S, 3, 0, pixels(9));
        sys.deliver(
            &video3(),
            0.2 + 2.0 * FRAME_S,
            3,
            V4L2_BUF_FLAG_ERROR,
            frame(true, true),
        );
        let f = cap.next(1.0).unwrap().unwrap();
        assert!(f.buf_error && !f.usable());
        assert_eq!(
            f.meta.lit,
            Some(true),
            "the label is still reported, the frame is not usable"
        );
        assert_eq!(
            cap.timing().first_lit,
            None,
            "a flagged frame never counts as first lit"
        );
        let f = cap.next(1.0).unwrap().unwrap();
        assert!(f.buf_error);
        assert_eq!(f.bytesused, 1000);
        assert_eq!(f.pixels.len(), 230_400);
        assert!(
            f.pixels[..1000].iter().all(|&p| p == 9) && f.pixels[1000..].iter().all(|&p| p == 0)
        );
        let f = cap.next(1.0).unwrap().unwrap();
        assert!(f.usable());
        assert!(f.meta.buf_error && f.meta.present);
        assert_eq!(f.meta.lit, None);
        assert_eq!(
            f.meta.fid,
            Some(true),
            "parsed fields other than the label stay"
        );
        assert_eq!(cap.timing().first_lit, None);
    }

    #[test]
    fn fid_mixed_first_buffer_is_handled_through_the_capture_path() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        let mut raw = Vec::new();
        block(&mut raw, 0x8C, &[]); // header-only block of the skipped dark slot, FID=0
        block(&mut raw, 0x8D, &item6(1)); // the lit frame, FID=1
        sys.deliver(&video2(), 0.2, 1, 0, pixels(1));
        sys.deliver(&video3(), 0.2, 1, 0, raw.clone());
        let f = cap.next(1.0).unwrap().unwrap();
        assert!(f.meta.fid_mixed);
        assert_eq!(f.meta.lit, Some(true));
        assert_eq!((f.meta.blocks, f.meta.items, f.meta.bytes), (2, 1, 60));
        assert!(!f.meta.parse_error);
        assert_eq!(f.meta.raw, raw);
        assert_eq!(cap.timing().first_lit, Some(f.arrival));
    }

    /// uvcvideo raises `EBUSY` at `S_FMT` (first privileged ioctl), not at
    /// `open(2)`: the retry unit is the whole open→S_FMT→STREAMON sequence,
    /// both nodes are dropped between attempts.
    #[test]
    fn ir_ebusy_at_sfmt_is_retried_three_times_then_camera_busy() {
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.sfmt_errors.extend([errno::EBUSY; 3]);
        sys.add_node(&video2(), n);
        let cap = open(&sys, OpenOptions::default());
        assert_eq!(sys.sleeps(), vec![150, 150, 150]);
        assert_eq!(sys.open_count(&video2()), 4, "4 attempts");
        assert_eq!(
            sys.open_count(&video3()),
            4,
            "the meta node is reopened with the video node on every attempt"
        );
        let l2 = sys.log_for(&video2());
        assert_eq!(l2.iter().filter(|l| *l == "VIDIOC_S_FMT").count(), 4);
        assert_eq!(l2.iter().filter(|l| *l == "close").count(), 3);
        assert_eq!(l2.iter().filter(|l| *l == "VIDIOC_STREAMON").count(), 1);
        assert!(
            !sys.log_for(&video3())
                .contains(&"VIDIOC_STREAMON".to_string())
                || sys
                    .log_for(&video3())
                    .iter()
                    .filter(|l| *l == "VIDIOC_STREAMON")
                    .count()
                    == 1
        );
        drop(cap);
        assert_eq!(sys.open_fds(), 0);

        // Four times busy: 3 sleeps, then camera_busy with nothing open.
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.sfmt_errors.extend([errno::EBUSY; 4]);
        sys.add_node(&video2(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(
            matches!(e, Error::Busy { ref dev } if dev == "/dev/video2"),
            "{e}"
        );
        assert_eq!(e.reason(), "camera_busy");
        assert_eq!(sys.sleeps(), vec![150, 150, 150]);
        assert_eq!(sys.open_fds(), 0);
        assert_eq!(sys.live_mappings(), 0);

        // Meta node busy at S_FMT (vb2_is_busy): same policy.
        let sys = fake();
        let mut n = FakeNode::ir_meta();
        n.sfmt_errors.extend([errno::EBUSY; 2]);
        sys.add_node(&video3(), n);
        let cap = open(&sys, OpenOptions::default());
        assert_eq!(sys.sleeps(), vec![150, 150]);
        assert_eq!(sys.open_count(&video2()), 3);
        drop(cap);

        // Busy at REQBUFS of the video node (after the meta STREAMON): the
        // meta stream of the failed attempt is STREAMOFF'd and closed.
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.reqbufs_errors.push_back(errno::EBUSY);
        sys.add_node(&video2(), n);
        let cap = open(&sys, OpenOptions::default());
        assert_eq!(sys.sleeps(), vec![150]);
        let l3 = sys.log_for(&video3());
        assert_eq!(l3.iter().filter(|l| *l == "VIDIOC_STREAMON").count(), 2);
        assert_eq!(l3.iter().filter(|l| *l == "VIDIOC_STREAMOFF").count(), 1);
        assert!(cap.metadata_node_active());
        drop(cap);
        assert_eq!(sys.open_fds(), 0);

        // Degenerate: EBUSY at open(2) is retried the same way.
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.open_errors.push_back(errno::EBUSY);
        sys.add_node(&video2(), n);
        let cap = open(&sys, OpenOptions::default());
        assert_eq!(sys.sleeps(), vec![150]);
        drop(cap);

        // A non-busy failure is not retried.
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.sfmt_errors.push_back(errno::EIO);
        sys.add_node(&video2(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(
            matches!(
                e,
                Error::Sys {
                    what: "VIDIOC_S_FMT",
                    ..
                }
            ),
            "{e}"
        );
        assert!(sys.sleeps().is_empty());
        assert_eq!(sys.open_fds(), 0);
    }

    #[test]
    fn open_is_pinned_to_the_discovered_node_identity() {
        // bus_info of another USB device: refused before any format/STREAMON.
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.bus_info = "usb-0000:00:14.0-4".into();
        sys.add_node(&video2(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Mismatch { .. }), "{e}");
        assert_eq!(e.reason(), "camera_mismatch");
        assert!(sys.sleeps().is_empty(), "a mismatch is never retried");
        assert!(!sys.log().iter().any(|l| l.contains("S_FMT")));
        assert_eq!(sys.open_fds(), 0);
        // The meta node renumbered underneath: refused too.
        let sys = fake();
        let mut n = FakeNode::ir_meta();
        n.rdev = (81, 13);
        sys.add_node(&video3(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(
            matches!(e, Error::Mismatch { ref found, .. } if found == &["81:13".to_string()]),
            "{e}"
        );
        assert_eq!(sys.open_fds(), 0);
        // A Camera without device numbers (nothing derivable): refused
        // before any open.
        let sys = fake();
        let mut cam = camera(true, false);
        cam.ir_cap.rdev = None;
        let e = IrCapture::open(&cam, &profile(), OpenOptions::default(), sys.clone()).unwrap_err();
        assert!(matches!(e, Error::Mismatch { .. }), "{e}");
        assert!(sys.log().is_empty());
        // The RGB node is pinned as well: a wrong identity is Failed
        // (mismatch), not Denied.
        let sys = fake();
        let mut n = FakeNode::rgb_video();
        n.bus_info = "usb-0000:00:14.0-4".into();
        sys.add_node(&video0(), n);
        let opts = OpenOptions {
            rgb: true,
            ..OpenOptions::default()
        };
        let mut cap = open(&sys, opts);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cap.rgb_assist() == RgbAssist::Pending && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        FrameSource::close(&mut cap);
        assert!(
            matches!(cap.rgb_assist(), RgbAssist::Failed(ref m) if m.contains("mismatch")),
            "{:?}",
            cap.rgb_assist()
        );
    }

    #[test]
    fn silent_format_change_fails_open_and_tears_down() {
        let sys = fake();
        let mut n = FakeNode::ir_video();
        n.sfmt_pix_override = Some((640, 480, crate::v4l2::V4L2_PIX_FMT_GREY));
        sys.add_node(&video2(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Format { .. }));
        assert_eq!(sys.open_fds(), 0);
        let sys = fake();
        let mut n = FakeNode::ir_meta();
        n.sfmt_meta_override = Some(crate::v4l2::V4L2_META_FMT_UVC);
        sys.add_node(&video3(), n);
        let e = IrCapture::open(
            &camera(true, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Format { ref got, .. } if got.contains("UVCH")));
        // Nothing was ever STREAMON'd, everything closed.
        assert!(
            !sys.log_for(&video3())
                .contains(&"VIDIOC_STREAMON".to_string())
        );
        assert_eq!(sys.open_fds(), 0);
    }

    #[test]
    fn missing_meta_node_is_refused_before_any_open() {
        let sys = fake();
        let e = IrCapture::open(
            &camera(false, false),
            &profile(),
            OpenOptions::default(),
            sys.clone(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::NoMetaNode { .. }), "{e}");
        assert_eq!(e.reason(), "camera_mismatch");
        assert!(sys.log().is_empty());
        let mut cam = camera(true, false);
        cam.ir_cap = Node::default();
        assert!(matches!(
            IrCapture::open(&cam, &profile(), OpenOptions::default(), sys.clone()),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn rgb_thread_streams_mjpg_1280x720_with_4_buffers_and_stops_on_drop() {
        let sys = fake();
        for i in 0..5 {
            sys.deliver(
                &video0(),
                0.1 + i as f64 / 30.0,
                i,
                if i == 2 { V4L2_BUF_FLAG_ERROR } else { 0 },
                vec![0xFF, 0xD8],
            );
        }
        schedule(&sys, 0.5, 1, true, Some(0.0));
        let opts = OpenOptions {
            rgb: true,
            ..OpenOptions::default()
        };
        let mut cap = open(&sys, opts);
        let f = cap.next(2.0).unwrap().unwrap();
        assert_eq!(f.sequence, 1);
        // Let the RGB thread consume everything scheduled.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cap.rgb_stats().is_none_or(|s| s.frames < 5) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(cap.rgb_assist(), RgbAssist::Streaming);
        FrameSource::close(&mut cap);
        let st = cap.rgb_stats().unwrap();
        assert!(st.frames >= 5, "final counters survive close: {st:?}");
        // Close order: the IR nodes were STREAMOFF'd (and closed) before the
        // RGB thread was joined; everything is down afterwards.
        for dev in [video2(), video3()] {
            assert_eq!(sys.log_for(&dev).last().unwrap(), "close", "{dev:?}");
        }
        assert_eq!(st.status, Some(RgbAssist::Streaming));
        assert_eq!(cap.rgb_assist(), RgbAssist::Streaming);
        let log = sys.log_for(&video0());
        assert_eq!(
            &log[..4],
            ["open", "VIDIOC_QUERYCAP", "VIDIOC_G_FMT", "VIDIOC_S_FMT"]
        );
        assert_eq!(log[4], "VIDIOC_REQBUFS 4");
        assert!(log.contains(&"VIDIOC_STREAMON".to_string()));
        assert!(log.contains(&"VIDIOC_STREAMOFF".to_string()));
        assert_eq!(log.last().unwrap(), "close");
        assert!(log.iter().filter(|l| *l == "VIDIOC_DQBUF").count() >= 5);
        assert_eq!(sys.open_fds(), 0);
        assert_eq!(sys.live_mappings(), 0);
        drop(cap);
    }

    #[test]
    fn rgb_stats_report_frames_errors_and_fps() {
        let sys = fake();
        for i in 0..4 {
            sys.deliver(
                &video0(),
                0.1 + i as f64 / 30.0,
                i,
                if i == 1 { V4L2_BUF_FLAG_ERROR } else { 0 },
                vec![1],
            );
        }
        let mut t = RgbThread::start(sys.clone(), &video0(), None, 1280, 720, V4L2_PIX_FMT_MJPEG);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while t.stats().frames < 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        t.stop();
        t.stop();
        let st = t.stats();
        assert_eq!((st.frames, st.buf_errors), (4, 1));
        assert_eq!(st.status, Some(RgbAssist::Streaming));
        assert!((st.fps().unwrap() - 30.0).abs() < 0.5, "{:?}", st.fps());
        assert_eq!(RgbStats::default().fps(), None);
        assert_eq!(RgbAssist::Denied.as_str(), "denied");
        assert_eq!(RgbAssist::Failed("x".into()).as_str(), "failed");
        assert_eq!(RgbAssist::NoNode.as_str(), "no_node");
        assert_eq!(RgbAssist::Pending.as_str(), "pending");

        // A thread that streams but never gets a frame is Failed once
        // stopped (fuprobe: `frames_ == 0` is a failed RGB condition).
        let sys = fake();
        let mut t = RgbThread::start(sys.clone(), &video0(), None, 1280, 720, V4L2_PIX_FMT_MJPEG);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while t.status() != Some(RgbAssist::Streaming) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(t.status(), Some(RgbAssist::Streaming), "provisional");
        t.stop();
        assert_eq!(t.status(), Some(RgbAssist::Failed("no RGB frames".into())));
        assert_eq!(t.stats().error, "no RGB frames");
    }

    #[test]
    fn rgb_open_is_skipped_once_stop_was_requested() {
        // `open_rgb` with the stop flag already set touches nothing: an
        // IrCapture::open that failed fast does not wake the RGB sensor.
        let sys = fake();
        let stop = AtomicBool::new(true);
        let r = RgbThread::open_rgb(&sys, &video0(), None, 1280, 720, V4L2_PIX_FMT_MJPEG, &stop)
            .unwrap();
        assert!(r.is_none());
        assert!(sys.log_for(&video0()).is_empty());
        // Not stopped: the full sequence runs.
        let stop = AtomicBool::new(false);
        let s = RgbThread::open_rgb(&sys, &video0(), None, 1280, 720, V4L2_PIX_FMT_MJPEG, &stop)
            .unwrap()
            .unwrap();
        assert!(s.streaming());
        drop(s);
        assert_eq!(sys.open_fds(), 0);
        // A thread whose IR sibling failed and which was stopped before
        // deciding reports Failed after the join, never Streaming.
        let sys = fake();
        let mut t = RgbThread::start(sys.clone(), &video0(), None, 1280, 720, V4L2_PIX_FMT_MJPEG);
        t.signal_stop();
        t.stop();
        // Whichever point the thread had reached (not opened, or streaming
        // with zero frames), the verdict is Failed and everything is closed.
        assert!(
            matches!(t.status(), Some(RgbAssist::Failed(_))),
            "{:?}",
            t.status()
        );
        assert_eq!(sys.open_fds(), 0);
    }

    /// `EBUSY` is classified by error type at every step (uvcvideo raises
    /// it at `S_FMT` for a node held by a browser), never by call site.
    #[test]
    fn rgb_ebusy_is_denied_and_ir_continues() {
        let opts = OpenOptions {
            rgb: true,
            ..OpenOptions::default()
        };
        for (step, knob) in [("S_FMT", 0u8), ("REQBUFS", 1), ("open", 2)] {
            let sys = fake();
            let mut n = FakeNode::rgb_video();
            match knob {
                0 => n.sfmt_errors.push_back(errno::EBUSY),
                1 => n.reqbufs_errors.push_back(errno::EBUSY),
                _ => n.open_errors.push_back(errno::EBUSY),
            }
            sys.add_node(&video0(), n);
            schedule(&sys, 0.2, 1, true, Some(0.0));
            let mut cap = open(&sys, opts);
            let f = cap.next(1.0).unwrap().unwrap();
            assert_eq!(f.meta.lit, Some(true));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while cap.rgb_assist() == RgbAssist::Pending && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(cap.rgb_assist(), RgbAssist::Denied, "EBUSY at {step}");
            assert_eq!(cap.rgb_assist().as_str(), "denied");
            FrameSource::close(&mut cap);
            assert_eq!(cap.rgb_assist(), RgbAssist::Denied, "final after close");
            assert_eq!(sys.open_count(&video0()), 1, "RGB is never retried");
            assert_eq!(sys.open_fds(), 0);
        }

        // Other RGB failures: Failed, IR still fine.
        let sys = fake();
        let mut n = FakeNode::rgb_video();
        n.sfmt_pix_override = Some((640, 480, V4L2_PIX_FMT_MJPEG));
        sys.add_node(&video0(), n);
        let cap = open(&sys, opts);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cap.rgb_assist() == RgbAssist::Pending && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(matches!(cap.rgb_assist(), RgbAssist::Failed(ref m) if m.contains("640x480")));
        // No RGB node discovered.
        let sys = fake();
        let cap = IrCapture::open(&camera(true, false), &profile(), opts, sys.clone()).unwrap();
        assert_eq!(cap.rgb_assist(), RgbAssist::NoNode);
        assert!(sys.log_for(&video0()).is_empty());
    }

    #[test]
    fn video_node_error_on_poll_is_reported() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        // Closing the fd behind the stream's back makes poll report NVAL.
        let fd = cap.video_fd();
        sys.clone().close(fd);
        let e = cap.next(0.5).unwrap_err();
        assert!(
            matches!(e, Error::Sys { what: "poll", ref dev, .. } if dev == "/dev/video2"),
            "{e}"
        );
        assert!(e.to_string().contains("device gone"));
        // Re-register so that the Drop teardown finds a valid fd.
        let _ = cap;
    }

    /// A dead metadata node fails closed at once instead of spinning until
    /// the deadline and delivering "unlabelled" frames.
    #[test]
    fn meta_node_error_on_poll_is_reported_not_spun_on() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        let fd = cap.meta_fd();
        sys.clone().close(fd);
        let before = sys.now();
        let e = cap.next(2.0).unwrap_err();
        assert!(
            matches!(e, Error::Sys { what: "poll", ref dev, .. } if dev == "/dev/video3"),
            "{e}"
        );
        assert!(e.to_string().contains("metadata node"));
        assert_eq!(e.reason(), "camera_error");
        assert!(sys.now() - before < 0.01, "returned immediately, no spin");
        // Same through the grace poll: video frame ready, meta fd dead.
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        sys.deliver(&video2(), 0.2, 1, 0, pixels(1));
        let fd = cap.meta_fd();
        // The main poll already reports NVAL on the meta fd; to reach the
        // grace poll the video must be ready first: deliver, then kill.
        let e = {
            // First call with a live meta fd returns the frame unlabelled
            // after the grace (nothing scheduled on meta).
            let f = cap.next(1.0).unwrap().unwrap();
            assert_eq!(f.meta.lit, None);
            sys.clone().close(fd);
            cap.next(1.0).unwrap_err()
        };
        assert!(matches!(e, Error::Sys { what: "poll", .. }), "{e}");
    }

    #[test]
    fn format_descriptions_and_node_paths() {
        let sys = fake();
        let mut cap = open(&sys, OpenOptions::default());
        let (v, m) = cap.format_descriptions();
        assert_eq!(v, "640x360 'GREY' sizeimage=230400");
        assert_eq!(m, "meta 'UVCM' buffersize=1024");
        let (a, b, c) = node_paths(&camera(true, true));
        assert_eq!((a, b, c), (video2(), video3(), video0()));
    }

    fn lf(seq: u32, lit: Option<bool>, usable: bool) -> IrFrame {
        IrFrame {
            sequence: seq,
            buf_error: !usable,
            meta: MetaInfo {
                present: lit.is_some(),
                lit,
                fid: lit,
                ..MetaInfo::default()
            },
            ..IrFrame::default()
        }
    }

    #[test]
    fn same_label_rule_fires_only_on_consecutive_sequences() {
        let mut r = LabelRules::default();
        assert_eq!(r.observe(&lf(1, Some(true), true), None), None);
        assert_eq!(r.observe(&lf(2, Some(false), true), None), None);
        // seq 3 dropped by the kernel: 4 lit after 2 dark is fine; 4 dark
        // after 2 dark would also be fine (gap).
        assert_eq!(r.observe(&lf(4, Some(false), true), None), None);
        assert_eq!(
            r.observe(&lf(5, Some(false), true), None),
            Some(Violation::SameLabelConsecutive {
                sequence: 5,
                label: false
            })
        );
        assert_eq!(r.same_label_at, vec![5]);
        assert_eq!(r.same_label_violations(), 1);
        // A truncated (unusable) frame still carries the firmware's label
        // and takes part: 6 lit-but-short, 7 dark -> fine.
        assert_eq!(r.observe(&lf(6, Some(true), false), None), None);
        assert_eq!(r.observe(&lf(7, Some(false), true), None), None);
        // Unlabelled frames do not participate.
        assert!(r.observe(&lf(8, None, true), None).is_none());
        assert_eq!(r.observe(&lf(9, Some(false), true), None), None);
        assert_eq!(
            (r.frames, r.usable, r.lit, r.dark, r.buf_errors),
            (8, 7, 1, 5, 1)
        );
        assert_eq!(r.unlabelled_usable, 1);
        let v = Violation::SameLabelConsecutive {
            sequence: 5,
            label: false,
        };
        assert_eq!(
            v.to_string(),
            "metadata: sequences 4 and 5 both labelled dark"
        );
        // FID shadow: fid == lit for every labelled frame here -> no
        // disagreement (the short frame 6 is compared too).
        assert_eq!((r.fid_compared, r.fid_disagree), (7, 0));

        // DESIGN §2.4 rule 3 says "delivered and labelled", not "usable":
        // lit(N) -> lit(N+1, short buffer) is a desync and fires; and the
        // short frame then anchors the check for N+2.
        let mut r = LabelRules::default();
        assert_eq!(r.observe(&lf(10, Some(true), true), None), None);
        assert_eq!(
            r.observe(&lf(11, Some(true), false), None),
            Some(Violation::SameLabelConsecutive {
                sequence: 11,
                label: true
            })
        );
        assert_eq!(
            r.observe(&lf(12, Some(true), true), None),
            Some(Violation::SameLabelConsecutive {
                sequence: 12,
                label: true
            })
        );
        assert_eq!((r.usable, r.buf_errors, r.lit), (2, 1, 2));
    }

    #[test]
    fn unlabelled_majority_rule_and_brightness_shadow() {
        let mut r = LabelRules::default();
        assert_eq!(
            r.observe(&lf(1, None, true), Some(10.0)),
            Some(Violation::TooManyUnlabelled {
                unlabelled: 1,
                usable: 1
            })
        );
        assert_eq!(r.observe(&lf(2, Some(true), true), Some(50.0)), None); // 1 of 2: not > 50 %
        assert_eq!(r.observe(&lf(3, Some(false), true), Some(20.0)), None);
        assert_eq!(
            r.observe(&lf(4, None, true), Some(20.0)),
            None,
            "2 of 4 is not > 50 %"
        );
        let v = r.observe(&lf(5, None, true), Some(20.0)).unwrap();
        assert_eq!(
            v,
            Violation::TooManyUnlabelled {
                unlabelled: 3,
                usable: 5
            }
        );
        assert_eq!(
            v.to_string(),
            "metadata: 3 of 5 usable frames carry no label"
        );
        // Unusable frames do not count towards the denominator.
        assert!(
            r.observe(&lf(6, None, false), None).is_none(),
            "unusable: no rule evaluated"
        );
        assert_eq!(r.usable, 5);
        // Brightness shadow: compared only when both the previous mean and
        // the label exist: seq 2 (50 > 10, lit: agree), seq 3 (20 < 50,
        // dark: agree).
        assert_eq!((r.bright_compared, r.bright_agree), (2, 2));
        let mut r = LabelRules::default();
        let mut f = lf(1, Some(true), true);
        f.meta.parse_error = true;
        f.meta.buf_error = true;
        r.observe(&f, None);
        assert_eq!((r.meta_parse_errors, r.meta_buf_errors), (1, 1));
        // FID shadow with an inverted but consistent relation: no
        // disagreement; a flip afterwards is one.
        let mut r = LabelRules::default();
        let mut f = lf(1, Some(true), true);
        f.meta.fid = Some(false);
        r.observe(&f, None);
        let mut f = lf(2, Some(false), true);
        f.meta.fid = Some(true);
        r.observe(&f, None);
        let f = lf(3, Some(true), true); // fid == lit now: disagrees with the relation
        r.observe(&f, None);
        assert_eq!((r.fid_compared, r.fid_disagree), (3, 1));
    }

    #[test]
    fn open_options_defaults_match_the_design() {
        let o = OpenOptions::default();
        assert_eq!((o.video_bufs, o.meta_bufs, o.rgb), (8, 32, false));
        assert_eq!((o.busy_retries, o.busy_retry_ms), (3, 150));
        assert_eq!(META_GRACE_MS, 25);
        assert_eq!(RGB_BUFS, 4);
        assert_eq!(RGB_POLL_MS, 20);
    }
}
