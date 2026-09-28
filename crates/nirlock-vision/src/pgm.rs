//! Binary PGM (`P5`, maxval 255) reader and writer.
//!
//! fuprobe records every IR frame as `ir_NNNNN.pgm` (640x360, GREY); the
//! replay path and the bench binary read them back with this. The format is
//! trivial enough that pulling an image crate into the daemon is not worth
//! the dependency (the `image` crate is a dev-dependency only).

use std::path::Path;

use crate::image::GrayImage;
use crate::{Error, Result};

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Parses a `P5` PGM from memory.
pub fn decode(bytes: &[u8]) -> Result<GrayImage> {
    let mut pos = 0usize;
    let mut fields: Vec<usize> = Vec::with_capacity(3);
    if bytes.len() < 2 || &bytes[..2] != b"P5" {
        return Err(Error::Image("not a P5 PGM".into()));
    }
    pos += 2;
    while fields.len() < 3 {
        // Skip whitespace and `#` comments between header tokens.
        while pos < bytes.len() {
            match bytes[pos] {
                b' ' | b'\t' | b'\r' | b'\n' => pos += 1,
                b'#' => {
                    while pos < bytes.len() && bytes[pos] != b'\n' {
                        pos += 1;
                    }
                }
                _ => break,
            }
        }
        let start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if start == pos {
            return Err(Error::Image("truncated PGM header".into()));
        }
        let v = std::str::from_utf8(&bytes[start..pos])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| Error::Image("bad PGM header number".into()))?;
        fields.push(v);
    }
    // Exactly one whitespace byte separates maxval from the raster.
    if pos >= bytes.len() || !bytes[pos].is_ascii_whitespace() {
        return Err(Error::Image("missing separator before PGM raster".into()));
    }
    pos += 1;
    let (w, h, maxval) = (fields[0], fields[1], fields[2]);
    if maxval != 255 {
        return Err(Error::Image(format!("unsupported PGM maxval {maxval}")));
    }
    let need = w
        .checked_mul(h)
        .ok_or_else(|| Error::Image("PGM size overflow".into()))?;
    let end = pos
        .checked_add(need)
        .ok_or_else(|| Error::Image("PGM size overflow".into()))?;
    let raster = bytes
        .get(pos..end)
        .ok_or_else(|| Error::Image("truncated PGM raster".into()))?;
    GrayImage::from_vec(w, h, raster.to_vec())
}

/// Reads a `P5` PGM file.
pub fn read(path: &Path) -> Result<GrayImage> {
    let bytes = std::fs::read(path).map_err(|e| io_err(path, e))?;
    decode(&bytes)
}

/// Serialises as `P5`.
pub fn encode(img: &GrayImage) -> Vec<u8> {
    let mut out = format!("P5\n{} {}\n255\n", img.width(), img.height()).into_bytes();
    out.extend_from_slice(img.as_slice());
    out
}

/// Writes a `P5` PGM file.
pub fn write(path: &Path, img: &GrayImage) -> Result<()> {
    std::fs::write(path, encode(img)).map_err(|e| io_err(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let img = GrayImage::from_vec(3, 2, vec![1, 2, 3, 4, 5, 6]).unwrap();
        let bytes = encode(&img);
        assert_eq!(decode(&bytes).unwrap(), img);
    }

    #[test]
    fn comments_and_whitespace() {
        let bytes = b"P5\n# a comment\n2 1\r\n255\n\x10\x20".to_vec();
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        assert_eq!(img.as_slice(), &[0x10, 0x20]);
    }

    #[test]
    fn rejects_truncated_and_wrong_magic() {
        assert!(decode(b"P6\n1 1\n255\n\0").is_err());
        assert!(decode(b"P5\n4 4\n255\n\0\0").is_err());
        assert!(decode(b"P5\n1 1\n65535\n\0\0").is_err());
        // w*h fits in usize but pos + w*h does not: an error, not a wrap.
        let huge = format!("P5\n{} 1\n255\n\0", usize::MAX);
        assert!(decode(huge.as_bytes()).is_err());
        let huge2 = format!("P5\n{} 2\n255\n\0", usize::MAX / 2);
        assert!(decode(huge2.as_bytes()).is_err());
    }

    #[test]
    fn matches_image_crate() {
        // The dev-only `image` crate is the oracle for our hand-written reader.
        let img = GrayImage::from_vec(4, 3, (0..12).map(|i| (i * 20) as u8).collect()).unwrap();
        let bytes = encode(&img);
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Pnm)
            .unwrap()
            .to_luma8();
        assert_eq!(decoded.dimensions(), (4, 3));
        assert_eq!(decoded.as_raw().as_slice(), img.as_slice());
    }
}
