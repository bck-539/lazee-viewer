//! Face detection: finds face boxes and five landmarks (eyes, nose tip,
//! mouth corners) in a photo.
//!
//! The model itself comes from a pack; this file holds the decoders that
//! turn a model's raw outputs into faces. Supporting a new detector family
//! means adding a decoder here and naming it in the pack's `output.format`.

use super::pack::{Channels, InputSpec, Pack};
use super::Face;
use crate::{Error, Result};
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

const FORMATS: &[&str] = &["yunet"];

pub fn known_format(format: &str) -> bool {
    FORMATS.contains(&format)
}

pub struct Detector {
    pack: Pack,
    session: Session,
    input_name: String,
}

impl Detector {
    pub(super) fn new(pack: Pack, session: Session) -> Self {
        let input_name = pack.manifest.input.name.clone().unwrap_or_else(|| session.inputs()[0].name().to_string());
        Detector { pack, session, input_name }
    }

    pub fn id(&self) -> &str {
        self.pack.id()
    }

    /// Faces in `img`, in its pixel coordinates, best first.
    pub fn detect(&mut self, img: &RgbImage) -> Result<Vec<Face>> {
        let spec = &self.pack.manifest.input;
        let (tensor, scale) = letterbox(img, spec);
        let input =
            Tensor::from_array(([1usize, 3, spec.height as usize, spec.width as usize], tensor)).map_err(ort_err)?;
        let outputs = self.session.run(ort::inputs![self.input_name.as_str() => input]).map_err(ort_err)?;
        let get = |name: &str| -> Result<&[f32]> {
            let v = outputs.get(name).ok_or_else(|| Error::Invalid(format!("model has no output '{name}'")))?;
            Ok(v.try_extract_tensor::<f32>().map_err(ort_err)?.1)
        };
        let out = &self.pack.manifest.output;
        let mut faces = match out.format.as_str() {
            "yunet" => decode_yunet(&get, spec.width, spec.height, out.score_threshold)?,
            other => return Err(Error::Invalid(format!("unknown detector format {other}"))),
        };
        faces = nms(faces, out.nms_iou);
        // Back from letterbox space to the photo's own pixels.
        for f in &mut faces {
            f.x /= scale;
            f.y /= scale;
            f.w /= scale;
            f.h /= scale;
            for p in &mut f.landmarks {
                p.0 /= scale;
                p.1 /= scale;
            }
        }
        Ok(faces)
    }
}

pub(super) fn ort_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("face model: {e}"))
}

/// Scales the image to fit the model's input, keeping its shape, and pads
/// the rest with black. Returns an NCHW tensor and the scale used.
fn letterbox(img: &RgbImage, spec: &InputSpec) -> (Vec<f32>, f32) {
    let (w, h) = (spec.width, spec.height);
    let scale = (w as f32 / img.width() as f32).min(h as f32 / img.height() as f32);
    let nw = ((img.width() as f32 * scale).round() as u32).clamp(1, w);
    let nh = ((img.height() as f32 * scale).round() as u32).clamp(1, h);
    let resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
    let mut padded = RgbImage::new(w, h);
    image::imageops::replace(&mut padded, &resized, 0, 0);
    (to_nchw(&padded, spec), scale)
}

/// Planar float tensor in the colour order and normalisation the model
/// asks for.
pub(super) fn to_nchw(img: &RgbImage, spec: &InputSpec) -> Vec<f32> {
    let plane = (img.width() * img.height()) as usize;
    let mut out = vec![0f32; plane * 3];
    let order: [usize; 3] = match spec.channels {
        Channels::Rgb => [0, 1, 2],
        Channels::Bgr => [2, 1, 0],
    };
    for (i, px) in img.pixels().enumerate() {
        for (c, &src) in order.iter().enumerate() {
            out[c * plane + i] = (px.0[src] as f32 - spec.mean[c]) / spec.std[c];
        }
    }
    out
}

/// YuNet (OpenCV Zoo, 2023): per-anchor class, objectness, box and
/// landmark outputs at strides 8, 16 and 32.
fn decode_yunet<'a>(
    get: &dyn Fn(&str) -> Result<&'a [f32]>,
    width: u32,
    height: u32,
    threshold: f32,
) -> Result<Vec<Face>> {
    let mut faces = Vec::new();
    for stride in [8u32, 16, 32] {
        let cols = (width / stride) as usize;
        let rows = (height / stride) as usize;
        let cls = get(&format!("cls_{stride}"))?;
        let obj = get(&format!("obj_{stride}"))?;
        let bbox = get(&format!("bbox_{stride}"))?;
        let kps = get(&format!("kps_{stride}"))?;
        let n = rows * cols;
        if cls.len() < n || obj.len() < n || bbox.len() < n * 4 || kps.len() < n * 10 {
            return Err(Error::Invalid(format!("YuNet output at stride {stride} has the wrong size")));
        }
        let s = stride as f32;
        for idx in 0..n {
            let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
            if score < threshold {
                continue;
            }
            let (r, c) = ((idx / cols) as f32, (idx % cols) as f32);
            let b = &bbox[idx * 4..idx * 4 + 4];
            let cx = (c + b[0]) * s;
            let cy = (r + b[1]) * s;
            let w = b[2].exp() * s;
            let h = b[3].exp() * s;
            let k = &kps[idx * 10..idx * 10 + 10];
            let mut landmarks = [(0.0, 0.0); 5];
            for (j, p) in landmarks.iter_mut().enumerate() {
                *p = ((k[2 * j] + c) * s, (k[2 * j + 1] + r) * s);
            }
            faces.push(Face { x: cx - w / 2.0, y: cy - h / 2.0, w, h, score, landmarks });
        }
    }
    Ok(faces)
}

fn iou(a: &Face, b: &Face) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Non-maximum suppression: keeps the best of each group of overlapping
/// boxes.
fn nms(mut faces: Vec<Face>, threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in faces {
        if kept.iter().all(|k| iou(k, &f) <= threshold) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, y: f32, s: f32, score: f32) -> Face {
        Face { x, y, w: s, h: s, score, landmarks: [(0.0, 0.0); 5] }
    }

    #[test]
    fn nms_keeps_best_of_overlaps() {
        let kept = nms(vec![face(0.0, 0.0, 10.0, 0.8), face(1.0, 1.0, 10.0, 0.9), face(50.0, 50.0, 10.0, 0.7)], 0.3);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9);
        assert_eq!(kept[1].x, 50.0);
    }

    #[test]
    fn tensor_layout_and_colour_order() {
        let img = RgbImage::from_pixel(2, 1, image::Rgb([10, 20, 30]));
        let spec = InputSpec {
            name: None,
            width: 2,
            height: 1,
            channels: Channels::Bgr,
            mean: [0.0; 3],
            std: [1.0; 3],
            align: None,
        };
        assert_eq!(to_nchw(&img, &spec), vec![30.0, 30.0, 20.0, 20.0, 10.0, 10.0]);
        let spec = InputSpec { channels: Channels::Rgb, mean: [10.0; 3], std: [2.0; 3], ..spec };
        assert_eq!(to_nchw(&img, &spec), vec![0.0, 0.0, 5.0, 5.0, 10.0, 10.0]);
    }
}
