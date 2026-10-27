//! Finding copies of photos across folders and deleting the extra ones.

use lazee_core::duplicates::{self, Role, ASIDE_FOLDER};
use lazee_core::{Library, ScanControl, SortOrder};
use std::path::Path;

fn jpeg(seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(320, 240, |x, y| image::Rgb([(x as u8).wrapping_mul(seed), y as u8, seed]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
    out
}

/// The same picture with an XMP block added, as a tagging program would.
fn tagged(jpeg: &[u8]) -> Vec<u8> {
    let body = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend(((body.len() + 2) as u16).to_be_bytes());
    out.extend(body);
    out.extend(&jpeg[2..]);
    out
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

#[test]
fn finds_copies_and_deletes_the_extra_ones() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = photos.path();
    let a = jpeg(3);
    write(&p.join("Main/a.jpg"), &a);
    write(&p.join("Main/sub/a copy.jpg"), &a);
    write(&p.join("Phone/IMG_0001.jpg"), &tagged(&a));
    write(&p.join("Backup/2015/a.jpg"), &a);
    write(&p.join("Main/unique.jpg"), &jpeg(5));
    write(&p.join("Main/c.jpg"), &jpeg(7));
    write(&p.join("Backup/c.jpg"), &jpeg(7));

    let lib = Library::open(data.path()).unwrap();
    lib.add_source(p).unwrap();
    let ctl = ScanControl::default();
    lib.scan(&ctl, &|_| {}).unwrap();
    lib.set_dup_folder(&p.join("Main"), Role::Leading).unwrap();
    lib.set_dup_folder(&p.join("Backup"), Role::Backup).unwrap();
    assert_eq!(lib.dup_folders().unwrap().len(), 2);

    let s = duplicates::find(&lib, &ctl, &|_| {}).unwrap();
    assert_eq!((s.groups, s.copies, s.failed), (2, 4, 0));

    let groups = lib.duplicates().unwrap();
    assert_eq!(groups.len(), 2);
    let g = groups.iter().find(|g| g.copies.len() == 4).unwrap();
    let names: Vec<_> = g.copies.iter().map(|c| (c.name.as_str(), c.role, c.suggested)).collect();
    assert_eq!(
        names,
        [
            ("a.jpg", Role::Leading, false),
            ("a copy.jpg", Role::Leading, true),
            ("IMG_0001.jpg", Role::Other, true),
            ("a.jpg", Role::Backup, false),
        ]
    );
    assert!(!g.expected);
    let c = groups.iter().find(|g| g.copies.len() == 2).unwrap();
    assert!(c.expected, "a copy only in a backup folder is expected");
    assert!(c.copies[0].folder.ends_with("Main"));

    // Every copy of a photo can't go.
    let all: Vec<i64> = c.copies.iter().map(|c| c.id).collect();
    let r = lib.delete_duplicates(&all).unwrap();
    assert_eq!((r.recycled + r.moved_aside, r.failed.len()), (0, 2));

    // The suggested copies go: to the Recycle Bin on a Windows fixed drive,
    // otherwise moved aside, where scans don't see them.
    let picked: Vec<i64> = g.copies.iter().filter(|c| c.suggested).map(|c| c.id).collect();
    let r = lib.delete_duplicates(&picked).unwrap();
    assert_eq!((r.recycled + r.moved_aside, r.failed.len()), (2, 0), "{:?}", r.failed);
    assert!(!p.join("Phone/IMG_0001.jpg").exists());
    assert!(!p.join("Main/sub/a copy.jpg").exists());
    if cfg!(windows) {
        assert_eq!(r.recycled, 2);
    } else {
        assert!(p.join(ASIDE_FOLDER).join("Phone/IMG_0001.jpg").exists());
        assert!(p.join(ASIDE_FOLDER).join("Main/sub/a copy.jpg").exists());
    }
    assert!(p.join("Main/a.jpg").exists());
    assert_eq!(lib.photo_list(SortOrder::Name, None).unwrap().ids.len(), 5);

    let s = lib.scan(&ctl, &|_| {}).unwrap();
    assert_eq!((s.added, s.removed), (0, 0));
    duplicates::find(&lib, &ctl, &|_| {}).unwrap();
    let groups = lib.duplicates().unwrap();
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.expected));

    // A file that changed since the scan is left alone.
    let g = groups.iter().find(|g| g.copies[0].name == "a.jpg").unwrap();
    write(&p.join("Backup/2015/a.jpg"), &jpeg(9));
    let r = lib.delete_duplicates(&[g.copies[1].id]).unwrap();
    assert_eq!(r.failed.len(), 1);
}
