//! `nirlock-replay`: runs a recorded session through the daemon's own
//! pipeline (detect → gate → align → embed → cosine) and writes the columns
//! `fuprobe score --csv` writes, so the two can be diffed row by row. This
//! is the M2 acceptance evidence of DESIGN §10: the Rust pipeline must
//! accept and reject the same frames and produce the same scores as the
//! C++ reference every number in `PHASE0-RESULTS.md` came from.
//!
//! usage: nirlock-replay --session DIR --template DIR --models DIR
//!                       --csv OUT [--ort PATH] [--threads N]
//!
//! No camera is opened and nothing is written outside `--csv`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use nirlock_vision::embed::{Embedder, Kind};
use nirlock_vision::gate::{GateConfig, Reject, frame_stats, quality_gate};
use nirlock_vision::image::{GrayImage, lit_minus_dark};
use nirlock_vision::template::Template;
use nirlock_vision::yunet::{Detector, Face};
use nirlock_vision::{pgm, runtime};

/// fuprobe's detector floor and "a face was reported" threshold.
const DETECTOR_FLOOR: f32 = 0.30;
const FACE_REPORT_SCORE: f32 = 0.60;

struct Args {
    session: PathBuf,
    template: PathBuf,
    models: PathBuf,
    csv: PathBuf,
    ort: Option<PathBuf>,
    threads: usize,
}

fn usage() -> ! {
    eprintln!(
        "usage: nirlock-replay --session DIR --template DIR --models DIR --csv OUT \
         [--ort PATH] [--threads N]"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut a = Args {
        session: PathBuf::new(),
        template: PathBuf::new(),
        models: PathBuf::from("models"),
        csv: PathBuf::new(),
        ort: None,
        threads: 4,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match k.as_str() {
            "--session" => a.session = PathBuf::from(val()),
            "--template" => a.template = PathBuf::from(val()),
            "--models" => a.models = PathBuf::from(val()),
            "--csv" => a.csv = PathBuf::from(val()),
            "--ort" => a.ort = Some(PathBuf::from(val())),
            "--threads" => a.threads = val().parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    if a.session.as_os_str().is_empty()
        || a.template.as_os_str().is_empty()
        || a.csv.as_os_str().is_empty()
    {
        usage();
    }
    a
}

/// One line of `frames.jsonl`, only the fields the replay needs.
struct Row {
    index: i32,
    file: String,
    seq: u32,
    lit: Option<bool>,
    buf_error: bool,
}

/// Minimal extractor for the flat, machine-written objects of
/// `frames.jsonl` (no nesting, no escapes in the values we read).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let start = line.find(&pat)? + pat.len();
    let rest = line[start..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"'))
}

fn read_rows(session: &Path) -> Vec<Row> {
    let path = session.join("frames.jsonl");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Row {
            index: field(l, "index").and_then(|v| v.parse().ok()).unwrap_or(-1),
            file: field(l, "file").unwrap_or_default().to_string(),
            seq: field(l, "seq").and_then(|v| v.parse().ok()).unwrap_or(0),
            lit: match field(l, "meta_lit") {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            },
            buf_error: field(l, "buf_error") == Some("true"),
        })
        .collect()
}

fn fail(msg: &str) -> ! {
    eprintln!("nirlock-replay: {msg}");
    std::process::exit(1)
}

fn main() {
    let a = parse_args();
    if let Err(e) = runtime::init(a.ort.as_deref()) {
        fail(&format!("ONNX Runtime: {e}"));
    }

    let session_name = a
        .session
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmpl = Template::load(&a.template).unwrap_or_else(|e| fail(&e.to_string()));
    let mut det = Detector::load(
        &a.models.join("face_detection_yunet_2026may.onnx"),
        1,
        DETECTOR_FLOOR,
    )
    .unwrap_or_else(|e| fail(&e.to_string()));
    let mut emb: Vec<Embedder> = [Kind::SFace, Kind::AuraFace]
        .into_iter()
        .map(|k| {
            Embedder::load(k, &a.models.join(k.file_name()), a.threads)
                .unwrap_or_else(|e| fail(&e.to_string()))
        })
        .collect();

    let rows = read_rows(&a.session);
    let cfg = GateConfig::default();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "session,index,seq,kind,meta_lit,buf_error,frame_mean,frame_sat,n_faces,n_candidates,\
         best_score,box_x,box_y,box_w,box_h,sat_in_box,box_mean,roll_deg,yaw,pitch,gate,\
         dark_index,dark_mean,dark_box_mean,\
         sface_tmpl_lit_max,sface_tmpl_diff_max,auraface_tmpl_lit_max,auraface_tmpl_diff_max"
    );

    // The dark half of a pair is the immediately preceding dark frame, as
    // `fuprobe score` paired them.
    let mut prev_dark: Option<(i32, GrayImage)> = None;
    let mut counts: HashMap<&'static str, u32> = HashMap::new();
    let (mut lit_seen, mut gate_pass) = (0u32, 0u32);

    for r in &rows {
        let img = match pgm::read(&a.session.join(&r.file)) {
            Ok(i) => i,
            Err(e) => fail(&format!("{}: {e}", r.file)),
        };
        let whole = frame_stats(&img);
        if r.lit == Some(false) {
            prev_dark = Some((r.index, img));
            continue;
        }
        if r.lit != Some(true) || r.buf_error {
            continue; // unlabelled or unusable frames are never scored
        }
        lit_seen += 1;

        let faces: Vec<Face> = det.detect(&img).unwrap_or_else(|e| fail(&e.to_string()));
        let reported = faces
            .iter()
            .filter(|f| f.score >= FACE_REPORT_SCORE)
            .count();
        let g = quality_gate(&faces, &img, &cfg);
        *counts.entry(g.reason.name()).or_default() += 1;
        if g.pass() {
            gate_pass += 1;
        }

        // Scores are computed for every frame with a face, gate or no gate,
        // so the CSV can be compared against fuprobe's "all detected" rows.
        let mut cos = [f64::NAN; 4]; // sface lit/diff, auraface lit/diff
        if let Some(f) = faces.first() {
            let diff = prev_dark
                .as_ref()
                .and_then(|(_, d)| lit_minus_dark(&img, d).ok());
            for (ei, e) in emb.iter_mut().enumerate() {
                let name = e.kind().name();
                for (vi, (variant, src)) in [("lit", Some(&img)), ("diff", diff.as_ref())]
                    .into_iter()
                    .enumerate()
                {
                    let Some(src) = src else { continue };
                    let Ok(crop) = nirlock_vision::align::align(src, f) else {
                        continue;
                    };
                    let Ok(v) = e.embed(&crop) else { continue };
                    if let Some(set) = tmpl.find(name, variant)
                        && let Some((mx, _, _)) = set.cosine_stats(&v, Some(&session_name))
                    {
                        cos[ei * 2 + vi] = mx;
                    }
                }
            }
        }

        let (bx, by, bw, bh, score) =
            faces
                .first()
                .map_or((f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN), |f| {
                    (
                        f.x as f64,
                        f.y as f64,
                        f.w as f64,
                        f.h as f64,
                        f.score as f64,
                    )
                });
        let (di, dm, dbm) = match &prev_dark {
            Some((i, d)) => {
                let ds = frame_stats(d);
                let dbox = faces.first().map_or(f64::NAN, |f| {
                    nirlock_vision::gate::frame_stats_in(d, f.x, f.y, f.w, f.h).mean
                });
                (i.to_string(), ds.mean, dbox)
            }
            None => (String::new(), f64::NAN, f64::NAN),
        };
        let _ = writeln!(
            out,
            "{session_name},{},{},lit,true,{},{:.4},{:.6},{},{},{:.6},{:.2},{:.2},{:.2},{:.2},\
             {:.6},{:.4},{:.4},{:.6},{:.6},{},{},{:.4},{:.4},{:.6},{:.6},{:.6},{:.6}",
            r.index,
            r.seq,
            r.buf_error,
            whole.mean,
            whole.sat_frac,
            reported,
            faces.len(),
            score,
            bx,
            by,
            bw,
            bh,
            g.sat_in_box,
            g.box_mean,
            g.pose.roll_deg,
            g.pose.yaw,
            g.pose.pitch,
            g.reason.name(),
            di,
            dm,
            dbm,
            cos[0],
            cos[1],
            cos[2],
            cos[3],
        );
    }

    std::fs::write(&a.csv, out).unwrap_or_else(|e| fail(&format!("{}: {e}", a.csv.display())));
    println!("session   {session_name}");
    println!("lit frames scored  {lit_seen}, gate passed {gate_pass}");
    let mut by_reason: Vec<_> = counts.into_iter().collect();
    by_reason.sort();
    println!(
        "gate      {}",
        by_reason
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("csv       {}", a.csv.display());
    let _ = Reject::None; // the enum is part of the compared surface
}
