//! Tags from other programs, naming people and suggestions.

use lazee_core::faces::{self, pack};
use lazee_core::{facescan, tagscan, Filter, Library, ScanControl, SortOrder};
use std::path::Path;

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 99]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
    out
}

/// Adds Windows Photo Gallery style XMP to a JPEG.
fn with_people(jpeg: &[u8], people: &[(&str, Option<&str>)], keywords: &[&str]) -> Vec<u8> {
    let regions: String = people
        .iter()
        .map(|(name, rect)| match rect {
            Some(r) => format!(r#"<rdf:li MPReg:PersonDisplayName="{name}" MPReg:Rectangle="{r}"/>"#),
            None => format!(r#"<rdf:li MPReg:PersonDisplayName="{name}"/>"#),
        })
        .collect();
    let subject: String = keywords.iter().map(|k| format!("<rdf:li>{k}</rdf:li>")).collect();
    let xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
        <rdf:Description xmlns:MP="http://ns.microsoft.com/photo/1.2/"
          xmlns:MPRI="http://ns.microsoft.com/photo/1.2/t/RegionInfo#"
          xmlns:MPReg="http://ns.microsoft.com/photo/1.2/t/Region#" xmlns:dc="http://purl.org/dc/elements/1.1/">
        <MP:RegionInfo rdf:parseType="Resource"><MPRI:Regions><rdf:Bag>{regions}</rdf:Bag></MPRI:Regions></MP:RegionInfo>
        <dc:subject><rdf:Bag>{subject}</rdf:Bag></dc:subject>
        </rdf:Description></rdf:RDF></x:xmpmeta>"#
    );
    let mut seg = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    seg.extend(xmp.as_bytes());
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend(((seg.len() + 2) as u16).to_be_bytes());
    out.extend(seg);
    out.extend(&jpeg[2..]);
    out
}

fn scan_all(lib: &Library) {
    let ctl = ScanControl::default();
    lib.scan(&ctl, &|_| {}).unwrap();
    tagscan::scan(lib, &ctl, &|_| {}).unwrap();
}

#[test]
fn imports_people_and_keywords_from_tags() {
    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = photos.path();
    std::fs::write(p.join("a.jpg"), with_people(&jpeg(64, 48), &[("Anna", Some("0.1, 0.1, 0.3, 0.3"))], &["Urlaub"]))
        .unwrap();
    std::fs::write(p.join("b.jpg"), with_people(&jpeg(64, 48), &[("anna", None), ("Ben", None)], &[])).unwrap();
    std::fs::write(p.join("c.jpg"), jpeg(64, 48)).unwrap();
    let lib = Library::open(data.path()).unwrap();
    lib.add_source(p).unwrap();
    scan_all(&lib);

    let people = lib.people().unwrap();
    let names: Vec<_> = people.iter().map(|p| (p.name.as_str(), p.photos)).collect();
    assert_eq!(names, vec![("Anna", 2), ("Ben", 1)]);
    let anna = people[0].id;
    let ids = lib.photos_of(SortOrder::Name, &Filter { person: Some(anna), ..Default::default() }).unwrap().ids;
    assert_eq!(ids.len(), 2);
    assert_eq!(lib.photo_tag_people(ids[1]).unwrap().len(), 2);

    // Tags are read once per version of a file.
    let s = tagscan::scan(&lib, &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!(s.photos, 0);
    std::fs::write(p.join("b.jpg"), jpeg(64, 40)).unwrap();
    scan_all(&lib);
    let people = lib.people().unwrap();
    assert_eq!(people.iter().map(|p| p.photos).collect::<Vec<_>>(), vec![1]);

    // Renaming to an existing name merges; deleting forgets.
    lib.rename_person(anna, "Anna Maria").unwrap();
    assert_eq!(lib.people().unwrap()[0].name, "Anna Maria");
    lib.delete_person(anna).unwrap();
    assert!(lib.people().unwrap().is_empty());
}

#[test]
fn names_spread_to_look_alikes() {
    let (Some(ort), Some(samples)) = (std::env::var_os("ORT_DYLIB_PATH"), std::env::var_os("LAZEE_FACE_PHOTOS")) else {
        eprintln!("skipped: set ORT_DYLIB_PATH and LAZEE_FACE_PHOTOS to run");
        return;
    };
    let samples = Path::new(&samples);
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
    let packs = pack::discover(&[models]);
    let pick = |id: &str| packs.iter().find(|p| p.id() == id && p.model_path().exists()).cloned();
    let (Some(det), Some(emb)) = (pick("yunet-2023mar"), pick("sface-2021dec")) else {
        eprintln!("skipped: run scripts/fetch-models.sh first");
        return;
    };
    let env = faces::runtime(Path::new(&ort)).unwrap();
    let mut engine = faces::Engine::new(env, det, emb, 2).unwrap();

    let photos = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = photos.path();
    // Lena tagged with her face area, as Windows Photo Gallery saves it.
    let lena = std::fs::read(samples.join("lena.jpg")).unwrap();
    std::fs::write(p.join("a.jpg"), with_people(&lena, &[("Lena", Some("0.40, 0.35, 0.28, 0.40"))], &[])).unwrap();
    let img = image::open(samples.join("lena.jpg")).unwrap();
    img.fliph().to_rgb8().save(p.join("b.jpg")).unwrap();
    img.resize(400, 400, image::imageops::FilterType::Triangle).to_rgb8().save(p.join("c.jpg")).unwrap();
    let messi = image::open(samples.join("messi5.jpg")).unwrap();
    for (name, m) in [("d.jpg", messi.clone()), ("e.jpg", messi.fliph())] {
        m.resize(1100, 1100, image::imageops::FilterType::Triangle).to_rgb8().save(p.join(name)).unwrap();
    }

    let lib = Library::open(data.path()).unwrap();
    lib.add_source(p).unwrap();
    scan_all(&lib);
    facescan::scan(&lib, &mut engine, &ScanControl::default(), &|_| {}).unwrap();
    let ids = lib.photo_list(SortOrder::Name, None).unwrap().ids;

    // The tag named Lena's face; her look-alikes are suggested.
    let a = lib.photo_faces(ids[0]).unwrap();
    assert_eq!(a[0].person.as_deref(), Some("Lena"));
    assert!(!a[0].suggested);
    let lena_id = a[0].person_id.unwrap();
    let suggestions = lib.suggestions(Some(lena_id)).unwrap();
    assert_eq!(suggestions.iter().map(|s| s.faces.len()).sum::<usize>(), 2);
    assert!(lib.photo_faces(ids[1]).unwrap()[0].suggested);

    // Messi is not suggested as anyone, and shows as an unnamed group.
    assert!(lib.photo_faces(ids[3]).unwrap().iter().all(|f| f.person_id.is_none()));
    let groups = lib.groups(2).unwrap();
    assert_eq!(groups.len(), 1);
    lib.name_group(groups[0].id, "Lionel").unwrap();
    assert_eq!(lib.photo_faces(ids[4]).unwrap()[0].person.as_deref(), Some("Lionel"));

    // Accept one suggestion, turn down the other.
    let s = &lib.suggestions(Some(lena_id)).unwrap()[0];
    let (first, rest) = s.faces.split_first().unwrap();
    lib.accept_suggestion(lena_id, &[*first]).unwrap();
    lib.reject_suggestion(lena_id, rest).unwrap();
    assert!(lib.suggestions(None).unwrap().is_empty());
    let lena_photos = lib.people().unwrap().into_iter().find(|p| p.id == lena_id).unwrap().photos;
    assert_eq!(lena_photos, 2);

    // "Not Lionel" takes one face out of his group.
    let face = lib.photo_faces(ids[4]).unwrap()[0].id;
    lib.unname_face(face).unwrap();
    assert!(lib.photo_faces(ids[4]).unwrap()[0].person_id.is_none());
    assert_eq!(lib.suggestions(None).unwrap().len(), 0, "a turned-down face is not suggested again");

    // Names survive a photo being looked at again.
    std::fs::write(
        p.join("d.jpg"),
        std::fs::read(p.join("d.jpg")).unwrap().into_iter().chain([0u8]).collect::<Vec<_>>(),
    )
    .unwrap();
    scan_all(&lib);
    facescan::scan(&lib, &mut engine, &ScanControl::default(), &|_| {}).unwrap();
    assert_eq!(lib.photo_faces(ids[3]).unwrap()[0].person.as_deref(), Some("Lionel"));
}
