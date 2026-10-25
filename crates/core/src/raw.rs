//! Embedded JPEG previews from RAW files.
//!
//! Every camera RAW carries a JPEG rendered by the camera, usually full size
//! or close to it. Reading that preview is many times faster than decoding
//! the sensor data, and it reads only a fraction of the file, which matters
//! when the photos sit on a NAS. Full RAW decoding (LibRaw) comes later.
//!
//! The readers here seek through the file instead of loading it, except for
//! the last-resort scan used for formats we don't parse yet.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Largest preview we are willing to read.
const MAX_PREVIEW: u64 = 64 * 1024 * 1024;
/// Largest file the fallback scan will load into memory.
const MAX_SCAN: u64 = 256 * 1024 * 1024;

const CR3_PREVIEW_UUID: [u8; 16] =
    [0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16];

/// Returns the largest baseline JPEG embedded in a RAW file, if any.
pub fn extract_preview(file: &mut File, len: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 16];
    file.seek(SeekFrom::Start(0))?;
    if len < 16 {
        return Ok(None);
    }
    file.read_exact(&mut head)?;

    let candidates = if head.starts_with(b"FUJIFILMCCD-RAW") {
        raf_candidates(file)?
    } else if &head[4..8] == b"ftyp" {
        return cr3_preview(file, len);
    } else if is_tiff_like(&head) {
        tiff_candidates(file, len, head[0] == b'I')?
    } else {
        Vec::new()
    };

    let mut best: Option<(u64, u64)> = None;
    for (off, size) in candidates {
        if !(2..=MAX_PREVIEW).contains(&size) || off.saturating_add(size) > len {
            continue;
        }
        if best.is_some_and(|(_, b)| b >= size) {
            continue;
        }
        if is_baseline_jpeg_at(file, off, size)? {
            best = Some((off, size));
        }
    }
    match best {
        Some((off, size)) => {
            let mut buf = vec![0; size as usize];
            file.seek(SeekFrom::Start(off))?;
            file.read_exact(&mut buf)?;
            Ok(Some(buf))
        }
        None => scan_fallback(file, len),
    }
}

fn is_tiff_like(h: &[u8]) -> bool {
    matches!(&h[..4], b"II*\0" | b"MM\0*" | b"IIRO" | b"IIRS" | b"MMOR" | b"IIU\0")
}

fn raf_candidates(f: &mut File) -> std::io::Result<Vec<(u64, u64)>> {
    let mut b = [0u8; 8];
    f.seek(SeekFrom::Start(84))?;
    f.read_exact(&mut b)?;
    let off = u32::from_be_bytes(b[0..4].try_into().unwrap()) as u64;
    let len = u32::from_be_bytes(b[4..8].try_into().unwrap()) as u64;
    Ok(vec![(off, len)])
}

struct Tiff<'a> {
    f: &'a mut File,
    le: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    raw: [u8; 4],
}

impl Tiff<'_> {
    fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }

    fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }

    /// The entry's first value as an integer, for SHORT and LONG types. For
    /// any other type with more than four bytes of data, the data offset.
    fn value(&self, e: &Entry) -> u32 {
        match e.typ {
            3 | 8 => self.u16(&e.raw) as u32,
            _ => self.u32(&e.raw),
        }
    }

    fn read_ifd(&mut self, off: u64) -> std::io::Result<(Vec<Entry>, u32)> {
        let mut n = [0u8; 2];
        self.f.seek(SeekFrom::Start(off))?;
        self.f.read_exact(&mut n)?;
        let count = self.u16(&n).min(1000) as usize;
        let mut buf = vec![0u8; count * 12 + 4];
        self.f.read_exact(&mut buf)?;
        let entries = buf[..count * 12]
            .chunks_exact(12)
            .map(|c| Entry {
                tag: self.u16(&c[0..2]),
                typ: self.u16(&c[2..4]),
                count: self.u32(&c[4..8]),
                raw: c[8..12].try_into().unwrap(),
            })
            .collect();
        let next = self.u32(&buf[count * 12..]);
        Ok((entries, next))
    }

    fn u32_array(&mut self, e: &Entry) -> std::io::Result<Vec<u32>> {
        if e.count <= 1 {
            return Ok(vec![self.value(e)]);
        }
        let n = e.count.min(64) as usize;
        let mut buf = vec![0u8; n * 4];
        self.f.seek(SeekFrom::Start(self.u32(&e.raw) as u64))?;
        self.f.read_exact(&mut buf)?;
        Ok(buf.chunks_exact(4).map(|c| self.u32(c)).collect())
    }
}

fn tiff_candidates(f: &mut File, len: u64, le: bool) -> std::io::Result<Vec<(u64, u64)>> {
    let mut t = Tiff { f, le };
    let mut hdr = [0u8; 8];
    t.f.seek(SeekFrom::Start(0))?;
    t.f.read_exact(&mut hdr)?;
    let mut queue = vec![t.u32(&hdr[4..8]) as u64];
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    while let Some(off) = queue.pop() {
        if off < 8 || off >= len || !seen.insert(off) || seen.len() > 32 {
            continue;
        }
        let Ok((entries, next)) = t.read_ifd(off) else { continue };
        queue.push(next as u64);

        let find = |tag| entries.iter().find(|e| e.tag == tag).copied();
        if let (Some(o), Some(l)) = (find(0x0201), find(0x0202)) {
            out.push((t.value(&o) as u64, t.value(&l) as u64));
        }
        // Panasonic RW2 keeps its preview as a byte blob in tag 0x2E.
        if let Some(e) = find(0x002E) {
            if e.count > 4 {
                out.push((t.u32(&e.raw) as u64, e.count as u64));
            }
        }
        // Old-style JPEG stored as a single strip (CR2's IFD0, DNG previews).
        // Lossless sensor data is stored the same way; the baseline check
        // later throws that out.
        let compression = find(0x0103).map(|e| t.value(&e));
        if matches!(compression, Some(6 | 7)) {
            if let (Some(o), Some(l)) = (find(0x0111), find(0x0117)) {
                if o.count == 1 && l.count == 1 {
                    out.push((t.value(&o) as u64, t.value(&l) as u64));
                }
            }
        }
        if let Some(e) = find(0x014A) {
            for sub in t.u32_array(&e).unwrap_or_default() {
                queue.push(sub as u64);
            }
        }
    }
    Ok(out)
}

/// Checks the JPEG at `off` is one we can decode: baseline or progressive,
/// not the lossless JPEG some RAW formats use for sensor data.
fn is_baseline_jpeg_at(f: &mut File, off: u64, size: u64) -> std::io::Result<bool> {
    let end = off + size;
    let mut pos = off;
    let mut b = [0u8; 4];
    f.seek(SeekFrom::Start(pos))?;
    f.read_exact(&mut b[..2])?;
    if b[..2] != [0xFF, 0xD8] {
        return Ok(false);
    }
    pos += 2;
    for _ in 0..64 {
        if pos + 4 > end {
            return Ok(false);
        }
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut b)?;
        if b[0] != 0xFF {
            return Ok(false);
        }
        match b[1] {
            0xC0..=0xC2 => return Ok(true),
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Ok(false),
            0xD9 | 0xDA => return Ok(false),
            0xFF => pos += 1,
            _ => pos += 2 + u16::from_be_bytes([b[2], b[3]]) as u64,
        }
    }
    Ok(false)
}

fn cr3_preview(f: &mut File, len: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut pos = 0u64;
    for _ in 0..64 {
        if pos + 8 > len {
            break;
        }
        let mut h = [0u8; 8];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut h)?;
        let mut size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        let mut header = 8;
        if size == 1 {
            let mut l = [0u8; 8];
            f.read_exact(&mut l)?;
            size = u64::from_be_bytes(l);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header || pos + size > len {
            break;
        }
        if &h[4..8] == b"uuid" && size <= MAX_PREVIEW {
            let mut body = vec![0u8; (size - header) as usize];
            f.read_exact(&mut body)?;
            if body.len() > 16 && body[..16] == CR3_PREVIEW_UUID {
                let Some(prvw) = find(&body, b"PRVW") else { return Ok(None) };
                let Some(soi) = find(&body[prvw..], &[0xFF, 0xD8, 0xFF]) else { return Ok(None) };
                return Ok(Some(body[prvw + soi..].to_vec()));
            }
        }
        pos += size;
    }
    Ok(None)
}

/// The TIFF blocks a CR3 keeps its EXIF in: CMT1 holds IFD0 (camera,
/// orientation) and CMT2 the EXIF IFD (dates). Both sit inside `moov`.
pub fn cr3_exif_blocks(f: &mut File, len: u64) -> std::io::Result<Vec<Vec<u8>>> {
    let mut pos = 0u64;
    for _ in 0..64 {
        if pos + 8 > len {
            break;
        }
        let mut h = [0u8; 8];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut h)?;
        let size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        if size < 8 || pos + size > len {
            break;
        }
        if &h[4..8] == b"moov" {
            if size > MAX_PREVIEW {
                break;
            }
            let mut moov = vec![0u8; (size - 8) as usize];
            f.read_exact(&mut moov)?;
            let mut out = Vec::new();
            for tag in [b"CMT1", b"CMT2"] {
                if let Some(i) = find(&moov, tag).filter(|&i| i >= 4) {
                    let n = u32::from_be_bytes(moov[i - 4..i].try_into().unwrap()) as usize;
                    if n > 8 && i - 4 + n <= moov.len() {
                        out.push(moov[i + 4..i - 4 + n].to_vec());
                    }
                }
            }
            return Ok(out);
        }
        pos += size;
    }
    Ok(Vec::new())
}

/// Last resort for formats whose preview we don't locate directly (Olympus
/// keeps it in the maker notes): load the file and take the largest
/// decodable JPEG in it.
fn scan_fallback(f: &mut File, len: u64) -> std::io::Result<Option<Vec<u8>>> {
    if len > MAX_SCAN {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(len as usize);
    f.seek(SeekFrom::Start(0))?;
    f.read_to_end(&mut data)?;
    Ok(largest_jpeg_in(&data).map(|r| data[r].to_vec()))
}

fn largest_jpeg_in(data: &[u8]) -> Option<std::ops::Range<usize>> {
    let starts: Vec<usize> =
        data.windows(3).enumerate().filter(|(_, w)| *w == [0xFF, 0xD8, 0xFF]).map(|(i, _)| i).collect();
    let mut best: Option<std::ops::Range<usize>> = None;
    for (i, &s) in starts.iter().enumerate() {
        // Nested thumbnails inside EXIF also start with SOI, so a region
        // runs to the next SOI; decoders stop at EOI and ignore the rest.
        let e = starts.get(i + 1).copied().unwrap_or(data.len());
        if best.as_ref().is_some_and(|b| b.len() >= e - s) {
            continue;
        }
        if baseline_in_slice(&data[s..e]) {
            best = Some(s..e);
        }
    }
    best
}

fn baseline_in_slice(d: &[u8]) -> bool {
    let mut pos = 2;
    for _ in 0..64 {
        if pos + 4 > d.len() || d[pos] != 0xFF {
            return false;
        }
        match d[pos + 1] {
            0xC0..=0xC2 => return true,
            0xC3..=0xCF | 0xD9 | 0xDA => return false,
            0xFF => pos += 1,
            _ => pos += 2 + u16::from_be_bytes([d[pos + 2], d[pos + 3]]) as usize,
        }
    }
    false
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A small baseline JPEG of the given size.
    pub fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, 90]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
        out
    }

    /// A lossless-JPEG header, like the sensor data in CR2 and DNG files.
    fn lossless_stub(len: usize) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xC3, 0x00, 0x0B];
        v.resize(len, 0);
        v
    }

    fn file_with(data: &[u8]) -> (File, u64) {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(data).unwrap();
        (f, data.len() as u64)
    }

    /// Little-endian TIFF with IFD0 pointing at a small thumbnail through
    /// JPEGInterchangeFormat, and a SubIFD holding a single-strip JPEG.
    pub fn fake_tiff_raw(thumb: &[u8], preview: &[u8], raw_data: &[u8]) -> Vec<u8> {
        let mut out = b"II*\0".to_vec();
        out.extend_from_slice(&8u32.to_le_bytes());
        let ifd0 = 8u32;
        let ifd0_len = 2 + 3 * 12 + 4;
        let sub = ifd0 + ifd0_len;
        let sub_len = 2 + 3 * 12 + 4;
        let sub2 = sub + sub_len;
        let data = sub2 + sub_len;
        let thumb_off = data;
        let prev_off = thumb_off + thumb.len() as u32;
        let raw_off = prev_off + preview.len() as u32;

        let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, val: u32| {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&typ.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&val.to_le_bytes());
        };
        out.extend_from_slice(&3u16.to_le_bytes());
        entry(&mut out, 0x014A, 4, 1, sub);
        entry(&mut out, 0x0201, 4, 1, thumb_off);
        entry(&mut out, 0x0202, 4, 1, thumb.len() as u32);
        out.extend_from_slice(&sub2.to_le_bytes()); // next IFD
                                                    // SubIFD: the preview, as a strip.
        out.extend_from_slice(&3u16.to_le_bytes());
        entry(&mut out, 0x0103, 3, 1, 7);
        entry(&mut out, 0x0111, 4, 1, prev_off);
        entry(&mut out, 0x0117, 4, 1, preview.len() as u32);
        out.extend_from_slice(&0u32.to_le_bytes());
        // Next IFD: lossless sensor data, larger than the preview.
        out.extend_from_slice(&3u16.to_le_bytes());
        entry(&mut out, 0x0103, 3, 1, 7);
        entry(&mut out, 0x0111, 4, 1, raw_off);
        entry(&mut out, 0x0117, 4, 1, raw_data.len() as u32);
        out.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(out.len() as u32, data);
        out.extend_from_slice(thumb);
        out.extend_from_slice(preview);
        out.extend_from_slice(raw_data);
        out
    }

    #[test]
    fn tiff_raw_picks_largest_decodable_preview() {
        let thumb = jpeg(16, 12);
        let preview = jpeg(320, 240);
        let raw = lossless_stub(preview.len() * 4);
        let (mut f, len) = file_with(&fake_tiff_raw(&thumb, &preview, &raw));
        assert_eq!(extract_preview(&mut f, len).unwrap(), Some(preview));
    }

    #[test]
    fn raf_preview() {
        let preview = jpeg(64, 48);
        let mut data = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        data.resize(84, 0);
        data.extend_from_slice(&100u32.to_be_bytes());
        data.extend_from_slice(&(preview.len() as u32).to_be_bytes());
        data.resize(100, 0);
        data.extend_from_slice(&preview);
        data.extend_from_slice(&[0; 50]);
        let (mut f, len) = file_with(&data);
        assert_eq!(extract_preview(&mut f, len).unwrap(), Some(preview));
    }

    #[test]
    fn cr3_preview_box() {
        let preview = jpeg(64, 48);
        let mut data = Vec::new();
        data.extend_from_slice(&24u32.to_be_bytes());
        data.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
        let mut body = CR3_PREVIEW_UUID.to_vec();
        body.extend_from_slice(&[0; 8]);
        body.extend_from_slice(b"\0\0\0\x20PRVW\0\0\0\0\0\0\0\0");
        body.extend_from_slice(&preview);
        data.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        data.extend_from_slice(b"uuid");
        data.extend_from_slice(&body);
        let (mut f, len) = file_with(&data);
        assert_eq!(extract_preview(&mut f, len).unwrap(), Some(preview));
    }

    #[test]
    fn fallback_scan_finds_jpeg_in_unknown_layout() {
        let preview = jpeg(64, 48);
        let mut data = b"IIRO\x08\0\0\0".to_vec(); // ORF header, empty IFD
        data.extend_from_slice(&[0; 6]);
        data.extend_from_slice(&lossless_stub(40));
        data.extend_from_slice(&preview);
        let (mut f, len) = file_with(&data);
        let got = extract_preview(&mut f, len).unwrap().unwrap();
        assert!(got.starts_with(&preview[..preview.len() - 2]));
    }
}
