//! Face embedders and their **distinct** input contracts (DESIGN §2.6,
//! ADR-0007).
//!
//! * AuraFace (`auraface_glintr100.onnx`, ArcFace-style, 512-d): the aligned
//!   112x112 crop, RGB, `float32 (x − 127.5) / 127.5`, NCHW; output
//!   L2-normalised by us. Equivalent to
//!   `blobFromImage(aligned, 1/127.5, (112,112), (127.5,127.5,127.5), swapRB=true)`.
//! * SFace (`face_recognition_sface_2021dec.onnx`, 128-d): the same crop,
//!   RGB, **raw 0..255 float32, no mean, no scale**, NCHW; output
//!   L2-normalised by us. Equivalent to what `FaceRecognizerSF::feature`
//!   does: `blobFromImage(aligned, 1, (112,112), (0,0,0), swapRB=true, crop=false)`.
//!
//! The crop is grey replicated to three channels, so `swapRB` is neutral; it
//! is still declared so the contract survives an RGB camera.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use crate::align::CROP;
use crate::image::GrayImage;
use crate::math::l2_normalise;
use crate::runtime::{Loaded, load_session};
use crate::{Error, Result};

/// Which embedder (and therefore which preprocessing) a session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    AuraFace,
    SFace,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::AuraFace => "auraface",
            Kind::SFace => "sface",
        }
    }

    /// Embedding dimensionality the model must produce.
    pub fn dim(self) -> usize {
        match self {
            Kind::AuraFace => 512,
            Kind::SFace => 128,
        }
    }

    /// Canonical file name inside the models directory.
    pub fn file_name(self) -> &'static str {
        match self {
            Kind::AuraFace => "auraface_glintr100.onnx",
            Kind::SFace => "face_recognition_sface_2021dec.onnx",
        }
    }
}

/// Builds the `1x3x112x112` NCHW float blob for `kind` from an aligned grey
/// crop. Channel order is RGB; all three planes are identical.
pub fn preprocess(kind: Kind, crop: &GrayImage) -> Result<Vec<f32>> {
    if crop.width() != CROP || crop.height() != CROP {
        return Err(Error::Image(format!(
            "aligned crop must be {CROP}x{CROP}, got {}x{}",
            crop.width(),
            crop.height()
        )));
    }
    let plane = CROP * CROP;
    let mut blob = vec![0f32; 3 * plane];
    {
        let (c0, rest) = blob.split_at_mut(plane);
        for (d, s) in c0.iter_mut().zip(crop.as_slice()) {
            *d = match kind {
                Kind::AuraFace => (*s as f32 - 127.5) / 127.5,
                Kind::SFace => *s as f32,
            };
        }
        let (c1, c2) = rest.split_at_mut(plane);
        c1.copy_from_slice(c0);
        c2.copy_from_slice(c0);
    }
    Ok(blob)
}

/// A loaded embedder session.
pub struct Embedder {
    kind: Kind,
    session: Session,
    input_name: String,
    output_name: String,
    pub load_ms: f64,
}

impl Embedder {
    /// Loads the model at `path` as `kind` with `intra_threads`. Verifies that
    /// the model has exactly one input and that its static dims (when
    /// declared) are `[?, 3, 112, 112]`.
    pub fn load(kind: Kind, path: &Path, intra_threads: usize) -> Result<Self> {
        let Loaded { session, load_ms } = load_session(path, intra_threads)?;
        let model = path.display().to_string();
        let bad = |msg: String| Error::Model {
            model: model.clone(),
            msg,
        };
        let inputs = session.inputs();
        if inputs.len() != 1 {
            return Err(bad(format!("expected 1 input, model has {}", inputs.len())));
        }
        let input_name = inputs[0].name().to_string();
        if let Some(shape) = inputs[0].dtype().tensor_shape() {
            let dims: Vec<i64> = shape.iter().copied().collect();
            if dims.len() != 4
                || (dims[1] != 3 && dims[1] != -1)
                || (dims[2] != CROP as i64 && dims[2] != -1)
                || (dims[3] != CROP as i64 && dims[3] != -1)
            {
                return Err(bad(format!(
                    "input {input_name} has shape {dims:?}, expected [N,3,112,112]"
                )));
            }
        }
        let outputs = session.outputs();
        if outputs.is_empty() {
            return Err(bad("model has no outputs".into()));
        }
        let output_name = outputs[0].name().to_string();
        Ok(Self {
            kind,
            session,
            input_name,
            output_name,
            load_ms,
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Warm-up forward on a black crop. Returns elapsed milliseconds.
    pub fn warm_up(&mut self) -> Result<f64> {
        let t = std::time::Instant::now();
        let _ = self.embed(&GrayImage::filled(CROP, CROP, 0))?;
        Ok(t.elapsed().as_secs_f64() * 1e3)
    }

    /// L2-normalised embedding of an aligned 112x112 crop.
    pub fn embed(&mut self, crop: &GrayImage) -> Result<Vec<f32>> {
        let blob = preprocess(self.kind, crop)?;
        let input = Tensor::from_array(([1i64, 3, CROP as i64, CROP as i64], blob))?;
        let outputs = self.session.run(vec![(self.input_name.as_str(), input)])?;
        let value = outputs.get(&self.output_name).ok_or_else(|| Error::Model {
            model: self.kind.name().into(),
            msg: format!("output {} missing at run time", self.output_name),
        })?;
        let (_, data) = value.try_extract_tensor::<f32>()?;
        if data.len() != self.kind.dim() {
            return Err(Error::Model {
                model: self.kind.name().into(),
                msg: format!(
                    "embedding has {} values, expected {}",
                    data.len(),
                    self.kind.dim()
                ),
            });
        }
        let mut v = data.to_vec();
        l2_normalise(&mut v);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_differ() {
        let mut crop = GrayImage::filled(CROP, CROP, 0);
        crop.as_mut_slice()[0] = 255;
        crop.as_mut_slice()[1] = 127;
        let a = preprocess(Kind::AuraFace, &crop).unwrap();
        let s = preprocess(Kind::SFace, &crop).unwrap();
        let plane = CROP * CROP;
        assert_eq!(a.len(), 3 * plane);
        assert!((a[0] - 1.0).abs() < 1e-6);
        assert!((a[1] - (127.0 - 127.5) / 127.5).abs() < 1e-6);
        assert!((a[2] + 1.0).abs() < 1e-6);
        assert_eq!(s[0], 255.0);
        assert_eq!(s[1], 127.0);
        assert_eq!(s[2], 0.0);
        // Planes replicated.
        assert_eq!(a[plane], a[0]);
        assert_eq!(a[2 * plane + 1], a[1]);
        assert_eq!(s[plane], 255.0);
        assert!(preprocess(Kind::SFace, &GrayImage::filled(64, 64, 0)).is_err());
    }
}
