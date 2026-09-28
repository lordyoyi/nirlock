//! A minimal single-channel 8-bit image, row-major, tightly packed.
//!
//! This is the only pixel container the daemon needs: the IR camera streams
//! `GREY` (V4L2_PIX_FMT_GREY, one byte per pixel) and every stage downstream
//! consumes grey and replicates it to three channels at the model boundary.

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl GrayImage {
    /// Wraps `data` (length must equal `width * height`).
    pub fn from_vec(width: usize, height: usize, data: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::Image("zero-sized image".into()));
        }
        if data.len() != width * height {
            return Err(Error::Image(format!(
                "buffer has {} bytes, expected {}x{}={}",
                data.len(),
                width,
                height,
                width * height
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// An all-`fill` image.
    pub fn filled(width: usize, height: usize, fill: u8) -> Self {
        Self {
            width,
            height,
            data: vec![fill; width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Pixel at `(x, y)`. Panics when `(x, y)` is outside the image (a real
    /// check, also in release: a wrong-row pixel must never be returned
    /// silently). Untrusted coordinates go through [`GrayImage::get`].
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) outside {}x{}",
            self.width,
            self.height
        );
        self.data[y * self.width + x]
    }

    /// Pixel at `(x, y)` or `None` when outside the image (used by the warp
    /// as `BORDER_CONSTANT` sampling).
    #[inline]
    pub fn get(&self, x: i64, y: i64) -> Option<u8> {
        if x < 0 || y < 0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.data[y * self.width + x])
    }

    /// Mean and fraction of pixels ≥ 250 (the saturation proxy used by the
    /// enrollment gate in phase 0, `frame_stats`).
    pub fn stats(&self) -> (f64, f64) {
        let n = self.data.len() as f64;
        let sum: u64 = self.data.iter().map(|&p| p as u64).sum();
        let sat = self.data.iter().filter(|&&p| p >= 250).count() as f64;
        (sum as f64 / n, sat / n)
    }
}

/// `lit - dark` clamped at 0, the `diff` variant of DESIGN §2.5: it cancels
/// ambient NIR so that only what the emitter lit remains. Both frames must
/// have the same size (`cv::subtract` on `CV_8U` saturates, hence the
/// clamp). Port of `lit_minus_dark` in `phase0/pipeline.cpp`.
pub fn lit_minus_dark(lit: &GrayImage, dark: &GrayImage) -> Result<GrayImage> {
    if lit.width() != dark.width() || lit.height() != dark.height() {
        return Err(Error::Image(format!(
            "diff needs equal sizes, got {}x{} and {}x{}",
            lit.width(),
            lit.height(),
            dark.width(),
            dark.height()
        )));
    }
    let data = lit
        .as_slice()
        .iter()
        .zip(dark.as_slice())
        .map(|(l, d)| l.saturating_sub(*d))
        .collect();
    GrayImage::from_vec(lit.width(), lit.height(), data)
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    #[test]
    fn lit_minus_dark_clamps_and_checks_sizes() {
        let lit = GrayImage::from_vec(2, 2, vec![10, 200, 0, 255]).unwrap();
        let dark = GrayImage::from_vec(2, 2, vec![20, 50, 0, 5]).unwrap();
        // Saturating, as cv::subtract on CV_8U: 10-20 clamps to 0, never wraps.
        assert_eq!(
            lit_minus_dark(&lit, &dark).unwrap().as_slice(),
            &[0, 150, 0, 250]
        );
        let other = GrayImage::from_vec(2, 1, vec![1, 2]).unwrap();
        assert!(lit_minus_dark(&lit, &other).is_err());
    }
}
