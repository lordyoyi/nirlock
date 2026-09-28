//! FUEMB1 template sets, as written by fuprobe `enroll`
//! (`phase0/pipeline.cpp::save_template_set`) and described by the
//! directory's `manifest.json`.
//!
//! File layout of `<embedder>_<variant>.emb`:
//! `8-byte magic "FUEMB1\0\0"`, `u32 LE dim`, `u32 LE n`, then `n` rows of
//! `dim` little-endian `f32`, each row already L2-normalised. A sibling
//! `<embedder>_<variant>.src.tsv` lists `session\tindex` per row.
//!
//! `manifest.json` is the contract: when it exists, each set is read from
//! the file it names (a bare file name inside the directory, no path
//! components) and its `dim`/`count` are cross-checked. The conventional
//! `<embedder>_<variant>.emb` name is used only when there is no manifest.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::math::dot;
use crate::{Error, Result};

const MAGIC: &[u8; 8] = b"FUEMB1\0\0";
const MAX_DIM: u32 = 4096;
const MAX_ROWS: u32 = 100_000;

/// One `sets[]` entry of `manifest.json`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SetInfo {
    pub embedder: String,
    pub variant: String,
    pub file: String,
    pub dim: usize,
    pub count: usize,
}

/// The parts of `manifest.json` this crate needs (unknown fields ignored).
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub sets: Vec<SetInfo>,
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub sessions: Vec<String>,
}

/// A matrix of unit rows.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateSet {
    pub embedder: String,
    pub variant: String,
    pub dim: usize,
    rows: Vec<f32>,
    /// `(session, index)` per row, when the `.src.tsv` sidecar exists.
    pub sources: Vec<(String, i32)>,
}

impl TemplateSet {
    pub fn len(&self) -> usize {
        self.rows.len().checked_div(self.dim).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, i: usize) -> &[f32] {
        &self.rows[i * self.dim..(i + 1) * self.dim]
    }

    pub fn rows(&self) -> impl Iterator<Item = &[f32]> {
        self.rows.chunks_exact(self.dim)
    }

    /// Maximum dot product over rows (both sides unit vectors: this is the
    /// MAX-cosine rule of DESIGN §1.4). `None` when the set is empty or the
    /// dimension does not match.
    pub fn max_cosine(&self, emb: &[f32]) -> Option<f64> {
        if self.is_empty() || emb.len() != self.dim {
            return None;
        }
        self.rows()
            .map(|r| dot(emb, r))
            .fold(None, |m, d| Some(m.map_or(d, |m: f64| m.max(d))))
    }

    /// The row that came from `(session, index)` according to the
    /// `.src.tsv` sidecar, if any. The parity check uses this to compare a
    /// frame's fresh embedding with the row fuprobe stored for that very
    /// frame ("own row"), which a max over all rows cannot distinguish from
    /// a near-duplicate neighbour.
    pub fn row_for(&self, session: &str, index: i32) -> Option<&[f32]> {
        let i = self
            .sources
            .iter()
            .position(|(s, i)| s == session && *i == index)?;
        (i < self.len()).then(|| self.row(i))
    }

    /// `(max, mean, n)` over rows, optionally skipping rows whose source
    /// session equals `skip_session` (fuprobe `cosine_stats`).
    pub fn cosine_stats(
        &self,
        emb: &[f32],
        skip_session: Option<&str>,
    ) -> Option<(f64, f64, usize)> {
        if self.is_empty() || emb.len() != self.dim {
            return None;
        }
        let (mut mx, mut sum, mut n) = (f64::MIN, 0.0, 0usize);
        for (i, r) in self.rows().enumerate() {
            if let Some(skip) = skip_session
                && self.sources.get(i).is_some_and(|(s, _)| s == skip)
            {
                continue;
            }
            let d = dot(emb, r);
            mx = mx.max(d);
            sum += d;
            n += 1;
        }
        (n > 0).then_some((mx, sum / n as f64, n))
    }
}

fn bad(path: &Path, msg: impl Into<String>) -> Error {
    Error::Template {
        path: path.display().to_string(),
        msg: msg.into(),
    }
}

/// Writes `<dir>/<embedder>_<variant>.emb` in the FUEMB1 format the loader
/// above expects, plus a `manifest.json` naming it. `rows` is `n` rows of
/// `dim` floats, already L2-normalised (the embedder does that).
///
/// Written to a temporary file and renamed, so an interrupted enrolment
/// cannot leave a half-written template that the daemon would then load and
/// compare faces against.
pub fn write_set(
    dir: &Path,
    embedder: &str,
    variant: &str,
    dim: usize,
    n: usize,
    rows: &[f32],
) -> Result<()> {
    if dim == 0 || dim > MAX_DIM as usize || n == 0 || n > MAX_ROWS as usize {
        return Err(bad(dir, format!("implausible dim={dim} n={n}")));
    }
    if rows.len() != dim * n {
        return Err(bad(
            dir,
            format!("{} floats for dim={dim} n={n}", rows.len()),
        ));
    }
    if rows.iter().any(|v| !v.is_finite()) {
        return Err(bad(dir, "non-finite value"));
    }
    let mut bytes = Vec::with_capacity(16 + rows.len() * 4);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(dim as u32).to_le_bytes());
    bytes.extend_from_slice(&(n as u32).to_le_bytes());
    for v in rows {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let name = format!("{embedder}_{variant}.emb");
    let tmp = dir.join(format!(".{name}.tmp"));
    let dst = dir.join(&name);
    let io = |path: &Path, e: std::io::Error| Error::Io {
        path: path.display().to_string(),
        source: e,
    };
    std::fs::write(&tmp, &bytes).map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, &dst).map_err(|e| io(&dst, e))?;

    let manifest = format!(
        "{{\n  \"sets\": [\n    {{\"embedder\": \"{embedder}\", \"variant\": \"{variant}\", \
         \"file\": \"{name}\", \"dim\": {dim}, \"count\": {n}}}\n  ]\n}}\n"
    );
    let mtmp = dir.join(".manifest.json.tmp");
    let mdst = dir.join("manifest.json");
    std::fs::write(&mtmp, manifest).map_err(|e| io(&mtmp, e))?;
    std::fs::rename(&mtmp, &mdst).map_err(|e| io(&mdst, e))?;
    Ok(())
}

/// Parses an FUEMB1 blob.
pub fn decode_emb(path: &Path, bytes: &[u8]) -> Result<(usize, Vec<f32>)> {
    if bytes.len() < 16 || &bytes[..8] != MAGIC {
        return Err(bad(path, "bad magic or truncated header"));
    }
    let le32 = |at: usize| -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(&bytes[at..at + 4]);
        u32::from_le_bytes(b)
    };
    let (dim, n) = (le32(8), le32(12));
    if dim == 0 || dim > MAX_DIM || n > MAX_ROWS {
        return Err(bad(path, format!("implausible dim={dim} n={n}")));
    }
    let need = 16 + (dim as usize) * (n as usize) * 4;
    if bytes.len() != need {
        return Err(bad(
            path,
            format!(
                "size mismatch: {} bytes, header says {need} (dim={dim}, n={n})",
                bytes.len()
            ),
        ));
    }
    let rows: Vec<f32> = bytes[16..need]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    if rows.iter().any(|v| !v.is_finite()) {
        return Err(bad(path, "non-finite value"));
    }
    Ok((dim as usize, rows))
}

/// Loads `<dir>/<embedder>_<variant>.emb` (+ optional `.src.tsv`), the
/// conventional fuprobe name (manifest-less directories only).
pub fn load_set(dir: &Path, embedder: &str, variant: &str) -> Result<TemplateSet> {
    load_set_file(dir, embedder, variant, &format!("{embedder}_{variant}.emb"))
}

/// Is `file` a bare file name (one normal path component, no `..`, no
/// separators)? The manifest may only name files inside its own directory.
fn bare_file_name(file: &str) -> bool {
    let mut comps = Path::new(file).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

/// Loads `<dir>/<file>` as the `(embedder, variant)` set, plus the
/// `<stem>.src.tsv` sidecar when it exists. `file` must be a bare name.
pub fn load_set_file(dir: &Path, embedder: &str, variant: &str, file: &str) -> Result<TemplateSet> {
    if !bare_file_name(file) {
        return Err(bad(
            &dir.join("manifest.json"),
            format!("set file {file:?} is not a bare file name"),
        ));
    }
    let emb_path: PathBuf = dir.join(file);
    let stem = file.strip_suffix(".emb").unwrap_or(file);
    let tsv_path = dir.join(format!("{stem}.src.tsv"));
    let bytes = std::fs::read(&emb_path).map_err(|e| Error::Io {
        path: emb_path.display().to_string(),
        source: e,
    })?;
    let (dim, rows) = decode_emb(&emb_path, &bytes)?;
    let n = rows.len() / dim;
    let mut sources = Vec::new();
    if let Ok(tsv) = std::fs::read_to_string(tsv_path) {
        for line in tsv.lines() {
            if let Some((s, i)) = line.split_once('\t') {
                sources.push((s.to_string(), i.trim().parse().unwrap_or(-1)));
            }
        }
        sources.resize(n, (String::new(), -1));
    }
    Ok(TemplateSet {
        embedder: embedder.into(),
        variant: variant.into(),
        dim,
        rows,
        sources,
    })
}

/// A whole template directory.
#[derive(Debug)]
pub struct Template {
    pub dir: PathBuf,
    pub manifest: Option<Manifest>,
    pub sets: Vec<TemplateSet>,
}

impl Template {
    /// Reads `manifest.json` when present and every set it lists; without a
    /// manifest, probes the four fuprobe names (`{sface,auraface}_{lit,diff}`).
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join("manifest.json");
        let manifest: Option<Manifest> = match std::fs::read(&manifest_path) {
            Ok(b) => {
                Some(serde_json::from_slice(&b).map_err(|e| bad(&manifest_path, e.to_string()))?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(Error::Io {
                    path: manifest_path.display().to_string(),
                    source: e,
                });
            }
        };
        let mut sets = Vec::new();
        match &manifest {
            Some(m) => {
                for s in &m.sets {
                    let set = load_set_file(dir, &s.embedder, &s.variant, &s.file)?;
                    if set.dim != s.dim || set.len() != s.count {
                        return Err(bad(
                            &dir.join(&s.file),
                            format!(
                                "manifest says dim={} count={}, file has dim={} rows={}",
                                s.dim,
                                s.count,
                                set.dim,
                                set.len()
                            ),
                        ));
                    }
                    sets.push(set);
                }
            }
            None => {
                for e in ["sface", "auraface"] {
                    for v in ["lit", "diff"] {
                        if dir.join(format!("{e}_{v}.emb")).exists() {
                            sets.push(load_set(dir, e, v)?);
                        }
                    }
                }
            }
        }
        if sets.is_empty() {
            return Err(bad(dir, "no template sets (*.emb) found"));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            sets,
        })
    }

    pub fn find(&self, embedder: &str, variant: &str) -> Option<&TemplateSet> {
        self.sets
            .iter()
            .find(|s| s.embedder == embedder && s.variant == variant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::l2_normalise;

    fn emb_bytes(dim: u32, rows: &[Vec<f32>]) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&dim.to_le_bytes());
        b.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for r in rows {
            for v in r {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn decode_and_max_cosine() {
        let mut r0 = vec![1.0f32, 0.0, 0.0];
        let mut r1 = vec![1.0f32, 1.0, 0.0];
        l2_normalise(&mut r0);
        l2_normalise(&mut r1);
        let bytes = emb_bytes(3, &[r0.clone(), r1.clone()]);
        let (dim, rows) = decode_emb(Path::new("x.emb"), &bytes).unwrap();
        assert_eq!(dim, 3);
        let set = TemplateSet {
            embedder: "t".into(),
            variant: "lit".into(),
            dim,
            rows,
            sources: vec![("a".into(), 0), ("b".into(), 1)],
        };
        assert_eq!(set.len(), 2);
        let q = r1.clone();
        assert!((set.max_cosine(&q).unwrap() - 1.0).abs() < 1e-6);
        let (mx, mean, n) = set.cosine_stats(&q, Some("b")).unwrap();
        assert_eq!(n, 1);
        assert!((mx - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((mean - mx).abs() < 1e-12);
        assert!(set.max_cosine(&[1.0, 0.0]).is_none());
    }

    #[test]
    fn rejects_bad_files() {
        let p = Path::new("x.emb");
        assert!(decode_emb(p, b"FUEMB0\0\0").is_err());
        let mut b = emb_bytes(3, &[vec![1.0, 0.0, 0.0]]);
        b.truncate(b.len() - 1);
        assert!(decode_emb(p, &b).is_err());
        // Trailing bytes after the declared rows are rejected too.
        let mut b = emb_bytes(3, &[vec![1.0, 0.0, 0.0]]);
        b.push(0);
        assert!(decode_emb(p, &b).is_err());
        let b = emb_bytes(0, &[]);
        assert!(decode_emb(p, &b).is_err());
        let b = emb_bytes(1, &[vec![f32::NAN]]);
        assert!(decode_emb(p, &b).is_err());
    }

    #[test]
    fn loads_directory_with_manifest() {
        let dir = std::env::temp_dir().join(format!("nirlock-tpl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auraface_lit.emb"),
            emb_bytes(2, &[vec![0.6, 0.8]]),
        )
        .unwrap();
        std::fs::write(dir.join("auraface_lit.src.tsv"), "sess-a\t7\n").unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"sets":[{"embedder":"auraface","variant":"lit","file":"auraface_lit.emb","dim":2,"count":1}],"extra":1}"#,
        )
        .unwrap();
        let t = Template::load(&dir).unwrap();
        let s = t.find("auraface", "lit").unwrap();
        assert_eq!(s.sources, vec![("sess-a".to_string(), 7)]);
        assert!((s.max_cosine(&[0.6, 0.8]).unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(s.row_for("sess-a", 7), Some(&[0.6f32, 0.8][..]));
        assert_eq!(s.row_for("sess-a", 8), None);
        assert_eq!(s.row_for("sess-b", 7), None);
        // The manifest's `file` is what gets opened, not the conventional
        // name: a renamed set must load from the name the manifest gives.
        std::fs::rename(dir.join("auraface_lit.emb"), dir.join("aura-v2.emb")).unwrap();
        std::fs::rename(
            dir.join("auraface_lit.src.tsv"),
            dir.join("aura-v2.src.tsv"),
        )
        .unwrap();
        assert!(
            Template::load(&dir).is_err(),
            "old name must not be substituted"
        );
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"sets":[{"embedder":"auraface","variant":"lit","file":"aura-v2.emb","dim":2,"count":1}]}"#,
        )
        .unwrap();
        let t = Template::load(&dir).unwrap();
        let s = t.find("auraface", "lit").unwrap();
        assert_eq!(s.sources, vec![("sess-a".to_string(), 7)]);
        // A manifest may not point outside its directory.
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"sets":[{"embedder":"auraface","variant":"lit","file":"../aura-v2.emb","dim":2,"count":1}]}"#,
        )
        .unwrap();
        assert!(Template::load(&dir).is_err());
        std::fs::rename(dir.join("aura-v2.emb"), dir.join("auraface_lit.emb")).unwrap();
        std::fs::rename(
            dir.join("aura-v2.src.tsv"),
            dir.join("auraface_lit.src.tsv"),
        )
        .unwrap();
        // Manifest/file disagreement is an error.
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"sets":[{"embedder":"auraface","variant":"lit","file":"auraface_lit.emb","dim":2,"count":5}]}"#,
        )
        .unwrap();
        assert!(Template::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
