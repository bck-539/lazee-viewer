//! Face detection and recognition, on the user's own PC.
//!
//! Models are plug-ins (see [`pack`]): a detector finds faces and their
//! landmarks, and an embedder turns each aligned face into a vector. Both
//! run on ONNX Runtime, which is loaded from a DLL at run time so the app
//! starts, and browsing works, even when it is missing.

pub mod align;
pub mod detect;
pub mod embed;
pub mod pack;

use crate::{Error, Result};
use image::RgbImage;
use ort::environment::Environment;
use pack::{Kind, Pack};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use detect::Detector;
pub use embed::Embedder;
pub use ort::environment::Environment as Runtime;

/// A face in pixel coordinates of the image it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Left eye, right eye, nose tip, left and right mouth corners.
    pub landmarks: [(f32, f32); 5],
}

static ENVIRONMENT: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

/// Loads ONNX Runtime from `library` the first time it is called; later
/// calls return the same runtime, whatever path they pass.
pub fn runtime(library: &Path) -> Result<&'static Environment> {
    ENVIRONMENT
        .get_or_init(|| {
            let builder = ort::init_from(library).map_err(|e| format!("cannot load {}: {e}", library.display()))?;
            builder.with_name("lazee").build().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| Error::Invalid(format!("ONNX Runtime: {e}")))
}

/// Where to look for the ONNX Runtime library: next to the app first, then
/// wherever `ORT_DYLIB_PATH` points.
pub fn runtime_candidates(app_dir: &Path) -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let mut out = vec![app_dir.join(name)];
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        out.push(PathBuf::from(p));
    }
    out
}

/// A detector and an embedder, ready to use.
pub struct Engine {
    pub detector: Detector,
    pub embedder: Embedder,
}

/// One face found in a photo, with its embedding.
#[derive(Debug, Clone)]
pub struct Found {
    pub face: Face,
    pub embedding: Vec<f32>,
}

impl Engine {
    pub fn new(env: &Environment, detector: Pack, embedder: Pack, threads: usize) -> Result<Self> {
        if detector.manifest.kind != Kind::Detector {
            return Err(Error::Invalid(format!("{} is not a face detector", detector.id())));
        }
        if embedder.manifest.kind != Kind::Embedder {
            return Err(Error::Invalid(format!("{} is not a face embedder", embedder.id())));
        }
        let session = |pack: &Pack| -> Result<ort::session::Session> {
            let fail = |e: &dyn std::fmt::Display| Error::Invalid(format!("cannot load model {}: {e}", pack.id()));
            let builder = ort::session::Session::builder(env).map_err(|e| fail(&e))?;
            let mut builder = builder.with_intra_threads(threads.max(1)).map_err(|e| fail(&e))?;
            builder.commit_from_file(pack.model_path()).map_err(|e| fail(&e))
        };
        Ok(Engine {
            detector: Detector::new(detector.clone(), session(&detector)?),
            embedder: Embedder::new(embedder.clone(), session(&embedder)?),
        })
    }

    /// Identifies the model pair. Faces found with a different pair are
    /// looked for again, so switching models never mixes embeddings that
    /// can't be compared.
    pub fn key(&self) -> String {
        format!("{}+{}", self.detector.id(), self.embedder.id())
    }

    /// Finds faces at least `min_size` pixels across (in `img`'s pixels)
    /// and computes their embeddings.
    pub fn analyze(&mut self, img: &RgbImage, min_size: f32) -> Result<Vec<Found>> {
        let faces: Vec<Face> = self.detector.detect(img)?.into_iter().filter(|f| f.w.min(f.h) >= min_size).collect();
        let embeddings = self.embedder.embed(img, &faces)?;
        Ok(faces.into_iter().zip(embeddings).map(|(face, embedding)| Found { face, embedding }).collect())
    }
}
