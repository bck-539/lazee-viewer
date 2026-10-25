//! Grid thumbnails, made once per photo and kept in the local cache.
//!
//! JPEGs are decoded at reduced size straight from their DCT data (1/2, 1/4
//! or 1/8 scale), which skips most of the decoding work for a full-size
//! camera photo. This is the step that makes the first scan of 200,000
//! photos bearable on a laptop CPU.

use crate::{Error, Result};
use image::metadata::Orientation;
use image::{DynamicImage, GrayImage, RgbImage};
use std::io::Cursor;

/// Longest edge of a thumbnail in pixels: sharp in a ~180 px grid cell on a
/// 150% scaled display, and about 20 KB as a JPEG.
pub const THUMB_EDGE: u32 = 360;
const QUALITY: u8 = 80;

#[derive(Debug)]
pub struct Thumbnail {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Size of the source image before scaling, without rotation applied.
    pub source_width: u32,
    pub source_height: u32,
}

pub fn from_jpeg(data: &[u8], orientation: u8) -> Result<Thumbnail> {
    let (img, source_width, source_height) = decode_scaled(data, THUMB_EDGE)?;
    let mut img = if img.width().max(img.height()) > THUMB_EDGE { img.thumbnail(THUMB_EDGE, THUMB_EDGE) } else { img };
    if let Some(o) = Orientation::from_exif(orientation) {
        img.apply_orientation(o);
    }
    let rgb = img.to_rgb8();
    let mut jpeg = Vec::with_capacity(32 * 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
        .encode_image(&rgb)
        .map_err(|e| Error::Decode(e.to_string()))?;
    Ok(Thumbnail { jpeg, width: rgb.width(), height: rgb.height(), source_width, source_height })
}

/// Decodes a JPEG at the smallest DCT scale that still covers `edge` pixels.
pub fn decode_scaled(data: &[u8], edge: u32) -> Result<(DynamicImage, u32, u32)> {
    let mut dec = jpeg_decoder::Decoder::new(Cursor::new(data));
    dec.read_info().map_err(|e| Error::Decode(e.to_string()))?;
    let info = dec.info().ok_or_else(|| Error::Decode("missing JPEG header".into()))?;
    let (sw, sh) = (info.width as u32, info.height as u32);
    let req = edge.min(u16::MAX as u32) as u16;
    // Ask for `edge` on the short side so the long side keeps full detail.
    let (w, h) = dec.scale(req.min(info.width), req.min(info.height)).map_err(|e| Error::Decode(e.to_string()))?;
    let pixels = dec.decode().map_err(|e| Error::Decode(e.to_string()))?;
    let (w, h) = (w as u32, h as u32);
    let bad = || Error::Decode("pixel buffer has the wrong size".into());
    let img = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, pixels).ok_or_else(bad)?),
        jpeg_decoder::PixelFormat::L8 => DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, pixels).ok_or_else(bad)?),
        jpeg_decoder::PixelFormat::L16 => {
            let px = pixels.chunks_exact(2).map(|c| c[0]).collect();
            DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, px).ok_or_else(bad)?)
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            let px = cmyk_to_rgb(&pixels);
            DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, px).ok_or_else(bad)?)
        }
    };
    Ok((img, sw, sh))
}

/// Adobe-style inverted CMYK, which is what CMYK JPEGs in the wild use.
fn cmyk_to_rgb(px: &[u8]) -> Vec<u8> {
    px.chunks_exact(4)
        .flat_map(|c| {
            let k = c[3] as u16;
            [(c[0] as u16 * k / 255) as u8, (c[1] as u16 * k / 255) as u8, (c[2] as u16 * k / 255) as u8]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::tests::jpeg;

    #[test]
    fn makes_small_thumbnail_keeping_aspect() {
        let t = from_jpeg(&jpeg(1600, 1200), 1).unwrap();
        assert_eq!((t.width, t.height), (THUMB_EDGE, 270));
        assert_eq!((t.source_width, t.source_height), (1600, 1200));
        assert!(t.jpeg.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn applies_orientation() {
        let t = from_jpeg(&jpeg(1600, 1200), 6).unwrap();
        assert_eq!((t.width, t.height), (270, THUMB_EDGE));
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let t = from_jpeg(&jpeg(100, 50), 1).unwrap();
        assert_eq!((t.width, t.height), (100, 50));
    }

    #[test]
    fn rejects_garbage() {
        assert!(from_jpeg(b"\xFF\xD8garbage", 1).is_err());
    }
}
