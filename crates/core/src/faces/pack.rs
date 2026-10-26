//! Model packs: face models as drop-in plug-ins.
//!
//! A pack is a folder holding an ONNX model, its licence, and a `model.toml`
//! that says what the model is and how to feed it. The app looks for packs
//! in the `models` folder next to the executable and in the library's own
//! `models` folder, so trying a different model means dropping a folder in
//! and picking it in settings: no rebuild.
//!
//! ```toml
//! id = "yunet-2023mar"
//! name = "YuNet (OpenCV Zoo, 2023)"
//! kind = "detector"                  # or "embedder"
//! file = "face_detection_yunet_2023mar.onnx"
//! licence = "MIT"
//!
//! [input]
//! width = 640
//! height = 640
//! channels = "bgr"                   # colour order the model expects
//! mean = [0.0, 0.0, 0.0]             # pixel = (value - mean) / std, values 0-255
//! std = [1.0, 1.0, 1.0]
//!
//! [output]
//! format = "yunet"                   # which decoder reads the outputs
//! score_threshold = 0.8
//! ```
//!
//! Embedders add `align = "arcface-112"` under `[input]` and `dim`,
//! `match_threshold` and `cluster_threshold` under `[output]`.

use crate::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "model.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Detector,
    Embedder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channels {
    Rgb,
    Bgr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub file: String,
    pub licence: String,
    #[serde(default)]
    pub url: Option<String>,
    pub input: InputSpec,
    pub output: OutputSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InputSpec {
    /// Input tensor name; the model's first input when left out.
    #[serde(default)]
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    pub channels: Channels,
    #[serde(default)]
    pub mean: [f32; 3],
    #[serde(default = "ones")]
    pub std: [f32; 3],
    /// How faces are cropped for an embedder, such as `arcface-112`.
    #[serde(default)]
    pub align: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputSpec {
    /// Decoder for detector outputs (`yunet`), or `embedding`.
    pub format: String,
    #[serde(default = "default_score")]
    pub score_threshold: f32,
    #[serde(default = "default_nms")]
    pub nms_iou: f32,
    /// Length of an embedding.
    #[serde(default)]
    pub dim: usize,
    /// Cosine similarity above which two faces are the same person.
    #[serde(default)]
    pub match_threshold: f32,
    /// Stricter similarity used when grouping faces without names, so
    /// that two people are rarely put in one group.
    #[serde(default)]
    pub cluster_threshold: f32,
}

fn ones() -> [f32; 3] {
    [1.0; 3]
}
fn default_score() -> f32 {
    0.8
}
fn default_nms() -> f32 {
    0.3
}

/// A model pack found on disk.
#[derive(Debug, Clone)]
pub struct Pack {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl Pack {
    pub fn load(dir: &Path) -> Result<Pack> {
        let text = std::fs::read_to_string(dir.join(MANIFEST))?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", dir.join(MANIFEST).display())))?;
        let pack = Pack { manifest, dir: dir.to_path_buf() };
        pack.validate()?;
        Ok(pack)
    }

    pub fn model_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.file)
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    fn validate(&self) -> Result<()> {
        let m = &self.manifest;
        let bad = |why: &str| Err(Error::Invalid(format!("model pack {}: {why}", self.dir.display())));
        if m.id.is_empty() || m.id.contains(['/', '\\', '+']) {
            return bad("id must be non-empty and contain no slashes or '+'");
        }
        if !self.model_path().is_file() {
            return bad(&format!("model file {} is missing", m.file));
        }
        if m.input.width == 0 || m.input.height == 0 || m.input.std.contains(&0.0) {
            return bad("input size and std must be non-zero");
        }
        match m.kind {
            Kind::Detector if !super::detect::known_format(&m.output.format) => {
                bad(&format!("unknown detector output format '{}'", m.output.format))
            }
            Kind::Embedder if m.output.dim == 0 => bad("embedders need output.dim"),
            Kind::Embedder if !super::align::known(m.input.align.as_deref()) => {
                bad(&format!("unknown alignment '{}'", m.input.align.as_deref().unwrap_or("")))
            }
            _ => Ok(()),
        }
    }
}

/// All valid packs in the given folders. Broken packs are logged and
/// skipped, so one bad download doesn't take face recognition down.
pub fn discover(dirs: &[PathBuf]) -> Vec<Pack> {
    let mut packs: Vec<Pack> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut subdirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subdirs.sort();
        for sub in subdirs {
            if !sub.join(MANIFEST).is_file() {
                continue;
            }
            match Pack::load(&sub) {
                // The first folder wins, so a user's pack can't be shadowed.
                Ok(p) if packs.iter().any(|q| q.id() == p.id()) => {
                    log::warn!("ignoring {}: model id {} is already taken", sub.display(), p.id())
                }
                Ok(p) => packs.push(p),
                Err(e) => log::warn!("{e}"),
            }
        }
    }
    packs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pack(root: &Path, id: &str, extra: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.onnx"), b"x").unwrap();
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "id = \"{id}\"\nname = \"Test\"\nkind = \"detector\"\nfile = \"m.onnx\"\nlicence = \"MIT\"\n\
                 [input]\nwidth = 640\nheight = 640\nchannels = \"bgr\"\n[output]\nformat = \"yunet\"\n{extra}"
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn discovers_packs_and_skips_broken_ones() {
        let root = tempfile::tempdir().unwrap();
        write_pack(root.path(), "good", "");
        let bad = write_pack(root.path(), "bad", "");
        std::fs::remove_file(bad.join("m.onnx")).unwrap();
        std::fs::create_dir_all(root.path().join("not-a-pack")).unwrap();

        let packs = discover(&[root.path().to_path_buf()]);
        assert_eq!(packs.len(), 1);
        let p = &packs[0];
        assert_eq!(p.id(), "good");
        assert_eq!(p.manifest.kind, Kind::Detector);
        assert_eq!(p.manifest.input.std, [1.0; 3]);
        assert_eq!(p.manifest.output.score_threshold, 0.8);
    }

    #[test]
    fn rejects_unknown_formats() {
        let root = tempfile::tempdir().unwrap();
        let dir = write_pack(root.path(), "x", "");
        let text = std::fs::read_to_string(dir.join(MANIFEST)).unwrap().replace("yunet", "mystery");
        std::fs::write(dir.join(MANIFEST), text).unwrap();
        assert!(Pack::load(&dir).is_err());
    }
}
