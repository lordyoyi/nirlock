//! nirlock-vision: the inference half of `nirlockd`.
//!
//! Ports `phase0/pipeline.{hpp,cpp}` of the lab repository without OpenCV:
//! YuNet decode + NMS, 5-point similarity alignment (Umeyama, no reflection)
//! with a hand-written bilinear warp, AuraFace / SFace preprocessing under
//! their distinct input contracts (DESIGN §2.6), L2 normalisation, cosine,
//! and the FUEMB1 template reader.
//!
//! ONNX Runtime is reached through `ort` with `load-dynamic`
//! (ADR-0015): nothing is linked at build time, the shared library is
//! `dlopen`ed by [`runtime::init`].
//!
//! `unsafe` is denied crate-wide; the single exemption is the
//! `OrtGetApiBase()->GetVersionString()` probe in `runtime::apibase`, which
//! `ort` does not expose (see the SAFETY comments there).

#![deny(unsafe_code)]

pub mod align;
pub mod embed;
pub mod gate;
pub mod image;
pub mod math;
pub mod pgm;
pub mod runtime;
pub mod template;
pub mod yunet;

/// Errors surfaced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ONNX Runtime: {0}")]
    Ort(#[from] ort::Error),
    #[error("cannot load ONNX Runtime library: {0}")]
    OrtLoad(String),
    #[error("model {model}: {msg}")]
    Model { model: String, msg: String },
    #[error("i/o {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("bad image: {0}")]
    Image(String),
    #[error("bad template {path}: {msg}")]
    Template { path: String, msg: String },
}

pub type Result<T> = std::result::Result<T, Error>;
