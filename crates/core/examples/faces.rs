//! Runs the face models on some photos and prints what they find, with the
//! similarity between every pair of faces. Useful for trying out a model
//! pack.
//!
//!     cargo run --release -p lazee-core --example faces -- <models folder> <photo>...
//!
//! ONNX Runtime is loaded from ORT_DYLIB_PATH.

use lazee_core::faces::{self, embed::similarity, pack};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let models = PathBuf::from(args.next().expect("usage: faces <models folder> <photo>..."));
    let photos: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let lib = std::env::var_os("ORT_DYLIB_PATH").expect("set ORT_DYLIB_PATH to the ONNX Runtime library");
    let env = faces::runtime(lib.as_ref()).expect("load ONNX Runtime");
    let packs = pack::discover(&[models]);
    let pick = |kind| packs.iter().find(|p| p.manifest.kind == kind).cloned().expect("model pack");
    let mut engine = faces::Engine::new(env, pick(pack::Kind::Detector), pick(pack::Kind::Embedder), 1).unwrap();
    println!("models: {}", engine.key());

    let mut all = Vec::new();
    for path in &photos {
        let img = image::open(path).expect("open photo").to_rgb8();
        let t = Instant::now();
        let found = engine.analyze(&img, 20.0).expect("analyze");
        println!("{}: {} faces in {:.0} ms", path.display(), found.len(), t.elapsed().as_secs_f64() * 1000.0);
        for (i, f) in found.iter().enumerate().take(5) {
            let b = &f.face;
            println!("  #{i} at ({:.0}, {:.0}) {:.0}x{:.0} score {:.2}", b.x, b.y, b.w, b.h, b.score);
            all.push((format!("{}#{i}", path.file_name().unwrap().to_string_lossy()), f.embedding.clone()));
        }
    }
    println!("similarity (same person above {}):", engine.embedder.match_threshold());
    for (i, (a, ea)) in all.iter().enumerate() {
        for (b, eb) in &all[i + 1..] {
            println!("  {a} ~ {b}: {:.3}", similarity(ea, eb));
        }
    }
}
