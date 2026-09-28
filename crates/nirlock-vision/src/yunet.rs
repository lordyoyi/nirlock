//! YuNet (`face_detection_yunet_2026may.onnx`) at native resolution.
//!
//! The model exports its raw heads (`cls_*`, `obj_*`, `bbox_*`, `kps_*` for
//! strides 8/16/32); the prior decode, score fusion and NMS below reproduce
//! `FaceDetectorYNImpl` in OpenCV's `modules/objdetect/src/face_detect.cpp`
//! (4.x), which is what fuprobe used through `cv::FaceDetectorYN`:
//!
//! * the input is zero-padded on the right/bottom to a multiple of 32
//!   (640x360 → 640x384), fed as raw 0..255 float, NCHW, 3 channels;
//! * `score = sqrt(clamp(cls) · clamp(obj))`;
//! * `cx = (c + bbox[0]) · s`, `cy = (r + bbox[1]) · s`, `w = exp(bbox[2]) · s`,
//!   `h = exp(bbox[3]) · s`, top-left = centre − size/2;
//! * landmark `n`: `((kps[2n] + c) · s, (kps[2n+1] + r) · s)`;
//! * candidates with `score ≥ floor` are sorted, truncated to `top_k` and
//!   greedily NMS'd with IoU > 0.3 on **integer-truncated** boxes
//!   (`Rect2i`, as OpenCV does).
//!
//! Landmark order: right eye, left eye, nose tip, right mouth corner, left
//! mouth corner ("right" = the subject's right = smaller x in the image).

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use crate::image::GrayImage;
use crate::runtime::{Loaded, load_session};
use crate::{Error, Result};

/// YuNet's internal score floor (fuprobe `kDetectorFloor`): low on purpose
/// so near-misses stay observable; decisions use [`REPORT_SCORE`] and the
/// quality gate.
pub const DETECTOR_FLOOR: f32 = 0.30;
/// "A face was detected" for reporting purposes (fuprobe `kFaceReportScore`).
pub const REPORT_SCORE: f32 = 0.60;
/// NMS IoU threshold used by fuprobe (`FaceDetectorYN::create(..., 0.3f, 50)`).
pub const NMS_IOU: f32 = 0.3;
/// Top-k candidates kept before NMS.
pub const TOP_K: usize = 50;

const STRIDES: [usize; 3] = [8, 16, 32];
const HEADS: [&str; 12] = [
    "cls_8", "cls_16", "cls_32", "obj_8", "obj_16", "obj_32", "bbox_8", "bbox_16", "bbox_32",
    "kps_8", "kps_16", "kps_32",
];

/// One detection in image pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    /// Top-left x, y, width, height (floats, as OpenCV returns them).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Five landmarks `[x, y]` in YuNet order.
    pub lm: [[f32; 2]; 5],
    pub score: f32,
}

impl Face {
    /// `min(w, h)`, the size measure the gate uses (`min_box`).
    pub fn min_side(&self) -> f32 {
        self.w.min(self.h)
    }
}

/// A loaded YuNet session bound to one input size.
pub struct Detector {
    session: Session,
    input_name: String,
    score_floor: f32,
    pub load_ms: f64,
}

/// Rounds `n` up to the next multiple of 32 (OpenCV: `((n-1)/32+1)*32`).
pub fn padded(n: usize) -> usize {
    ((n.max(1) - 1) / 32 + 1) * 32
}

/// Builds the padded NCHW float blob (`1x3xHpxWp`) from a grey frame,
/// replicating the channel three times (BGR of a grey image).
pub fn preprocess(img: &GrayImage) -> (Vec<f32>, [i64; 4]) {
    let (w, h) = (img.width(), img.height());
    let (pw, ph) = (padded(w), padded(h));
    let plane = pw * ph;
    let mut blob = vec![0f32; 3 * plane];
    let src = img.as_slice();
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let dst = &mut blob[y * pw..y * pw + w];
        for (d, s) in dst.iter_mut().zip(row) {
            *d = *s as f32;
        }
    }
    let (c0, rest) = blob.split_at_mut(plane);
    let (c1, c2) = rest.split_at_mut(plane);
    c1.copy_from_slice(c0);
    c2.copy_from_slice(c0);
    (blob, [1, 3, ph as i64, pw as i64])
}

/// Decodes the twelve raw heads for a padded input of `pw x ph` into
/// candidate faces (no threshold, no NMS). `heads[i]` is the flat float data
/// of `HEADS[i]`.
pub fn decode(heads: &[&[f32]; 12], pw: usize, ph: usize) -> Result<Vec<Face>> {
    let mut faces = Vec::new();
    for (i, &s) in STRIDES.iter().enumerate() {
        let cols = pw / s;
        let rows = ph / s;
        let n = cols * rows;
        let (cls, obj, bbox, kps) = (heads[i], heads[i + 3], heads[i + 6], heads[i + 9]);
        if cls.len() < n || obj.len() < n || bbox.len() < 4 * n || kps.len() < 10 * n {
            return Err(Error::Model {
                model: "yunet".into(),
                msg: format!(
                    "stride {s}: head sizes cls={} obj={} bbox={} kps={} < expected for {cols}x{rows}",
                    cls.len(),
                    obj.len(),
                    bbox.len(),
                    kps.len()
                ),
            });
        }
        let sf = s as f32;
        for r in 0..rows {
            for c in 0..cols {
                let idx = r * cols + c;
                let cs = cls[idx].clamp(0.0, 1.0);
                let os = obj[idx].clamp(0.0, 1.0);
                let score = (cs * os).sqrt();
                let cx = (c as f32 + bbox[idx * 4]) * sf;
                let cy = (r as f32 + bbox[idx * 4 + 1]) * sf;
                let w = bbox[idx * 4 + 2].exp() * sf;
                let h = bbox[idx * 4 + 3].exp() * sf;
                let mut lm = [[0f32; 2]; 5];
                for (n, l) in lm.iter_mut().enumerate() {
                    l[0] = (kps[idx * 10 + 2 * n] + c as f32) * sf;
                    l[1] = (kps[idx * 10 + 2 * n + 1] + r as f32) * sf;
                }
                faces.push(Face {
                    x: cx - w / 2.0,
                    y: cy - h / 2.0,
                    w,
                    h,
                    lm,
                    score,
                });
            }
        }
    }
    Ok(faces)
}

/// Integer rectangle as OpenCV's `Rect2i(int(x), int(y), int(w), int(h))`
/// (truncation towards zero).
#[derive(Clone, Copy)]
struct IRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl IRect {
    fn of(f: &Face) -> Self {
        Self {
            x: f.x as i32,
            y: f.y as i32,
            w: f.w as i32,
            h: f.h as i32,
        }
    }
    fn area(&self) -> f64 {
        (self.w as f64) * (self.h as f64)
    }
    /// `1 - jaccardDistance(a, b)` for `Rect2i`.
    fn iou(&self, o: &IRect) -> f64 {
        let aa = self.area();
        let ab = o.area();
        if aa + ab <= f64::EPSILON {
            return 1.0;
        }
        let x1 = self.x.max(o.x);
        let y1 = self.y.max(o.y);
        let x2 = (self.x + self.w).min(o.x + o.w);
        let y2 = (self.y + self.h).min(o.y + o.h);
        let inter = if x2 > x1 && y2 > y1 {
            ((x2 - x1) as f64) * ((y2 - y1) as f64)
        } else {
            0.0
        };
        inter / (aa + ab - inter)
    }
}

/// `dnn::NMSBoxes(boxes, scores, floor, iou, keep, eta=1, top_k)` on the
/// decoded candidates. Returns the survivors sorted by descending score.
pub fn nms(cands: &[Face], floor: f32, iou: f32, top_k: usize) -> Vec<Face> {
    let mut idx: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].score > floor)
        .collect();
    // OpenCV sorts with std::stable_sort on (score desc); index order breaks ties.
    idx.sort_by(|&a, &b| {
        cands[b]
            .score
            .partial_cmp(&cands[a].score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if top_k > 0 && idx.len() > top_k {
        idx.truncate(top_k);
    }
    let mut kept: Vec<usize> = Vec::new();
    for &i in &idx {
        let ri = IRect::of(&cands[i]);
        let keep = kept
            .iter()
            .all(|&k| ri.iou(&IRect::of(&cands[k])) <= iou as f64);
        if keep {
            kept.push(i);
        }
    }
    kept.into_iter().map(|i| cands[i]).collect()
}

impl Detector {
    /// Loads YuNet with `intra_threads` (ADR-0015: 1 for this model in the
    /// daemon) and checks its heads. It does **not** run a forward: the
    /// first-inference cost (lazy kernel init, +3–20 ms) is paid by the
    /// first [`Detector::detect`] unless the caller runs
    /// [`Detector::warm_up`] first, which is what the daemon's `prewarm`
    /// does (DESIGN §2.6) and what the bench deliberately does not, so its
    /// `first` column is the true cold cost.
    pub fn load(model: &Path, intra_threads: usize, score_floor: f32) -> Result<Self> {
        let Loaded { session, load_ms } = load_session(model, intra_threads)?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| Error::Model {
                model: model.display().to_string(),
                msg: "model has no inputs".into(),
            })?;
        let names: Vec<&str> = session.outputs().iter().map(|o| o.name()).collect();
        for h in HEADS {
            if !names.contains(&h) {
                return Err(Error::Model {
                    model: model.display().to_string(),
                    msg: format!("missing output head {h}; outputs are {names:?}"),
                });
            }
        }
        Ok(Self {
            session,
            input_name,
            score_floor,
            load_ms,
        })
    }

    /// Warm-up forward (lazy kernel init). Returns the elapsed milliseconds.
    pub fn warm_up(&mut self, width: usize, height: usize) -> Result<f64> {
        let t = std::time::Instant::now();
        let _ = self.detect(&GrayImage::filled(width, height, 0))?;
        Ok(t.elapsed().as_secs_f64() * 1e3)
    }

    /// Runs the detector; faces sorted by descending score, already NMS'd
    /// and above the score floor.
    pub fn detect(&mut self, img: &GrayImage) -> Result<Vec<Face>> {
        let (blob, shape) = preprocess(img);
        let (pw, ph) = (shape[3] as usize, shape[2] as usize);
        let input = Tensor::from_array((shape, blob))?;
        let outputs = self.session.run(vec![(self.input_name.as_str(), input)])?;
        let mut heads: [&[f32]; 12] = [&[]; 12];
        for (i, h) in HEADS.iter().enumerate() {
            let v = outputs.get(h).ok_or_else(|| Error::Model {
                model: "yunet".into(),
                msg: format!("output {h} missing at run time"),
            })?;
            let (_, data) = v.try_extract_tensor::<f32>()?;
            heads[i] = data;
        }
        let cands = decode(&heads, pw, ph)?;
        Ok(nms(&cands, self.score_floor, NMS_IOU, TOP_K))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_rounds_up_to_32() {
        assert_eq!(padded(640), 640);
        assert_eq!(padded(360), 384);
        assert_eq!(padded(1), 32);
        assert_eq!(padded(33), 64);
    }

    #[test]
    fn preprocess_pads_bottom_and_replicates_channels() {
        let mut img = GrayImage::filled(40, 20, 7);
        img.as_mut_slice()[0] = 200;
        let (blob, shape) = preprocess(&img);
        assert_eq!(shape, [1, 3, 32, 64]);
        let plane = 32 * 64;
        assert_eq!(blob.len(), 3 * plane);
        assert_eq!(blob[0], 200.0);
        assert_eq!(blob[1], 7.0);
        assert_eq!(blob[40], 0.0); // right pad
        assert_eq!(blob[20 * 64], 0.0); // bottom pad
        assert_eq!(blob[plane], 200.0);
        assert_eq!(blob[2 * plane + 1], 7.0);
    }

    #[test]
    fn decode_matches_opencv_formula() {
        // One stride-8 cell at (c=3, r=2) on a 64x32 padded input.
        let (pw, ph) = (64usize, 32usize);
        let n8 = (pw / 8) * (ph / 8);
        let n16 = (pw / 16) * (ph / 16);
        let n32 = (pw / 32) * (ph / 32);
        let mut cls8 = vec![0f32; n8];
        let mut obj8 = vec![0f32; n8];
        let mut bbox8 = vec![0f32; 4 * n8];
        let mut kps8 = vec![0f32; 10 * n8];
        let idx = 2 * (pw / 8) + 3;
        cls8[idx] = 0.81;
        obj8[idx] = 1.5; // clamped to 1
        bbox8[idx * 4..idx * 4 + 4].copy_from_slice(&[0.5, -0.25, 1.0f32.ln(), 2.0f32.ln()]);
        for k in 0..5 {
            kps8[idx * 10 + 2 * k] = 0.1 * k as f32;
            kps8[idx * 10 + 2 * k + 1] = -0.1 * k as f32;
        }
        let z16 = vec![0f32; 10 * n16];
        let z32 = vec![0f32; 10 * n32];
        let heads: [&[f32]; 12] = [
            &cls8, &z16, &z32, &obj8, &z16, &z32, &bbox8, &z16, &z32, &kps8, &z16, &z32,
        ];
        let faces = decode(&heads, pw, ph).unwrap();
        assert_eq!(faces.len(), n8 + n16 + n32);
        let f = faces[idx];
        assert!(
            (f.score - 0.9).abs() < 1e-6,
            "sqrt(0.81*1) = 0.9, got {}",
            f.score
        );
        // cx = (3+0.5)*8 = 28, cy = (2-0.25)*8 = 14, w = 8, h = 16
        assert!((f.x - 24.0).abs() < 1e-5 && (f.y - 6.0).abs() < 1e-5);
        assert!((f.w - 8.0).abs() < 1e-5 && (f.h - 16.0).abs() < 1e-5);
        assert!((f.lm[1][0] - (0.1 + 3.0) * 8.0).abs() < 1e-5);
        assert!((f.lm[1][1] - (-0.1 + 2.0) * 8.0).abs() < 1e-5);
        // A cell with zeros everywhere decodes to a 1-cell box of score 0.
        let g = faces[n8 + n16]; // first stride-32 cell
        assert_eq!(g.score, 0.0);
        assert!((g.w - 32.0).abs() < 1e-5);
    }

    fn face(x: f32, y: f32, w: f32, h: f32, score: f32) -> Face {
        Face {
            x,
            y,
            w,
            h,
            lm: [[0.0; 2]; 5],
            score,
        }
    }

    #[test]
    fn nms_suppresses_overlaps_and_respects_floor_and_topk() {
        let cands = vec![
            face(10.0, 10.0, 100.0, 100.0, 0.9),
            face(15.0, 12.0, 100.0, 100.0, 0.8), // IoU ~0.8 with the first: suppressed
            face(300.0, 10.0, 100.0, 100.0, 0.7),
            face(300.0, 10.0, 100.0, 100.0, 0.2), // below floor
            face(500.0, 10.0, 50.0, 50.0, 0.65),
        ];
        let out = nms(&cands, 0.3, 0.3, 50);
        let scores: Vec<f32> = out.iter().map(|f| f.score).collect();
        assert_eq!(scores, vec![0.9, 0.7, 0.65]);
        // top_k truncates the sorted candidates *before* NMS (OpenCV
        // GetMaxScoreIndex): with top_k=2 only [0.9, 0.8] survive the cut and
        // 0.8 is then suppressed, so a single face comes out.
        let out2 = nms(&cands, 0.3, 0.3, 2);
        assert_eq!(out2.len(), 1);
        let out3 = nms(&cands, 0.3, 0.3, 3);
        assert_eq!(
            out3.iter().map(|f| f.score).collect::<Vec<_>>(),
            vec![0.9, 0.7]
        );
    }

    #[test]
    fn iou_uses_truncated_int_rects() {
        let a = IRect::of(&face(0.9, 0.9, 10.9, 10.9, 1.0));
        let b = IRect::of(&face(5.0, 0.0, 10.0, 10.0, 1.0));
        // a truncates to (0,0,10,10): inter = 5x10 = 50, union = 150
        assert!((a.iou(&b) - 50.0 / 150.0).abs() < 1e-12);
    }
}
