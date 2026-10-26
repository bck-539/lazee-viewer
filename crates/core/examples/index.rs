//! Indexes a folder from the command line and reports how long it took.
//!
//!     cargo run --release -p lazee-core --example index -- <photo folder> [data folder]
//!
//! With `ORT_DYLIB_PATH` set, it then looks for faces using the packs in
//! `models/` and reports that speed too.

use lazee_core::{Library, ScanControl, SortOrder};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(photos) = args.next() else {
        eprintln!("usage: index <photo folder> [data folder]");
        std::process::exit(2);
    };
    let data = args.next().unwrap_or_else(|| "lazee-data".into());
    let lib = Library::open(data.as_ref()).expect("open library");
    if let Err(e) = lib.add_source(photos.as_ref()) {
        eprintln!("{e}");
    }
    let start = Instant::now();
    let summary = lib
        .scan(&ScanControl::default(), &|p| {
            eprint!("\r{:?}: {} found, {}/{}      ", p.phase, p.found, p.processed, p.to_process)
        })
        .expect("scan");
    let secs = start.elapsed().as_secs_f64();
    let n = summary.added + summary.updated + summary.moved;
    eprintln!();
    println!("{summary:?}");
    println!("{secs:.1} s, {:.1} photos/s", n as f64 / secs.max(0.001));
    let list = lib.photo_list(SortOrder::NewestFirst, None).expect("list");
    let start = Instant::now();
    for id in list.ids.iter().take(1000) {
        lib.thumbnail(*id).expect("thumbnail");
    }
    println!(
        "{} photos in library; thumbnail lookup {:.2} ms each",
        list.ids.len(),
        start.elapsed().as_secs_f64() * 1000.0 / list.ids.len().clamp(1, 1000) as f64
    );

    if let Some(ort) = std::env::var_os("ORT_DYLIB_PATH") {
        use lazee_core::faces::{self, pack};
        let env = faces::runtime(ort.as_ref()).expect("load ONNX Runtime");
        let packs = pack::discover(&["models".into()]);
        let pick = |kind| packs.iter().find(|p| p.manifest.kind == kind).cloned().expect("model pack");
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 4);
        let mut engine = faces::Engine::new(env, pick(pack::Kind::Detector), pick(pack::Kind::Embedder), threads)
            .expect("load models");
        let start = Instant::now();
        let s = lazee_core::facescan::scan(&lib, &mut engine, &ScanControl::default(), &|p| {
            eprint!("\rfaces: {}/{}, {} found      ", p.done, p.total, p.faces)
        })
        .expect("face scan");
        let secs = start.elapsed().as_secs_f64();
        eprintln!();
        println!("{s:?}");
        println!("{secs:.1} s, {:.1} photos/s", s.photos as f64 / secs.max(0.001));
    }
}
