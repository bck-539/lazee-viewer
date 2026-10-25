//! End-to-end scans of a temporary photo folder.

use lazee_core::{Library, ScanControl, ScanSummary, SortOrder};
use std::path::Path;

fn jpeg(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x as u8).wrapping_mul(seed), y as u8, seed]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
    out
}

fn scan(lib: &Library) -> ScanSummary {
    lib.scan(&ScanControl::default(), &|_| {}).unwrap()
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

#[test]
fn indexes_moves_and_deletes() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = photos.path();
    write(&p.join("2015/a.jpg"), &jpeg(800, 600, 3));
    write(&p.join("2015/b.JPG"), &jpeg(600, 800, 5));
    write(&p.join("2016/c.jpeg"), &jpeg(640, 480, 7));
    write(&p.join("2016/notes.txt"), b"not a photo");
    write(&p.join("$RECYCLE.BIN/d.jpg"), &jpeg(64, 64, 9));

    let lib = Library::open(data.path()).unwrap();
    let source = lib.add_source(p).unwrap();
    assert!(source.online);
    assert!(lib.add_source(&p.join("2015")).is_err(), "nested sources are refused");

    let s = scan(&lib);
    assert_eq!((s.added, s.failed, s.removed), (3, 0, 0));
    let list = lib.photo_list(SortOrder::Name, None).unwrap();
    assert_eq!(list.ids.len(), 3);
    assert!(list.offline.is_empty());
    for id in &list.ids {
        let thumb = lib.thumbnail(*id).unwrap().expect("thumbnail");
        assert!(thumb.starts_with(&[0xFF, 0xD8]));
    }
    let first = lib.photo_info(list.ids[0]).unwrap().unwrap();
    assert_eq!(first.name, "a.jpg");
    assert_eq!((first.width, first.height), (Some(800), Some(600)));
    assert_eq!(lib.sources().unwrap()[0].photo_count, 3);

    // A second scan touches nothing.
    let s = scan(&lib);
    assert_eq!((s.added, s.updated, s.unchanged), (0, 0, 3));

    // Moving and renaming a file keeps its id.
    let id_a = list.ids[0];
    std::fs::create_dir_all(p.join("sorted")).unwrap();
    std::fs::rename(p.join("2015/a.jpg"), p.join("sorted/renamed.jpg")).unwrap();
    std::fs::remove_file(p.join("2016/c.jpeg")).unwrap();
    let s = scan(&lib);
    assert_eq!((s.moved, s.removed, s.added), (1, 1, 0));
    let info = lib.photo_info(id_a).unwrap().unwrap();
    assert_eq!(info.name, "renamed.jpg");
    assert!(info.folder.ends_with("sorted"));
    assert_eq!(lib.photo_list(SortOrder::NewestFirst, Some(source.id)).unwrap().ids.len(), 2);

    // Removing the source empties the library but leaves the files alone.
    lib.remove_source(source.id).unwrap();
    assert!(lib.photo_list(SortOrder::Name, None).unwrap().ids.is_empty());
    assert!(p.join("sorted/renamed.jpg").exists());
}

#[test]
fn broken_files_are_listed_and_not_retried() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write(&photos.path().join("broken.jpg"), b"\xFF\xD8\xFF\xE0 truncated");
    write(&photos.path().join("empty.nef"), b"");

    let lib = Library::open(data.path()).unwrap();
    lib.add_source(photos.path()).unwrap();
    let s = scan(&lib);
    assert_eq!((s.added, s.failed), (2, 2));
    let s = scan(&lib);
    assert_eq!((s.unchanged, s.failed), (2, 0));
    let ids = lib.photo_list(SortOrder::Name, None).unwrap().ids;
    assert!(lib.photo_info(ids[0]).unwrap().unwrap().error.is_some());
}

#[test]
fn raw_and_jpeg_pair_show_once() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    // A RAW with an unknown layout whose preview is found by the fallback.
    let mut raw = b"IIRO\x08\0\0\0\0\0\0\0\0\0".to_vec();
    raw.extend_from_slice(&jpeg(400, 300, 11));
    write(&photos.path().join("IMG_1.ORF"), &raw);
    write(&photos.path().join("IMG_1.jpg"), &jpeg(400, 300, 12));
    write(&photos.path().join("IMG_2.orf"), &raw);

    let lib = Library::open(data.path()).unwrap();
    lib.add_source(photos.path()).unwrap();
    let s = scan(&lib);
    assert_eq!((s.added, s.failed), (3, 0));
    let ids = lib.photo_list(SortOrder::Name, None).unwrap().ids;
    let names: Vec<_> = ids.iter().map(|id| lib.photo_info(*id).unwrap().unwrap().name).collect();
    assert_eq!(names, ["IMG_1.jpg", "IMG_2.orf"]);
    assert!(lib.thumbnail(ids[1]).unwrap().is_some());
}

#[test]
fn cancelled_scan_removes_nothing() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write(&photos.path().join("a.jpg"), &jpeg(100, 100, 1));
    let lib = Library::open(data.path()).unwrap();
    lib.add_source(photos.path()).unwrap();
    scan(&lib);
    std::fs::remove_file(photos.path().join("a.jpg")).unwrap();

    let ctl = ScanControl::default();
    ctl.cancel();
    let s = lib.scan(&ctl, &|_| {}).unwrap();
    assert!(s.cancelled);
    assert_eq!(lib.photo_list(SortOrder::Name, None).unwrap().ids.len(), 1);
}

#[test]
fn full_images_and_partial_scans() {
    use lazee_core::FullImage;
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let original = jpeg(300, 200, 4);
    write(&a.path().join("a.jpg"), &original);
    let mut raw = b"IIRO\x08\0\0\0\0\0\0\0\0\0".to_vec();
    raw.extend_from_slice(&jpeg(300, 200, 6));
    write(&b.path().join("b.orf"), &raw);

    let lib = Library::open(data.path()).unwrap();
    let sa = lib.add_source(a.path()).unwrap();
    lib.add_source(b.path()).unwrap();
    let s = lib.scan_sources(&[sa.id], &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!(s.added, 1, "only the requested source is scanned");
    scan(&lib);

    let ids = lib.photo_list(SortOrder::Name, None).unwrap().ids;
    match lib.full_image(ids[0]).unwrap() {
        FullImage::Jpeg(bytes) => assert_eq!(bytes, original, "JPEGs pass through untouched"),
        _ => panic!("expected the JPEG"),
    }
    match lib.full_image(ids[1]).unwrap() {
        FullImage::Jpeg(bytes) => assert!(image::load_from_memory(&bytes).is_ok()),
        _ => panic!("expected the RAW preview"),
    }
    assert_eq!(lib.watch_targets().unwrap().len(), 2);

    std::fs::remove_file(a.path().join("a.jpg")).unwrap();
    assert!(matches!(lib.full_image(ids[0]).unwrap(), FullImage::Missing));
    assert!(matches!(lib.full_image(-1).unwrap(), FullImage::Missing));
}
