//! Face alignment: rotates, scales and crops a face so its eyes, nose and
//! mouth land where the embedding model was trained to see them.

use image::{Rgb, RgbImage};

/// Landmark positions in the standard 112×112 ArcFace crop: left eye,
/// right eye, nose tip, left and right mouth corners.
const ARCFACE_112: [(f32, f32); 5] =
    [(38.2946, 51.6963), (73.5318, 51.5014), (56.0252, 71.7366), (41.5493, 92.3655), (70.7299, 92.2041)];

pub fn known(name: Option<&str>) -> bool {
    matches!(name, Some("arcface-112"))
}

/// Cuts out an aligned `size`×`size` face from `img` using its landmarks.
pub fn crop(img: &RgbImage, landmarks: &[(f32, f32); 5], method: &str, size: u32) -> RgbImage {
    debug_assert!(known(Some(method)));
    let k = size as f32 / 112.0;
    let template: Vec<(f32, f32)> = ARCFACE_112.iter().map(|&(x, y)| (x * k, y * k)).collect();
    let t = Similarity::fit(&template, landmarks);
    RgbImage::from_fn(size, size, |x, y| {
        let (sx, sy) = t.apply(x as f32 + 0.5, y as f32 + 0.5);
        bilinear(img, sx - 0.5, sy - 0.5)
    })
}

/// A rotation, uniform scale and shift: `p' = [a -b; b a] (p - from) + to`.
#[derive(Debug, Clone, Copy)]
struct Similarity {
    a: f32,
    b: f32,
    from: (f32, f32),
    to: (f32, f32),
}

impl Similarity {
    /// Least-squares fit mapping `src` points onto `dst` points.
    fn fit(src: &[(f32, f32)], dst: &[(f32, f32)]) -> Self {
        let n = src.len() as f32;
        let mean = |p: &[(f32, f32)]| {
            let (sx, sy) = p.iter().fold((0.0, 0.0), |acc, q| (acc.0 + q.0, acc.1 + q.1));
            (sx / n, sy / n)
        };
        let (ms, md) = (mean(src), mean(dst));
        let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
        for (s, d) in src.iter().zip(dst) {
            let (sx, sy) = (s.0 - ms.0, s.1 - ms.1);
            let (dx, dy) = (d.0 - md.0, d.1 - md.1);
            num_a += sx * dx + sy * dy;
            num_b += sx * dy - sy * dx;
            den += sx * sx + sy * sy;
        }
        let (a, b) = if den > 0.0 { (num_a / den, num_b / den) } else { (1.0, 0.0) };
        Similarity { a, b, from: ms, to: md }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let (px, py) = (x - self.from.0, y - self.from.1);
        (self.a * px - self.b * py + self.to.0, self.b * px + self.a * py + self.to.1)
    }
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let px = |xi: i64, yi: i64| -> [f32; 3] {
        if xi < 0 || yi < 0 || xi >= w || yi >= h {
            [0.0; 3]
        } else {
            let p = img.get_pixel(xi as u32, yi as u32).0;
            [p[0] as f32, p[1] as f32, p[2] as f32]
        }
    };
    let (xi, yi) = (x0 as i64, y0 as i64);
    let (p00, p10, p01, p11) = (px(xi, yi), px(xi + 1, yi), px(xi, yi + 1), px(xi + 1, yi + 1));
    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
        out[c] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_rotation_scale_and_shift() {
        let src = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];
        // Rotate 90°, scale 2, move by (5, 7).
        let dst: Vec<_> = src.iter().map(|&(x, y)| (-2.0 * y + 5.0, 2.0 * x + 7.0)).collect();
        let t = Similarity::fit(&src, &dst);
        for (s, d) in src.iter().zip(&dst) {
            let (x, y) = t.apply(s.0, s.1);
            assert!((x - d.0).abs() < 1e-4 && (y - d.1).abs() < 1e-4);
        }
    }

    #[test]
    fn aligned_crop_of_a_template_face_is_identity() {
        // An image that is itself the template: cropping it changes nothing.
        let img = RgbImage::from_fn(112, 112, |x, y| Rgb([x as u8, y as u8, 128]));
        let out = crop(&img, &ARCFACE_112, "arcface-112", 112);
        assert_eq!(out.get_pixel(30, 40), img.get_pixel(30, 40));
        assert_eq!(out.get_pixel(100, 5), img.get_pixel(100, 5));
    }
}
