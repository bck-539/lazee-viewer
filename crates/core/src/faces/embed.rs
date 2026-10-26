//! Face embeddings: a short vector per face, where faces of the same
//! person point in nearly the same direction.

use super::align;
use super::detect::{ort_err, to_nchw};
use super::pack::Pack;
use super::Face;
use crate::{Error, Result};
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

pub struct Embedder {
    pack: Pack,
    session: Session,
    input_name: String,
}

impl Embedder {
    pub(super) fn new(pack: Pack, session: Session) -> Self {
        let input_name = pack.manifest.input.name.clone().unwrap_or_else(|| session.inputs()[0].name().to_string());
        Embedder { pack, session, input_name }
    }

    pub fn id(&self) -> &str {
        self.pack.id()
    }

    pub fn dim(&self) -> usize {
        self.pack.manifest.output.dim
    }

    pub fn match_threshold(&self) -> f32 {
        self.pack.manifest.output.match_threshold
    }

    pub fn cluster_threshold(&self) -> f32 {
        self.pack.manifest.output.cluster_threshold
    }

    /// The aligned face crop this model sees.
    pub fn aligned(&self, img: &RgbImage, face: &Face) -> RgbImage {
        let spec = &self.pack.manifest.input;
        align::crop(img, &face.landmarks, spec.align.as_deref().unwrap_or_default(), spec.width)
    }

    /// Unit-length embeddings for `faces` found in `img`, one per face.
    pub fn embed(&mut self, img: &RgbImage, faces: &[Face]) -> Result<Vec<Vec<f32>>> {
        let spec = self.pack.manifest.input.clone();
        let dim = self.dim();
        let mut out = Vec::with_capacity(faces.len());
        for face in faces {
            let crop = self.aligned(img, face);
            let input =
                Tensor::from_array(([1usize, 3, spec.height as usize, spec.width as usize], to_nchw(&crop, &spec)))
                    .map_err(ort_err)?;
            let outputs = self.session.run(ort::inputs![self.input_name.as_str() => input]).map_err(ort_err)?;
            let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
            if data.len() != dim {
                return Err(Error::Invalid(format!("embedding has {} values, the pack says {dim}", data.len())));
            }
            out.push(normalize(data));
        }
        Ok(out)
    }
}

pub fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

/// Cosine similarity of two unit vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
