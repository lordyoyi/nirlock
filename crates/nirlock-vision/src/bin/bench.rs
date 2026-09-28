//! `nirlock-bench`: M0 measurement of the vision pipeline under ONNX Runtime.
//!
//! ```text
//! nirlock-bench --models DIR --frames GLOB [--template DIR] [--threads N]
//!               [--yunet-threads N] [--iters N] [--pace-ms N] [--ort PATH]
//!               [--all-frames] [--session NAME] [--dump DIR] [--out FILE.json]
//! ```
//!
//! Order of operations mirrors the daemon (ADR-0004): the three sessions are
//! loaded up front, in daemon order (YuNet with `--yunet-threads`, default 1
//! as ADR-0015 prescribes, then AuraFace, then SFace with `--threads`), and
//! only then does any frame flow. RSS is sampled after each load (the delta
//! is that model's footprint), after all three are resident, and at the end.
//!
//! Per model it prints load ms, the true cold first-inference ms (no warm-up
//! is run before it), steady median/p90 over `--iters` iterations (≥ 30;
//! even `n` uses the mean of the two middle values and p90 is nearest-rank,
//! exactly fuprobe's `summarise`), then the parity check: for every lit
//! frame, the max cosine of the Rust embedding against the stored template
//! rows and the cosine against the frame's **own** row (via `.src.tsv`).
//!
//! `--pace-ms N` sleeps so that consecutive steady iterations start ≥ N ms
//! apart (133 = the lit-frame cadence); the default 0 is a tight loop.
//! `--dump DIR` writes one `<session>.dump.jsonl` with per-frame candidates,
//! landmarks, the 2x3 matrix and both embeddings (biometric data: keep it
//! out of the repository) for the OpenCV comparison in `tools/bench-opencv`.
//!
//! No camera, no network: frames are PGM files; lit frames are selected by
//! `meta_lit` in the sibling `frames.jsonl` when it exists.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nirlock_vision::align::{self, Affine};
use nirlock_vision::embed::{Embedder, Kind};
use nirlock_vision::image::GrayImage;
use nirlock_vision::math::dot;
use nirlock_vision::template::Template;
use nirlock_vision::yunet::{DETECTOR_FLOOR, Detector, Face, REPORT_SCORE};
use nirlock_vision::{pgm, runtime};

const YUNET_FILE: &str = "face_detection_yunet_2026may.onnx";

struct Args {
    models: PathBuf,
    frames: String,
    template: Option<PathBuf>,
    threads: usize,
    yunet_threads: usize,
    iters: usize,
    pace_ms: u64,
    ort: Option<PathBuf>,
    all_frames: bool,
    session: Option<String>,
    dump: Option<PathBuf>,
    out: Option<PathBuf>,
}

fn usage() -> ! {
    eprintln!(
        "usage: nirlock-bench --models DIR --frames GLOB [--template DIR] [--threads N] [--yunet-threads N] \
         [--iters N] [--pace-ms N] [--ort PATH] [--all-frames] [--session NAME] [--dump DIR] [--out FILE.json]"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut a = Args {
        models: PathBuf::new(),
        frames: String::new(),
        template: None,
        threads: 4,
        yunet_threads: 1,
        iters: 30,
        pace_ms: 0,
        ort: None,
        all_frames: false,
        session: None,
        dump: None,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match k.as_str() {
            "--models" => a.models = PathBuf::from(val()),
            "--frames" => a.frames = val(),
            "--template" => a.template = Some(PathBuf::from(val())),
            "--threads" => a.threads = val().parse().unwrap_or_else(|_| usage()),
            "--yunet-threads" => a.yunet_threads = val().parse().unwrap_or_else(|_| usage()),
            "--iters" => a.iters = val().parse().unwrap_or_else(|_| usage()),
            "--pace-ms" => a.pace_ms = val().parse().unwrap_or_else(|_| usage()),
            "--ort" => a.ort = Some(PathBuf::from(val())),
            "--session" => a.session = Some(val()),
            "--dump" => a.dump = Some(PathBuf::from(val())),
            "--out" => a.out = Some(PathBuf::from(val())),
            "--all-frames" => a.all_frames = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    if a.models.as_os_str().is_empty() || a.frames.is_empty() {
        usage();
    }
    a
}

/// Minimal `*`/`?` matcher for the file-name part of `--frames`.
fn wildcard(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Expands `dir/pattern` (or a bare directory → `ir_*.pgm`) to sorted paths.
fn expand_frames(glob: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let p = Path::new(glob);
    let (dir, pat): (PathBuf, String) = if p.is_dir() {
        (p.to_path_buf(), "ir_*.pgm".into())
    } else {
        (
            p.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    };
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| wildcard(&pat, &n.to_string_lossy()))
        })
        .collect();
    v.sort();
    Ok((dir, v))
}

/// Per frame `file → (meta_lit, index)` from `frames.jsonl`, when present.
fn frame_map(dir: &Path) -> Option<HashMap<String, (bool, i32)>> {
    let text = std::fs::read_to_string(dir.join("frames.jsonl")).ok()?;
    let mut m = HashMap::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(f) = v.get("file").and_then(|f| f.as_str())
        {
            let lit = v.get("meta_lit").and_then(|l| l.as_bool()).unwrap_or(false);
            let idx = v
                .get("index")
                .and_then(|i| i.as_i64())
                .map(|i| i as i32)
                .unwrap_or(-1);
            m.insert(f.to_string(), (lit, idx));
        }
    }
    Some(m)
}

/// `ir_00014.pgm → 14` (the fuprobe frame index when there is no jsonl).
fn index_from_name(name: &str) -> i32 {
    name.trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(-1)
}

fn proc_status_kib(key: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn vm_hwm_kib() -> Option<u64> {
    proc_status_kib("VmHWM:")
}

fn vm_rss_kib() -> Option<u64> {
    proc_status_kib("VmRSS:")
}

fn sysfs(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn power_source() -> String {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else {
        return "unknown".into();
    };
    let mut found = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let online = std::fs::read_to_string(e.path().join("online")).ok();
        let kind = std::fs::read_to_string(e.path().join("type")).unwrap_or_default();
        if kind.trim() == "Mains" || name.starts_with("AC") {
            match online.map(|s| s.trim() == "1") {
                Some(true) => return format!("AC ({name} online)"),
                Some(false) => found.push(format!("{name} offline")),
                None => {}
            }
        }
    }
    if found.is_empty() {
        "unknown".into()
    } else {
        format!("battery ({})", found.join(", "))
    }
}

/// The CPU/power state that decides whether two runs are comparable
/// (finding: the platform profile flips on battery under Omarchy).
#[derive(serde::Serialize)]
struct Platform {
    power: String,
    scaling_governor: String,
    energy_performance_preference: String,
    platform_profile: String,
    cpus_allowed: String,
}

fn platform() -> Platform {
    let cpus_allowed = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Cpus_allowed_list:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".into());
    Platform {
        power: power_source(),
        scaling_governor: sysfs("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        energy_performance_preference: sysfs(
            "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference",
        ),
        platform_profile: sysfs("/sys/firmware/acpi/platform_profile"),
        cpus_allowed,
    }
}

#[derive(Default, serde::Serialize)]
struct Stats {
    n: usize,
    min_ms: f64,
    /// Mean of the two middle values for even `n` (fuprobe `summarise`).
    median_ms: f64,
    /// Nearest-rank: `v[min(n-1, ceil(0.9·n) - 1)]` (fuprobe `summarise`).
    p90_ms: f64,
    max_ms: f64,
    mean_ms: f64,
}

fn stats(mut v: Vec<f64>) -> Stats {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return Stats::default();
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median_ms = if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    };
    let p90_ms = v[((0.9 * n as f64).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1)];
    Stats {
        n,
        min_ms: v[0],
        median_ms,
        p90_ms,
        max_ms: v[n - 1],
        mean_ms: v.iter().sum::<f64>() / n as f64,
    }
}

#[derive(serde::Serialize)]
struct ModelReport {
    model: String,
    intra_threads: usize,
    load_ms: f64,
    /// True cold first inference (no warm-up before it).
    first_infer_ms: f64,
    steady: Stats,
    /// `VmRSS` right after this session was created, and the delta over the
    /// previous sample (this model's footprint in daemon load order).
    rss_after_load_kib: Option<u64>,
    rss_delta_kib: Option<i64>,
}

#[derive(serde::Serialize)]
struct FrameReport {
    file: String,
    index: i32,
    faces: usize,
    score: Option<f32>,
    min_side: Option<f32>,
    auraface_max_cos: Option<f64>,
    auraface_own_cos: Option<f64>,
    sface_max_cos: Option<f64>,
    sface_own_cos: Option<f64>,
}

#[derive(serde::Serialize)]
struct Memory {
    rss_after_loads_kib: Option<u64>,
    vm_hwm_after_loads_kib: Option<u64>,
    rss_end_kib: Option<u64>,
    vm_hwm_end_kib: Option<u64>,
    malloc_mmap_threshold_env: Option<String>,
}

#[derive(serde::Serialize)]
struct Report {
    ort_dylib: String,
    /// `OrtGetApiBase()->GetVersionString()`.
    ort_version: String,
    ort_build_info: String,
    threads: usize,
    yunet_threads: usize,
    iters: usize,
    pace_ms: u64,
    intra_op_spinning: bool,
    platform: Platform,
    session: String,
    frames_total: usize,
    frames_used: usize,
    memory: Memory,
    models: Vec<ModelReport>,
    frames: Vec<FrameReport>,
    parity: serde_json::Value,
}

/// One embedder's per-frame parity numbers.
#[derive(Default)]
struct Parity {
    max_cos: Vec<Option<f64>>,
    own_cos: Vec<Option<f64>>,
    embeddings: Vec<Option<Vec<f32>>>,
}

/// A steady-state loop: `iters` timed calls of `f`, paced to `pace` between
/// iteration starts (a tight loop when `pace` is zero).
fn steady<F: FnMut() -> Result<(), String>>(
    iters: usize,
    pace: Duration,
    mut f: F,
) -> Result<Vec<f64>, String> {
    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f()?;
        times.push(t.elapsed().as_secs_f64() * 1e3);
        if !pace.is_zero() {
            let elapsed = t.elapsed();
            if elapsed < pace {
                std::thread::sleep(pace - elapsed);
            }
        }
    }
    Ok(times)
}

fn mib(kib: Option<u64>) -> String {
    kib.map(|k| format!("{}", k / 1024))
        .unwrap_or_else(|| "?".into())
}

fn print_model(
    name: &str,
    threads: usize,
    load_ms: f64,
    first_ms: f64,
    s: &Stats,
    rss: Option<u64>,
    delta: Option<i64>,
) {
    println!(
        "{name:<9} intra {threads} | load {load_ms:7.1} ms | first {first_ms:7.1} ms | steady n={} median {:7.2} ms p90 {:7.2} ms min {:7.2} max {:7.2} | RSS after load {} MiB (Δ {} MiB)",
        s.n,
        s.median_ms,
        s.p90_ms,
        s.min_ms,
        s.max_ms,
        mib(rss),
        delta
            .map(|d| format!("{:+}", d / 1024))
            .unwrap_or_else(|| "?".into())
    );
}

fn fmt(v: &serde_json::Value) -> String {
    v.as_f64()
        .map(|f| format!("{f:.5}"))
        .unwrap_or_else(|| "-".into())
}

fn summarise(vals: &[Option<f64>]) -> serde_json::Value {
    let vals: Vec<f64> = vals.iter().flatten().copied().collect();
    if vals.is_empty() {
        return serde_json::json!({"n": 0});
    }
    let mut s = vals.clone();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    let median = if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    };
    serde_json::json!({
        "n": n,
        "min": s[0],
        "median": median,
        "max": s[n - 1],
        "frames_ge_0_95": vals.iter().filter(|&&c| c >= 0.95).count(),
        "frames_ge_0_99": vals.iter().filter(|&&c| c >= 0.99).count(),
    })
}

fn main() {
    if let Err(e) = run() {
        eprintln!("nirlock-bench: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args();
    let iters = args.iters.max(30);
    let pace = Duration::from_millis(args.pace_ms);
    let e = |e: nirlock_vision::Error| e.to_string();

    let dylib = runtime::init(args.ort.as_deref()).map_err(e)?;
    let ver = runtime::version().map_err(e)?;
    let plat = platform();
    println!(
        "ort       : {} (ONNX Runtime {}; {})",
        dylib.display(),
        ver.version,
        ver.build_info
    );
    println!(
        "power     : {} | governor {} | epp {} | platform_profile {} | cpus {}",
        plat.power,
        plat.scaling_governor,
        plat.energy_performance_preference,
        plat.platform_profile,
        plat.cpus_allowed
    );
    let opts = runtime::SessionOptions::threads(args.threads);
    println!(
        "threads   : embedders {} intra-op, YuNet {} intra-op (inter 1, sequential, intra-op spinning {}); pace {} ms",
        args.threads,
        args.yunet_threads,
        if opts.spinning { "on" } else { "off" },
        args.pace_ms
    );

    let (dir, all) = expand_frames(&args.frames)?;
    let session = args.session.clone().unwrap_or_else(|| {
        dir.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let fmap = frame_map(&dir);
    let name_of = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let frames: Vec<PathBuf> = all
        .iter()
        .filter(|p| {
            if args.all_frames {
                return true;
            }
            match &fmap {
                Some(m) => m.get(&name_of(p)).is_some_and(|(lit, _)| *lit),
                None => true,
            }
        })
        .cloned()
        .collect();
    let indices: Vec<i32> = frames
        .iter()
        .map(|p| {
            let n = name_of(p);
            fmap.as_ref()
                .and_then(|m| m.get(&n).map(|(_, i)| *i))
                .unwrap_or_else(|| index_from_name(&n))
        })
        .collect();
    println!(
        "frames    : {} of {} in {} ({}; session {session})",
        frames.len(),
        all.len(),
        dir.display(),
        if fmap.is_some() && !args.all_frames {
            "meta_lit=true only"
        } else {
            "all"
        }
    );
    if frames.is_empty() {
        return Err("no frames".into());
    }
    let images: Vec<GrayImage> = frames
        .iter()
        .map(|p| pgm::read(p).map_err(e))
        .collect::<Result<_, _>>()?;
    let (w, h) = (images[0].width(), images[0].height());
    println!("size      : {w}x{h}");

    // ---- Load everything first, in daemon order (ADR-0004 / ADR-0015).
    let rss0 = vm_rss_kib();
    let mut det = Detector::load(
        &args.models.join(YUNET_FILE),
        args.yunet_threads,
        DETECTOR_FLOOR,
    )
    .map_err(e)?;
    let rss_y = vm_rss_kib();
    let mut aura = Embedder::load(
        Kind::AuraFace,
        &args.models.join(Kind::AuraFace.file_name()),
        args.threads,
    )
    .map_err(e)?;
    let rss_a = vm_rss_kib();
    let mut sface = Embedder::load(
        Kind::SFace,
        &args.models.join(Kind::SFace.file_name()),
        args.threads,
    )
    .map_err(e)?;
    let rss_s = vm_rss_kib();
    let hwm_loads = vm_hwm_kib();
    let delta = |after: Option<u64>, before: Option<u64>| Some(after? as i64 - before? as i64);
    println!(
        "loaded    : yunet {:.0} ms, auraface {:.0} ms, sface {:.0} ms | RSS {} → {} → {} → {} MiB, VmHWM {} MiB",
        det.load_ms,
        aura.load_ms,
        sface.load_ms,
        mib(rss0),
        mib(rss_y),
        mib(rss_a),
        mib(rss_s),
        mib(hwm_loads)
    );
    println!();

    // ---- Cold first inference per model, then the steady loops.
    let t = Instant::now();
    let first_faces = det.detect(&images[0]).map_err(e)?;
    let y_first = t.elapsed().as_secs_f64() * 1e3;
    let mut ii = 0usize;
    let y_times = steady(iters, pace, || {
        let img = &images[ii % images.len()];
        ii += 1;
        det.detect(img).map(|_| ()).map_err(e)
    })?;
    let ys = stats(y_times);
    print_model(
        "yunet",
        args.yunet_threads,
        det.load_ms,
        y_first,
        &ys,
        rss_y,
        delta(rss_y, rss0),
    );
    println!(
        "            first frame: {} face(s){}",
        first_faces.len(),
        first_faces
            .first()
            .map(|f| format!(", best score {:.3} box {:.0}x{:.0}", f.score, f.w, f.h))
            .unwrap_or_default()
    );

    // Detect every frame once; keep the best face, its matrix and crop.
    struct PerFrame {
        face: Option<Face>,
        affine: Option<Affine>,
        crop: Option<GrayImage>,
    }
    let mut per_frame: Vec<PerFrame> = Vec::with_capacity(images.len());
    for img in &images {
        let faces = det.detect(img).map_err(e)?;
        let face = faces.into_iter().find(|f| f.score >= REPORT_SCORE);
        let affine = face.as_ref().map(align::face_to_arcface);
        let crop = match &face {
            Some(f) => match align::align(img, f) {
                Ok(c) => Some(c),
                // A singular alignment is a quality rejection, not a crash.
                Err(nirlock_vision::Error::Image(_)) => None,
                Err(err) => return Err(err.to_string()),
            },
            None => None,
        };
        per_frame.push(PerFrame { face, affine, crop });
    }
    let crops: Vec<&GrayImage> = per_frame.iter().filter_map(|p| p.crop.as_ref()).collect();
    if crops.is_empty() {
        return Err(format!(
            "no face ≥ {REPORT_SCORE} in any frame; cannot bench embedders"
        ));
    }

    let template = match args.template.as_deref() {
        Some(d) => Some(Template::load(d).map_err(e)?),
        None => None,
    };
    let mut models = vec![ModelReport {
        model: "yunet".into(),
        intra_threads: args.yunet_threads,
        load_ms: det.load_ms,
        first_infer_ms: y_first,
        steady: ys,
        rss_after_load_kib: rss_y,
        rss_delta_kib: delta(rss_y, rss0),
    }];
    let mut parity: HashMap<Kind, Parity> = HashMap::new();
    for (emb, rss, before) in [(&mut aura, rss_a, rss_y), (&mut sface, rss_s, rss_a)] {
        let kind = emb.kind();
        let t = Instant::now();
        emb.embed(crops[0]).map_err(e)?;
        let first = t.elapsed().as_secs_f64() * 1e3;
        let mut ci = 0usize;
        let times = steady(iters, pace, || {
            let c = crops[ci % crops.len()];
            ci += 1;
            emb.embed(c).map(|_| ()).map_err(e)
        })?;
        let s = stats(times);
        print_model(
            kind.name(),
            args.threads,
            emb.load_ms,
            first,
            &s,
            rss,
            delta(rss, before),
        );
        models.push(ModelReport {
            model: kind.name().into(),
            intra_threads: args.threads,
            load_ms: emb.load_ms,
            first_infer_ms: first,
            steady: s,
            rss_after_load_kib: rss,
            rss_delta_kib: delta(rss, before),
        });
        // Parity: every frame's crop vs the template's `lit` rows (max) and
        // vs the row fuprobe stored for this very frame (own).
        let set = template.as_ref().and_then(|t| t.find(kind.name(), "lit"));
        let mut p = Parity::default();
        for (pf, &idx) in per_frame.iter().zip(&indices) {
            let embedding = match &pf.crop {
                Some(c) => Some(emb.embed(c).map_err(e)?),
                None => None,
            };
            let (mx, own) = match (&embedding, set) {
                (Some(v), Some(set)) => (
                    set.max_cosine(v),
                    set.row_for(&session, idx).map(|r| dot(v, r)),
                ),
                _ => (None, None),
            };
            p.max_cos.push(mx);
            p.own_cos.push(own);
            p.embeddings.push(embedding);
        }
        parity.insert(kind, p);
    }
    let rss_end = vm_rss_kib();
    let hwm_end = vm_hwm_kib();

    // ---- Parity table
    let mut frame_reports = Vec::new();
    for (i, pf) in per_frame.iter().enumerate() {
        let get = |k: Kind, own: bool| {
            parity
                .get(&k)
                .and_then(|p| if own { p.own_cos[i] } else { p.max_cos[i] })
        };
        frame_reports.push(FrameReport {
            file: name_of(&frames[i]),
            index: indices[i],
            faces: usize::from(pf.face.is_some()),
            score: pf.face.map(|f| f.score),
            min_side: pf.face.map(|f| f.min_side()),
            auraface_max_cos: get(Kind::AuraFace, false),
            auraface_own_cos: get(Kind::AuraFace, true),
            sface_max_cos: get(Kind::SFace, false),
            sface_own_cos: get(Kind::SFace, true),
        });
    }
    let with_face = per_frame.iter().filter(|p| p.face.is_some()).count();
    let mut parity_json = serde_json::json!({
        "template": args.template.as_ref().map(|p| p.display().to_string()),
        "session": session,
        "frames_with_face": with_face,
        "frames_aligned": crops.len(),
    });
    println!();
    println!(
        "parity    : cosine vs template rows (lit variant); {with_face} frames with a face ≥ {REPORT_SCORE}, {} aligned",
        crops.len()
    );
    for kind in [Kind::AuraFace, Kind::SFace] {
        let Some(p) = parity.get(&kind) else { continue };
        let mx = summarise(&p.max_cos);
        let own = summarise(&p.own_cos);
        println!(
            "  {:<9} max-over-rows n={} min={} median={} max={} ≥0.95={} | own-row n={} min={} median={} ≥0.99={}",
            kind.name(),
            mx["n"],
            fmt(&mx["min"]),
            fmt(&mx["median"]),
            fmt(&mx["max"]),
            mx["frames_ge_0_95"],
            own["n"],
            fmt(&own["min"]),
            fmt(&own["median"]),
            own["frames_ge_0_99"],
        );
        parity_json[format!("{}_lit_max", kind.name())] = mx;
        parity_json[format!("{}_lit_own", kind.name())] = own;
    }
    println!(
        "memory    : RSS after loads {} MiB (VmHWM {}), at end with all three sessions resident {} MiB (VmHWM {})",
        mib(rss_s),
        mib(hwm_loads),
        mib(rss_end),
        mib(hwm_end)
    );

    // ---- Dump (biometric data; never inside the repository)
    if let Some(dump_dir) = &args.dump {
        std::fs::create_dir_all(dump_dir)
            .map_err(|err| format!("{}: {err}", dump_dir.display()))?;
        let path = dump_dir.join(format!("{session}.rust.dump.jsonl"));
        let mut text = String::new();
        for (i, pf) in per_frame.iter().enumerate() {
            let emb = |k: Kind| parity.get(&k).and_then(|p| p.embeddings[i].clone());
            let line = serde_json::json!({
                "file": name_of(&frames[i]),
                "index": indices[i],
                "face": pf.face.map(|f| serde_json::json!({
                    "x": f.x, "y": f.y, "w": f.w, "h": f.h, "score": f.score, "lm": f.lm,
                })),
                "affine": pf.affine.map(|a| a.0),
                "auraface": emb(Kind::AuraFace),
                "sface": emb(Kind::SFace),
            });
            let _ = writeln!(text, "{line}");
        }
        std::fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))?;
        println!("dump      : {}", path.display());
    }

    let report = Report {
        ort_dylib: dylib.display().to_string(),
        ort_version: ver.version,
        ort_build_info: ver.build_info,
        threads: args.threads,
        yunet_threads: args.yunet_threads,
        iters,
        pace_ms: args.pace_ms,
        intra_op_spinning: opts.spinning,
        platform: plat,
        session,
        frames_total: all.len(),
        frames_used: frames.len(),
        memory: Memory {
            rss_after_loads_kib: rss_s,
            vm_hwm_after_loads_kib: hwm_loads,
            rss_end_kib: rss_end,
            vm_hwm_end_kib: hwm_end,
            malloc_mmap_threshold_env: std::env::var("MALLOC_MMAP_THRESHOLD_").ok(),
        },
        models,
        frames: frame_reports,
        parity: parity_json,
    };
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?;
        std::fs::write(out, json).map_err(|err| format!("write {}: {err}", out.display()))?;
        println!("report    : {}", out.display());
    }
    Ok(())
}
