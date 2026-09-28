//! `nirlockctl record`: port of `cmd_record` (`phase0/main.cpp` 531–671)
//! on top of `nirlock-cam`.
//!
//! Outputs (same names and fields as fuprobe, so the lab's `enroll` /
//! `score` loaders read them unchanged): `ir_%05d.pgm`, `frames.jsonl`
//! (plus `meta_raw`, the whole UVCM buffer in hex, which fuprobe did not
//! keep and which provides the parser's real fixtures), `session.json`
//! (plus a `nirlock` object with the label-rule and shadow cross-check
//! counters). Privacy: the session directory must be under the lab's
//! `data/sessions` (refused anywhere else), created 0700 under `umask 077`,
//! files 0600.
//!
//! Deliberate deviations from `cmd_record`, both benign for the lab's
//! loaders (which read `frames.jsonl` and `session.json`, not the terminal
//! or the RGB dump):
//!
//! 1. **RGB frames are never written.** fuprobe saved `rgb_%05d.jpg` at
//!    ~5 fps plus `rgb.jsonl` (`main.cpp` 441-451); here the RGB node is
//!    only the exposure lever of ADR-0006, so its buffers are requeued
//!    without ever being decoded or stored. `session.json` keeps the
//!    `rgb` object (with `frames_saved: 0`) so the shape still parses.
//!    Not writing a second, colour, recognisable image of the user for
//!    every session is also the better privacy default.
//! 2. **The console summary is extended, not identical.** fuprobe's lines
//!    come first with the same numbers; the tail of the `open …` line
//!    carries first-frame/first-lit/rgb_assist instead of the MS extension
//!    unit's `face_auth GET_CUR`, which this crate cannot query at all
//!    (ADR-0005), and the `rgb` line adds the assist status.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use nirlock_cam::discover::usb_runtime_status;
use nirlock_cam::{
    FrameSource, IrCapture, LabelRules, OpenOptions, RealSys, RgbAssist, Violation,
    mono_now, select,
};

/// The only place frames may be written to (DESIGN §2.10, privacy rule of
/// the lab). Sessions land in `<root>/<UTC stamp>_<label>/`.
///
/// This used to be one developer's absolute home path compiled into the
/// binary. The guard it enforces is the point — recorded IR frames are
/// someone's face and must not be written wherever a flag says — but the
/// location is not, so it comes from the environment with a default under
/// the user's own data directory. The root must already exist: creating it
/// silently would turn a typo into a new place faces get written.
pub fn sessions_root() -> PathBuf {
    if let Ok(v) = std::env::var("NIRLOCK_SESSIONS_ROOT")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/nirlock/sessions")
}

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Session label, `[A-Za-z0-9_-]+`; the directory is `<stamp>_<label>`.
    #[arg(long)]
    pub label: String,
    /// Recording length in seconds (0, 600], measured from the video
    /// `STREAMON`.
    #[arg(long, default_value_t = 4.0)]
    pub seconds: f64,
    /// Start the RGB lever (MJPG 1280x720 on the RGB node) concurrently.
    #[arg(long)]
    pub rgb: bool,
    /// Parent directory of the session; must be under the sessions root
    /// (`NIRLOCK_SESSIONS_ROOT`, default `~/.local/share/nirlock/sessions`).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Free-text note stored in `session.json`.
    #[arg(long, default_value = "")]
    pub note: String,
}

#[derive(Debug)]
pub struct RecordError(String);

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for RecordError {
    fn from(e: std::io::Error) -> Self {
        RecordError(e.to_string())
    }
}

impl From<nirlock_cam::Error> for RecordError {
    fn from(e: nirlock_cam::Error) -> Self {
        RecordError(format!("{e} (unavailable {})", e.reason()))
    }
}

type Result<T> = std::result::Result<T, RecordError>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(RecordError(msg.into()))
}

// ---------------------------------------------------------------- helpers

/// `YYYYMMDD-HHMMSS` / `YYYY-MM-DDTHH:MM:SSZ` of now (UTC), without a
/// date crate (Howard Hinnant's civil-from-days).
fn utc_stamp(iso: bool) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    if iso {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    } else {
        format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
    }
}

fn read_attr(p: &Path) -> String {
    nirlock_cam::discover::read_attr(p)
}

fn kernel_release() -> String {
    read_attr(Path::new("/proc/sys/kernel/osrelease"))
}

/// 1 = on AC, 0 = on battery, None = unknown (Mains or USB-C source online).
fn ac_online() -> Option<bool> {
    let mut out = None;
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else {
        return None;
    };
    for e in rd.flatten() {
        let t = read_attr(&e.path().join("type"));
        if t != "Mains" && t != "USB" {
            continue;
        }
        match read_attr(&e.path().join("online")).as_str() {
            "1" => return Some(true),
            "" => {}
            _ => out = Some(false),
        }
    }
    out
}

/// The `als` IIO device, `(raw + offset) * scale`.
fn als_lux() -> Option<f64> {
    let rd = std::fs::read_dir("/sys/bus/iio/devices").ok()?;
    for e in rd.flatten() {
        let d = e.path();
        if read_attr(&d.join("name")) != "als" {
            continue;
        }
        let raw: f64 = read_attr(&d.join("in_illuminance_raw")).parse().ok()?;
        let scale: f64 = read_attr(&d.join("in_illuminance_scale"))
            .parse()
            .unwrap_or(1.0);
        let off: f64 = read_attr(&d.join("in_illuminance_offset"))
            .parse()
            .unwrap_or(0.0);
        return Some((raw + off) * scale);
    }
    None
}

fn jstr(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn jopt(b: Option<bool>) -> &'static str {
    match b {
        None => "null",
        Some(true) => "true",
        Some(false) => "false",
    }
}

fn jnum(v: Option<f64>) -> String {
    v.map_or_else(|| "null".into(), |v| format!("{v:.3}"))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// `frame_stats` of `phase0/pipeline.cpp`: mean, p99 (first value whose
/// cumulative count reaches 99 %), fraction of pixels >= 250.
struct FrameStats {
    mean: f64,
    p99: u32,
    sat_frac: f64,
}

fn frame_stats(px: &[u8]) -> FrameStats {
    let mut hist = [0u64; 256];
    let mut sum = 0u64;
    for &p in px {
        hist[p as usize] += 1;
        sum += u64::from(p);
    }
    let total = px.len() as u64;
    if total == 0 {
        return FrameStats {
            mean: 0.0,
            p99: 0,
            sat_frac: 0.0,
        };
    }
    let mut acc = 0u64;
    let mut p99 = 0u32;
    for (i, h) in hist.iter().enumerate() {
        acc += h;
        if acc as f64 >= 0.99 * total as f64 {
            p99 = i as u32;
            break;
        }
    }
    let sat: u64 = hist[250..].iter().sum();
    FrameStats {
        mean: sum as f64 / total as f64,
        p99,
        sat_frac: sat as f64 / total as f64,
    }
}

fn private_file(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)
}

/// Every write on the capture path is checked: on a full disk a session
/// must fail loudly now, not as a truncated PGM in a later enroll.
fn write_pgm(p: &Path, px: &[u8], w: u32, h: u32) -> Result<()> {
    let r = (|| -> std::io::Result<()> {
        let mut f = private_file(p)?;
        f.write_all(format!("P5\n{w} {h}\n255\n").as_bytes())?;
        f.write_all(px)?;
        // No per-frame fsync: fuprobe's write_file_checked (`main.cpp`
        // 230-241) only checked fwrite/fclose, and an fdatasync of 230 KB
        // fifteen times a second can lag the 66 ms capture loop, which would
        // skew the very arrival timings this tool exists to compare. The
        // directory is synced once when the session closes.
        Ok(())
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(p); // do not leave a truncated frame behind
        return err(format!("write failed for {}: {e}", p.display()));
    }
    Ok(())
}

/// The session parent must resolve under [`sessions_root`].
fn require_under_sessions(out: &Path) -> Result<PathBuf> {
    let root = sessions_root();
    if !root.is_dir() {
        return err(format!(
            "sessions root {} does not exist; create it, or set NIRLOCK_SESSIONS_ROOT. \
             This tool only records into that directory: the frames are a face.",
            root.display()
        ));
    }
    let root = std::fs::canonicalize(root)?;
    let abs = std::fs::canonicalize(out)
        .map_err(|e| RecordError(format!("--out {}: {e}", out.display())))?;
    if !abs.starts_with(&root) {
        return err(format!(
            "refusing to record outside {}: {}",
            root.display(),
            abs.display()
        ));
    }
    Ok(abs)
}

// ---------------------------------------------------------------- record

pub fn run(a: &Args) -> Result<()> {
    if a.label.is_empty()
        || !a
            .label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return err("--label may only contain [A-Za-z0-9_-]");
    }
    if !(a.seconds > 0.0 && a.seconds <= 600.0) {
        return err("--seconds must be in (0, 600]");
    }
    nirlock_cam::sys::umask_private();
    let out = require_under_sessions(a.out.as_deref().unwrap_or(&sessions_root()))?;

    let (cam, profile, _) = select(None)?;

    let dir = out.join(format!("{}_{}", utc_stamp(false), a.label));
    if dir.exists() {
        return err(format!(
            "session directory already exists: {}",
            dir.display()
        ));
    }
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    }

    let als0 = als_lux();
    let started = utc_stamp(true);
    let status0 = usb_runtime_status(&cam);

    let opts = OpenOptions {
        rgb: a.rgb,
        ..OpenOptions::default()
    };
    let mut cap = match IrCapture::open(&cam, &profile, opts, RealSys) {
        Ok(c) => c,
        Err(e) => {
            // EBUSY & co: do not leave an empty session directory behind.
            let _ = std::fs::remove_dir(&dir);
            return err(format!("{e} (unavailable {})", e.reason()));
        }
    };
    let mut jl = std::io::BufWriter::new(private_file(&dir.join("frames.jsonl"))?);
    eprintln!("recording {:.1} s into {}", a.seconds, dir.display());

    let mut rules = LabelRules::default();
    let mut n = 0u32;
    let mut rule2_seen = false;
    let mut dropped: Vec<u32> = Vec::new();
    let mut prev_seq: Option<u32> = None;
    let mut prev_mean: Option<f64> = None;
    let mut first_label: Option<bool> = None;
    let mut first_seq: Option<u32> = None;
    let mut violations: Vec<String> = Vec::new();
    let mut write_error = String::new();
    let mut capture_error = String::new();
    let t_streamon = cap.timing().t_streamon_done;
    while mono_now() - t_streamon < a.seconds {
        let f = match cap.next(1.0) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => {
                capture_error = format!("{e} (unavailable {})", e.reason());
                break;
            }
        };
        // Mid-stream drops only. Sequence slots BEFORE the first delivered
        // frame are reported separately (frames_before_first_delivery).
        if let Some(p) = prev_seq {
            dropped.extend(p + 1..f.sequence);
        }
        prev_seq = Some(f.sequence);
        let st = frame_stats(&f.pixels);
        let name = format!("ir_{n:05}.pgm");
        if let Err(e) = write_pgm(&dir.join(&name), &f.pixels, f.width, f.height) {
            write_error = e.to_string();
            break;
        }

        // Brightness heuristic, logged for comparison ONLY. The label of
        // record is meta_lit; this field must never fill in a missing one.
        let guess = match prev_mean {
            Some(pm) if f.usable() => Some(st.mean > pm),
            _ => None,
        };
        if f.usable() {
            prev_mean = Some(st.mean);
        }
        if n == 0 {
            first_label = f.meta.lit;
            first_seq = Some(f.sequence);
        }
        if let Some(v) = rules.observe(&f, f.usable().then_some(st.mean)) {
            // Rule 2 keeps holding for every later frame once the unlabelled
            // majority tips; the daemon ends the request on the first
            // violation, but `record` runs to the end, so report it once.
            // Rule 3 is per offending pair and is always kept.
            let rule2 = matches!(v, Violation::TooManyUnlabelled { .. });
            if !rule2 || !rule2_seen {
                rule2_seen |= rule2;
                violations.push(format!("seq {}: {v}", f.sequence));
            }
        }

        let t = cap.timing();
        let mut line = String::new();
        let _ = write!(
            line,
            "{{\"index\":{n},\"file\":\"{name}\",\"seq\":{},\"v4l2_ts\":{:.6},\"arrival_mono\":{:.6},\"ms_since_open\":{:.1},",
            f.sequence,
            f.v4l2_ts,
            f.arrival,
            t.rel_ms(f.arrival)
        );
        let _ = write!(
            line,
            "\"meta_present\":{},\"meta_lit\":{},\"label_source\":{},\"fid\":{},",
            f.meta.present,
            jopt(f.meta.lit),
            if f.meta.lit.is_some() {
                "\"metadata\""
            } else {
                "\"none\""
            },
            jopt(f.meta.fid)
        );
        let _ = write!(
            line,
            "\"fid_mixed\":{},\"meta_blocks\":{},\"meta_items\":{},\"meta_bytes\":{},\"meta_buf_error\":{},\"meta_parse_error\":{},\"buf_error\":{},\"bytesused\":{},",
            f.meta.fid_mixed,
            f.meta.blocks,
            f.meta.items,
            f.meta.bytes,
            f.meta.buf_error,
            f.meta.parse_error,
            f.buf_error,
            f.bytesused
        );
        let _ = write!(
            line,
            "\"mean\":{:.2},\"p99\":{},\"sat_frac\":{:.5},\"brightness_guess_lit\":{},\"meta_raw\":\"{}\"}}",
            st.mean,
            st.p99,
            st.sat_frac,
            jopt(guess),
            hex(&f.meta.raw)
        );
        line.push('\n');
        if let Err(e) = jl.write_all(line.as_bytes()) {
            write_error = format!("frames.jsonl: {e}");
            break;
        }
        n += 1;
    }
    let (fmt_video, fmt_meta) = cap.format_descriptions();
    FrameSource::close(&mut cap);
    let timing = cap.timing();
    let rgb_status = cap.rgb_assist();
    let rgb_stats = cap.rgb_stats();

    let als1 = als_lux();
    let mut sj = String::new();
    let _ = writeln!(sj, "{{");
    let _ = writeln!(sj, "  \"label\": {},", jstr(&a.label));
    let _ = writeln!(sj, "  \"note\": {},", jstr(&a.note));
    let _ = writeln!(sj, "  \"started_utc\": {},", jstr(&started));
    let _ = writeln!(sj, "  \"ended_utc\": {},", jstr(&utc_stamp(true)));
    let _ = writeln!(sj, "  \"interrupted\": false,");
    let _ = writeln!(sj, "  \"write_error\": {},", jstr(&write_error));
    let _ = writeln!(sj, "  \"kernel\": {},", jstr(&kernel_release()));
    let _ = writeln!(sj, "  \"bcdDevice\": {},", jstr(&cam.bcd_device));
    let _ = writeln!(
        sj,
        "  \"uvcvideo_nodrop\": {},",
        jstr(&read_attr(Path::new(
            "/sys/module/uvcvideo/parameters/nodrop"
        )))
    );
    let _ = writeln!(sj, "  \"ac_online\": {},", jopt(ac_online()));
    let _ = writeln!(
        sj,
        "  \"ir_node\": {}, \"ir_meta_node\": {},",
        jstr(&cam.ir_cap.dev.display().to_string()),
        jstr(&cam.ir_meta.dev.display().to_string())
    );
    let _ = writeln!(
        sj,
        "  \"usb_runtime_status_before_open\": {},",
        jstr(&status0)
    );
    let _ = writeln!(
        sj,
        "  \"als_lux_start\": {}, \"als_lux_end\": {},",
        jnum(als0),
        jnum(als1)
    );
    // fuprobe queried the MS extension unit here; the daemon-side crate has
    // no control-query path at all (ADR-0005), so the field says so.
    let _ = writeln!(sj, "  \"face_auth_get_cur\": \"not queried (ADR-0005)\",");
    let _ = writeln!(
        sj,
        "  \"open_ms\": {:.1}, \"streamon_done_ms\": {:.1},",
        timing.open_ms(),
        timing.streamon_ms()
    );
    let _ = writeln!(
        sj,
        "  \"frames\": {}, \"lit\": {}, \"dark\": {}, \"missing_metadata\": {}, \"buffer_errors\": {}, \"meta_buffer_errors\": {}, \"meta_parse_errors\": {},",
        rules.frames,
        // fuprobe's semantics (`main.cpp` 594-596): lit/dark over every
        // delivered LABELLED frame, usable or not, so a truncated frame with
        // a valid label is not reported as lost metadata. `rules.frames` (not
        // the saved-frame counter `n`) is the population these were counted
        // over; using `n` could underflow if a write error ends the loop
        // between `observe()` and `n += 1`.
        rules.lit_all,
        rules.dark_all,
        rules.frames - rules.lit_all - rules.dark_all,
        rules.buf_errors,
        rules.meta_buf_errors,
        rules.meta_parse_errors
    );
    let _ = writeln!(sj, "  \"label_source\": \"metadata\",");
    let _ = writeln!(sj, "  \"first_frame_meta_lit\": {},", jopt(first_label));
    // uvcvideo starts every stream at sequence -1 and increments on each FID
    // toggle, so the first frame it SEES is 0. first_sequence = k > 0 means
    // k frame slots (~67 ms each) went by after STREAMON without a delivered
    // buffer. They are not mid-stream drops and are reported separately.
    let fs = first_seq.map_or("null".to_string(), |s| s.to_string());
    let _ = writeln!(
        sj,
        "  \"first_sequence\": {fs}, \"frames_before_first_delivery\": {fs},"
    );
    let _ = writeln!(
        sj,
        "  \"brightness_guess_agreement\": {{\"agree\": {}, \"compared\": {}}},",
        rules.bright_agree, rules.bright_compared
    );
    let _ = writeln!(
        sj,
        "  \"dropped_sequence_numbers\": [{}],",
        dropped
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let (rgb_frames, rgb_errs, rgb_fps, rgb_err) = match &rgb_stats {
        // `main.cpp` 399 wrote "RGB node not found" rather than an empty
        // string when the lever had no node to open; keep that text so the
        // field distinguishes "not asked for" from "asked for, unavailable".
        Some(s) if s.error.is_empty() && s.status == Some(RgbAssist::NoNode) => (
            s.frames,
            s.buf_errors,
            s.fps(),
            "RGB node not found".to_string(),
        ),
        Some(s) => (s.frames, s.buf_errors, s.fps(), s.error.clone()),
        None => (0, 0, None, String::new()),
    };
    let _ = writeln!(
        sj,
        "  \"rgb\": {{\"enabled\": {}, \"frames_streamed\": {rgb_frames}, \"frames_saved\": 0, \"buffer_errors\": {rgb_errs}, \"fps\": {}, \"error\": {}}},",
        a.rgb,
        jnum(rgb_fps),
        jstr(&rgb_err)
    );
    // Additions over fuprobe (all under one key, so the lab's loaders
    // ignore them).
    let _ = writeln!(sj, "  \"nirlock\": {{");
    let _ = writeln!(
        sj,
        "    \"tool\": \"nirlockctl record {}\",",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(sj, "    \"profile\": {},", jstr(&profile.id));
    let _ = writeln!(
        sj,
        "    \"usb_sysfs\": {},",
        jstr(&cam.usb_sysfs.display().to_string())
    );
    let _ = writeln!(
        sj,
        "    \"video_format\": {}, \"meta_format\": {},",
        jstr(&fmt_video),
        jstr(&fmt_meta)
    );
    let _ = writeln!(
        sj,
        "    \"first_frame_ms\": {}, \"first_lit_ms\": {},",
        jnum(timing.first_frame_ms()),
        jnum(timing.first_lit_ms())
    );
    let _ = writeln!(
        sj,
        "    \"rgb_assist\": {}, \"rgb_first_frame_ms\": {},",
        jstr(rgb_status.as_str()),
        jnum(
            rgb_stats
                .as_ref()
                .and_then(|s| s.first_frame)
                .map(|t| timing.rel_ms(t))
        )
    );
    let _ = writeln!(
        sj,
        "    \"usable\": {}, \"unlabelled_usable\": {}, \"same_label_consecutive_at\": [{}],",
        rules.usable,
        rules.unlabelled_usable,
        rules
            .same_label_at
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let _ = writeln!(
        sj,
        "    \"fid_parity\": {{\"compared\": {}, \"disagree\": {}}},",
        rules.fid_compared, rules.fid_disagree
    );
    let _ = writeln!(
        sj,
        "    \"violations\": [{}],",
        violations
            .iter()
            .map(|v| jstr(v))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(sj, "    \"capture_error\": {}", jstr(&capture_error));
    let _ = writeln!(sj, "  }}");
    let _ = writeln!(sj, "}}");
    {
        let mut f = private_file(&dir.join("session.json"))?;
        f.write_all(sj.as_bytes())?;
        f.sync_data()?;
    }
    jl.flush()?;
    jl.into_inner()
        .map_err(|e| RecordError(e.to_string()))?
        .sync_data()?;
    // One directory sync instead of an fsync per frame (see `write_pgm`):
    // the frame files' contents are already on their way out, and this is
    // what makes their names durable.
    if let Ok(d) = std::fs::File::open(&dir) {
        let _ = d.sync_all();
    }

    println!("session           {}", dir.display());
    println!(
        "frames            {}  (lit {}, dark {}, missing metadata {}, buffer errors {}, metadata errors {})",
        rules.frames,
        rules.lit_all,
        rules.dark_all,
        rules.frames - rules.lit_all - rules.dark_all,
        rules.buf_errors,
        rules.meta_buf_errors + rules.meta_parse_errors
    );
    println!(
        "first frame       meta_lit={}  sequence={} (= frame slots that passed before the first delivered buffer)",
        jopt(first_label),
        first_seq.map_or("-".to_string(), |s| s.to_string())
    );
    println!("dropped seq       {} (mid-stream)", dropped.len());
    println!(
        "brightness guess  agrees with metadata on {}/{} frames (comparison only)",
        rules.bright_agree, rules.bright_compared
    );
    println!(
        "open {:.0} ms, STREAMON done {:.0} ms, first frame {} ms, first lit {} ms after t0; rgb_assist={}",
        timing.open_ms(),
        timing.streamon_ms(),
        timing
            .first_frame_ms()
            .map_or("-".to_string(), |v| format!("{v:.0}")),
        timing
            .first_lit_ms()
            .map_or("-".to_string(), |v| format!("{v:.0}")),
        rgb_status.as_str()
    );
    if let Some(s) = &rgb_stats {
        println!(
            "rgb               {} frames, {} buffer errors, fps {}",
            s.frames,
            s.buf_errors,
            s.fps().map_or("-".to_string(), |v| format!("{v:.1}"))
        );
    }
    println!("ALS               {} -> {} lux", jnum(als0), jnum(als1));
    if !violations.is_empty() {
        println!(
            "label rules       {} violation(s): {}",
            violations.len(),
            violations.join("; ")
        );
    }
    if !capture_error.is_empty() {
        println!("capture error     {capture_error}");
    }
    if !write_error.is_empty() {
        return err(write_error);
    }
    if rgb_status == RgbAssist::Denied {
        eprintln!("note: the RGB node was busy; recorded IR-only (rgb_assist=denied)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_well_formed() {
        let s = utc_stamp(false);
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
        let i = utc_stamp(true);
        assert_eq!(i.len(), 20);
        assert!(i.ends_with('Z') && &i[10..11] == "T");
        assert!(s.starts_with("20"));
    }

    #[test]
    fn frame_stats_match_the_cpp_definition() {
        let mut px = vec![10u8; 100];
        px[0] = 255;
        px[1] = 250;
        let s = frame_stats(&px);
        assert!((s.mean - (98.0 * 10.0 + 505.0) / 100.0).abs() < 1e-9);
        assert_eq!(s.p99, 250);
        assert!((s.sat_frac - 0.02).abs() < 1e-9);
        let s = frame_stats(&[]);
        assert_eq!((s.mean, s.p99, s.sat_frac), (0.0, 0, 0.0));
    }

    #[test]
    fn json_helpers() {
        assert_eq!(jstr("a\"b\n"), "\"a\\\"b\\n\"");
        assert_eq!(jopt(None), "null");
        assert_eq!(jnum(Some(1.23456)), "1.235");
        assert_eq!(jnum(None), "null");
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }

    #[test]
    fn refuses_paths_outside_the_sessions_root() {
        let e = require_under_sessions(Path::new("/tmp")).unwrap_err();
        assert!(
            e.to_string().contains("refusing to record outside")
                || e.to_string().contains("does not exist"),
            "{e}"
        );
        assert!(require_under_sessions(Path::new("/nonexistent/x")).is_err());
    }
}
