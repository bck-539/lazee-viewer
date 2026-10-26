//! The face scan end to end, with real models on real photos.
//!
//! Needs ONNX Runtime (`ORT_DYLIB_PATH`), the model packs in `models/`
//! (`scripts/fetch-models.sh`) and a folder `LAZEE_FACE_PHOTOS` holding
//! OpenCV's sample photos `lena.jpg` and `messi5.jpg`. Skipped otherwise.

use lazee_core::faces::{self, pack};
use lazee_core::{facescan, Library, ScanControl, SortOrder};
use std::path::{Path, PathBuf};

fn setup() -> Option<(faces::Engine, PathBuf)> {
    let (Some(ort), Some(photos)) = (std::env::var_os("ORT_DYLIB_PATH"), std::env::var_os("LAZEE_FACE_PHOTOS")) else {
        eprintln!("skipped: set ORT_DYLIB_PATH and LAZEE_FACE_PHOTOS to run");
        return None;
    };
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
    let packs = pack::discover(&[models]);
    let pick = |id: &str| packs.iter().find(|p| p.id() == id && p.model_path().exists()).cloned();
    let (Some(det), Some(emb)) = (pick("yunet-2023mar"), pick("sface-2021dec")) else {
        eprintln!("skipped: run scripts/fetch-models.sh first");
        return None;
    };
    let env = faces::runtime(Path::new(&ort)).expect("load ONNX Runtime");
    Some((faces::Engine::new(env, det, emb, 2).unwrap(), PathBuf::from(photos)))
}

#[test]
fn finds_and_groups_faces() {
    let Some((mut engine, samples)) = setup() else { return };
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = photos.path();
    std::fs::copy(samples.join("lena.jpg"), p.join("a-lena.jpg")).unwrap();
    // Messi's face is too small to count at the sample's size.
    let messi = image::open(samples.join("messi5.jpg")).unwrap();
    messi.resize(1100, 1100, image::imageops::FilterType::Triangle).to_rgb8().save(p.join("c-messi.jpg")).unwrap();
    // The same person again, mirrored and smaller.
    let lena = image::open(samples.join("lena.jpg")).unwrap();
    lena.fliph().resize(400, 400, image::imageops::FilterType::Triangle).to_rgb8().save(p.join("b-lena.jpg")).unwrap();
    // No faces here.
    image::RgbImage::from_pixel(300, 200, image::Rgb([90, 140, 200])).save(p.join("d-sky.jpg")).unwrap();

    let lib = Library::open(data.path()).unwrap();
    lib.add_source(p).unwrap();
    lib.scan(&ScanControl::default(), &|_| {}).unwrap();
    let ids = lib.photo_list(SortOrder::Name, None).unwrap().ids;
    assert_eq!(ids.len(), 4);

    let s = facescan::scan(&lib, &mut engine, &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!((s.photos, s.failed), (4, 0));
    let lena_a = lib.photo_faces(ids[0]).unwrap();
    let lena_b = lib.photo_faces(ids[1]).unwrap();
    let messi = lib.photo_faces(ids[2]).unwrap();
    assert_eq!(lena_a.len(), 1);
    assert_eq!(lena_b.len(), 1);
    assert!(!messi.is_empty());
    assert!(lib.photo_faces(ids[3]).unwrap().is_empty());
    let b = &lena_a[0];
    assert!(b.x > 0.0 && b.y > 0.0 && b.x + b.w < 1.0 && b.y + b.h < 1.0);

    // Both Lenas in one group, Messi in another.
    assert_eq!(lena_a[0].group, lena_b[0].group);
    assert!(messi.iter().all(|f| f.group != lena_a[0].group));
    let lena_group = lena_a[0].group.unwrap();
    assert_eq!(
        lib.photos_of(SortOrder::Name, &lazee_core::Filter { group: Some(lena_group), ..Default::default() })
            .unwrap()
            .ids,
        vec![ids[0], ids[1]]
    );
    let groups = lib.groups(2).unwrap();
    assert_eq!(groups[0].id, lena_group);
    assert_eq!(groups[0].photos, 2);
    assert!(lib.face_chip(groups[0].cover).unwrap().is_some());

    // Nothing left to do the second time.
    let s = facescan::scan(&lib, &mut engine, &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!(s.photos, 0);
    let stats = lib.face_stats().unwrap();
    assert_eq!((stats.photos, stats.scanned), (4, 4));

    // A changed photo is looked at again; its old faces go.
    std::fs::copy(p.join("c-messi.jpg"), p.join("a-lena.jpg")).unwrap();
    lib.scan(&ScanControl::default(), &|_| {}).unwrap();
    let s = facescan::scan(&lib, &mut engine, &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!(s.photos, 1);
    assert_eq!(lib.photo_faces(ids[0]).unwrap().len(), messi.len());
    assert_ne!(lib.photo_faces(ids[0]).unwrap()[0].group, Some(lena_group));

    lib.forget_faces().unwrap();
    assert!(lib.groups(1).unwrap().is_empty());
    assert_eq!(lib.face_stats().unwrap().scanned, 0);
}
