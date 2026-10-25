//! Content hashes that identify a photo wherever it lives.
//!
//! Hashing whole files would mean reading every RAW over the network twice,
//! so the hash covers the file size plus its first and last 64 KiB. For
//! camera files that is enough: the head holds the EXIF block with the
//! capture time and the tail holds image data, so two different photos of the
//! same size almost never share both.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub const SAMPLE: usize = 64 * 1024;
pub type ContentHash = [u8; 16];

/// Hash from a file that is already fully in memory.
pub fn from_bytes(data: &[u8]) -> ContentHash {
    let head = &data[..data.len().min(SAMPLE)];
    let tail = &data[data.len().saturating_sub(SAMPLE)..];
    finish(data.len() as u64, head, tail)
}

/// Hash by reading only the head and tail of a file.
pub fn from_file(file: &mut File, len: u64) -> std::io::Result<ContentHash> {
    let sample = SAMPLE as u64;
    let mut head = vec![0; len.min(sample) as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)?;
    let tail_start = len.saturating_sub(sample);
    let mut tail = vec![0; (len - tail_start) as usize];
    file.seek(SeekFrom::Start(tail_start))?;
    file.read_exact(&mut tail)?;
    Ok(finish(len, &head, &tail))
}

fn finish(len: u64, head: &[u8], tail: &[u8]) -> ContentHash {
    let mut h = blake3::Hasher::new();
    h.update(&len.to_le_bytes());
    h.update(head);
    h.update(tail);
    let mut out = [0; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn file_and_bytes_agree() {
        for len in [0usize, 10, SAMPLE, SAMPLE + 1, 3 * SAMPLE + 7] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let mut f = tempfile::tempfile().unwrap();
            f.write_all(&data).unwrap();
            assert_eq!(from_file(&mut f, len as u64).unwrap(), from_bytes(&data), "len {len}");
        }
    }

    #[test]
    fn size_matters() {
        assert_ne!(from_bytes(&[0; 10]), from_bytes(&[0; 11]));
    }
}
