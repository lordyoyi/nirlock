//! 5-point similarity alignment to the ArcFace 112x112 template.
//!
//! [`similarity`] solves the least-squares similarity transform (rotation +
//! uniform scale + translation, **no reflection**) that maps the five YuNet
//! landmarks onto [`ARCFACE_REF`], the reference table
//! `FaceRecognizerSF::alignCrop` uses in OpenCV. In 2D a similarity without
//! reflection is multiplication by one complex number `a + ib` plus a
//! translation, so Umeyama's SVD-based closed form and the direct
//! least-squares solution for `(a, b)` coincide (both minimise the same sum
//! of squared residuals over the same 4-parameter family); the direct form
//! is what `phase0/pipeline.cpp::align_face` computes and is used here.
//!
//! [`warp`] is a hand-written `warpAffine(INTER_LINEAR, BORDER_CONSTANT 0)`:
//! the forward matrix is inverted, every destination pixel samples the source
//! bilinearly at the inverse-mapped position, samples outside the image read
//! 0, and the result is rounded to the nearest integer. OpenCV interpolates in
//! 5-bit fixed point, so parity is "≤ 1 grey level on almost every pixel",
//! not bit-exact (DESIGN §2.6).
//!
//! Nothing here panics on data: degenerate landmarks (all five on a point or
//! a line) give a singular forward map, which [`warp`] and [`align`] report
//! as `Error::Image("singular alignment")` so the gate can count the frame as
//! a quality rejection rather than feeding a garbage crop to the embedder.

use crate::image::GrayImage;
use crate::yunet::Face;
use crate::{Error, Result};

/// ArcFace / InsightFace reference landmarks for a 112x112 crop
/// (right eye, left eye, nose, right mouth corner, left mouth corner).
pub const ARCFACE_REF: [[f64; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Output crop side in pixels.
pub const CROP: usize = 112;

/// A 2x3 affine matrix in row-major order: `[a, b, tx, c, d, ty]`,
/// mapping `(x, y) → (a·x + b·y + tx, c·x + d·y + ty)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
    }

    /// Inverse of the affine map; `None` when singular or not finite.
    pub fn invert(&self) -> Option<Affine> {
        let [a, b, tx, c, d, ty] = self.0;
        let det = a * d - b * c;
        if !det.is_finite() || det.abs() < 1e-12 || !self.0.iter().all(|v| v.is_finite()) {
            return None;
        }
        let ia = d / det;
        let ib = -b / det;
        let ic = -c / det;
        let id = a / det;
        Some(Affine([
            ia,
            ib,
            -(ia * tx + ib * ty),
            ic,
            id,
            -(ic * tx + id * ty),
        ]))
    }

    /// Uniform scale factor of a similarity (`sqrt(a² + c²)`).
    pub fn scale(&self) -> f64 {
        (self.0[0] * self.0[0] + self.0[3] * self.0[3]).sqrt()
    }
}

/// Least-squares similarity `src → dst` without reflection (Umeyama, 2D).
///
/// With centred coordinates `xs, ys` (source) and `xd, yd` (destination):
/// `a = Σ(xs·xd + ys·yd) / Σ(xs² + ys²)`, `b = Σ(xs·yd − ys·xd) / Σ(xs² + ys²)`,
/// `M = [a −b tx; b a ty]` with `t` chosen so centroids coincide.
///
/// The point count is a type-level constant so mismatched inputs cannot
/// compile; `N ≥ 2` is required for the problem to be determined. Coincident
/// points make the map singular (`a = b = 0`), which [`Affine::invert`]
/// reports as `None`.
pub fn similarity<const N: usize>(src: &[[f64; 2]; N], dst: &[[f64; 2]; N]) -> Affine {
    let n = N as f64;
    let (mut msx, mut msy, mut mdx, mut mdy) = (0.0, 0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        msx += s[0];
        msy += s[1];
        mdx += d[0];
        mdy += d[1];
    }
    msx /= n;
    msy /= n;
    mdx /= n;
    mdy /= n;
    let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let xs = s[0] - msx;
        let ys = s[1] - msy;
        let xd = d[0] - mdx;
        let yd = d[1] - mdy;
        num_a += xs * xd + ys * yd;
        num_b += xs * yd - ys * xd;
        den += xs * xs + ys * ys;
    }
    let den = den.max(1e-9);
    let a = num_a / den;
    let b = num_b / den;
    Affine([
        a,
        -b,
        mdx - (a * msx - b * msy),
        b,
        a,
        mdy - (b * msx + a * msy),
    ])
}

/// Forward matrix that takes the face's landmarks onto [`ARCFACE_REF`].
pub fn face_to_arcface(f: &Face) -> Affine {
    let src: [[f64; 2]; 5] = f.lm.map(|p| [p[0] as f64, p[1] as f64]);
    similarity(&src, &ARCFACE_REF)
}

/// `warpAffine(src, m, (w, h), INTER_LINEAR, BORDER_CONSTANT, 0)` with `m`
/// the **forward** map (source → destination), like OpenCV's default.
///
/// Errors: `w == 0 || h == 0`, `w * h` overflow, or a singular / non-finite
/// `m` (`"singular alignment"`).
pub fn warp(src: &GrayImage, m: &Affine, w: usize, h: usize) -> Result<GrayImage> {
    if w == 0 || h == 0 {
        return Err(Error::Image("zero-sized warp target".into()));
    }
    let len = w
        .checked_mul(h)
        .ok_or_else(|| Error::Image("warp target too large".into()))?;
    let inv = m
        .invert()
        .ok_or_else(|| Error::Image("singular alignment".into()))?;
    let mut out = vec![0u8; len];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = inv.apply(x as f64, y as f64);
            out[y * w + x] = sample_bilinear(src, sx, sy);
        }
    }
    GrayImage::from_vec(w, h, out)
}

/// Bilinear sample at `(sx, sy)` with zero outside the image.
#[inline]
fn sample_bilinear(src: &GrayImage, sx: f64, sy: f64) -> u8 {
    let x0 = sx.floor();
    let y0 = sy.floor();
    let fx = sx - x0;
    let fy = sy - y0;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let p = |x: i64, y: i64| src.get(x, y).map(|v| v as f64).unwrap_or(0.0);
    let v = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
        + p(x0 + 1, y0) * fx * (1.0 - fy)
        + p(x0, y0 + 1) * (1.0 - fx) * fy
        + p(x0 + 1, y0 + 1) * fx * fy;
    v.round().clamp(0.0, 255.0) as u8
}

/// The aligned 112x112 grey crop for a detected face (fuprobe `align_face`,
/// before the grey→3-channel replication that happens at the model boundary).
/// Degenerate landmarks are `Err(Error::Image("singular alignment"))`.
pub fn align(src: &GrayImage, f: &Face) -> Result<GrayImage> {
    warp(src, &face_to_arcface(f), CROP, CROP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn recovers_a_known_similarity() {
        // Rotate by 17°, scale 0.37, translate (120, -40): src = T(ref).
        let th = 17f64.to_radians();
        let s = 0.37;
        let (ca, sa) = (th.cos() * s, th.sin() * s);
        let fwd = Affine([ca, -sa, 120.0, sa, ca, -40.0]);
        let src: [[f64; 2]; 5] = ARCFACE_REF.map(|p| {
            let (x, y) = fwd.apply(p[0], p[1]);
            [x, y]
        });
        let m = similarity(&src, &ARCFACE_REF);
        let expect = fwd.invert().unwrap();
        for i in 0..6 {
            assert!(
                close(m.0[i], expect.0[i], 1e-9),
                "m[{i}] = {} vs {}",
                m.0[i],
                expect.0[i]
            );
        }
        for (p, r) in src.iter().zip(&ARCFACE_REF) {
            let (x, y) = m.apply(p[0], p[1]);
            assert!(close(x, r[0], 1e-9) && close(y, r[1], 1e-9));
        }
        assert!(close(m.scale(), 1.0 / s, 1e-9));
    }

    #[test]
    fn matches_umeyama_svd_on_noisy_points() {
        // Independent oracle: Umeyama via an explicit 2x2 SVD of the
        // cross-covariance. Both must agree on perturbed landmarks (≤ 1e-9).
        let src = [
            [231.4, 118.2],
            [289.9, 121.7],
            [261.0, 152.3],
            [238.7, 181.9],
            [284.2, 183.1],
        ];
        let dst = ARCFACE_REF;
        let m = similarity(&src, &dst);
        let n = 5.0;
        let ms = [
            src.iter().map(|p| p[0]).sum::<f64>() / n,
            src.iter().map(|p| p[1]).sum::<f64>() / n,
        ];
        let md = [
            dst.iter().map(|p| p[0]).sum::<f64>() / n,
            dst.iter().map(|p| p[1]).sum::<f64>() / n,
        ];
        // Σ = (1/n) Σ (d - md)(s - ms)^T ; σ² = (1/n) Σ |s - ms|²
        let mut sig = [[0.0f64; 2]; 2];
        let mut var = 0.0;
        for (s, d) in src.iter().zip(&dst) {
            let ds = [s[0] - ms[0], s[1] - ms[1]];
            let dd = [d[0] - md[0], d[1] - md[1]];
            for (i, di) in dd.iter().enumerate() {
                for (j, sj) in ds.iter().enumerate() {
                    sig[i][j] += di * sj / n;
                }
            }
            var += (ds[0] * ds[0] + ds[1] * ds[1]) / n;
        }
        // 2x2 SVD via the polar decomposition: Σ = R·P with R a rotation
        // (det > 0 is the no-reflection case) and tr(D S) = tr(P).
        let det = sig[0][0] * sig[1][1] - sig[0][1] * sig[1][0];
        assert!(det > 0.0);
        // R = Σ (ΣᵀΣ)^(-1/2); for 2x2 use the closed form of the rotation
        // that best matches Σ: angle = atan2(s10 - s01, s00 + s11).
        let ang = (sig[1][0] - sig[0][1]).atan2(sig[0][0] + sig[1][1]);
        let (c, s_) = (ang.cos(), ang.sin());
        let r = [[c, -s_], [s_, c]];
        // P = Rᵀ Σ ; tr(P) = Σ_i (Rᵀ Σ)_ii
        let tr =
            r[0][0] * sig[0][0] + r[1][0] * sig[1][0] + r[0][1] * sig[0][1] + r[1][1] * sig[1][1];
        let scale = tr / var;
        let t = [
            md[0] - scale * (r[0][0] * ms[0] + r[0][1] * ms[1]),
            md[1] - scale * (r[1][0] * ms[0] + r[1][1] * ms[1]),
        ];
        let u = Affine([
            scale * r[0][0],
            scale * r[0][1],
            t[0],
            scale * r[1][0],
            scale * r[1][1],
            t[1],
        ]);
        for i in 0..6 {
            assert!(
                close(m.0[i], u.0[i], 1e-9),
                "m[{i}] = {} vs umeyama {}",
                m.0[i],
                u.0[i]
            );
        }
    }

    #[test]
    fn invert_roundtrips() {
        let m = Affine([0.8, -0.2, 5.0, 0.2, 0.8, -3.0]);
        let inv = m.invert().unwrap();
        let (x, y) = m.apply(13.0, -7.5);
        let (bx, by) = inv.apply(x, y);
        assert!(close(bx, 13.0, 1e-12) && close(by, -7.5, 1e-12));
        assert!(Affine([1.0, 2.0, 0.0, 2.0, 4.0, 0.0]).invert().is_none());
    }

    #[test]
    fn warp_identity_and_translation() {
        let img = GrayImage::from_vec(4, 3, (0..12).map(|i| i as u8 * 10).collect()).unwrap();
        let id = warp(&img, &Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]), 4, 3).unwrap();
        assert_eq!(id, img);
        // Shift right by one: dst(x) = src(x-1); column 0 reads the border (0).
        let sh = warp(&img, &Affine([1.0, 0.0, 1.0, 0.0, 1.0, 0.0]), 4, 3).unwrap();
        assert_eq!(
            sh.as_slice(),
            &[0, 0, 10, 20, 0, 40, 50, 60, 0, 80, 90, 100]
        );
        // Half-pixel shift interpolates between neighbours and rounds.
        let hf = warp(&img, &Affine([1.0, 0.0, 0.5, 0.0, 1.0, 0.0]), 4, 3).unwrap();
        assert_eq!(hf.at(1, 0), 5); // (0 + 10) / 2
        assert_eq!(hf.at(3, 1), 65); // (60 + 70) / 2
        assert_eq!(hf.at(0, 0), 0); // (border 0 + 0) / 2
    }

    #[test]
    fn align_output_size_and_scale() {
        let img = GrayImage::filled(640, 360, 128);
        let f = Face {
            x: 250.0,
            y: 90.0,
            w: 140.0,
            h: 180.0,
            lm: [
                [290.0, 150.0],
                [350.0, 150.0],
                [320.0, 185.0],
                [295.0, 220.0],
                [345.0, 220.0],
            ],
            score: 0.9,
        };
        let crop = align(&img, &f).unwrap();
        assert_eq!((crop.width(), crop.height()), (112, 112));
        // Flat input → flat output where the crop stays inside the frame.
        assert_eq!(crop.at(56, 56), 128);
        let m = face_to_arcface(&f);
        // Inter-ocular 60 px → 35.24 px in the template: scale ≈ 0.587.
        assert!(
            close(m.scale(), 35.2372 / 60.0, 0.02),
            "scale {}",
            m.scale()
        );
    }

    #[test]
    fn degenerate_inputs_are_errors_not_panics_or_garbage() {
        let img = GrayImage::filled(64, 64, 200);
        // All five landmarks on one point: the similarity collapses to
        // a = b = 0, the map is singular, and align must say so.
        let f = Face {
            x: 10.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
            lm: [[30.0, 30.0]; 5],
            score: 0.9,
        };
        let m = face_to_arcface(&f);
        assert!(m.invert().is_none());
        match align(&img, &f) {
            Err(Error::Image(msg)) => assert!(msg.contains("singular"), "{msg}"),
            other => panic!("expected singular alignment, got {other:?}"),
        }
        // Non-finite matrices are singular too.
        assert!(
            Affine([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0])
                .invert()
                .is_none()
        );
        assert!(
            Affine([f64::INFINITY, 0.0, 0.0, 0.0, 1.0, 0.0])
                .invert()
                .is_none()
        );
        // Zero-sized targets are errors, not panics.
        let id = Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(warp(&img, &id, 0, 5).is_err());
        assert!(warp(&img, &id, 5, 0).is_err());
        assert!(warp(&img, &id, usize::MAX, 2).is_err());
    }
}
