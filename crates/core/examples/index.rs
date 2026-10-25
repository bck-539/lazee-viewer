//! Indexes a folder from the command line and reports how long it took.
//!
//!     cargo run --release -p lazee-core --example index -- <photo folder> [data folder]

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
}
