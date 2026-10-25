# Lazee Viewer

A fast, lightweight photo viewer for Windows that finds the faces in your
library, groups them by person, and lets you find every photo of someone in
one search. Everything runs on your own PC: no cloud, no accounts, no
telemetry.

Built for big libraries on modest hardware: 200,000+ JPEG and RAW photos
spread over internal disks, USB drives and a NAS.

> **Status:** early development. Milestone 1 (library indexing and the
> thumbnail grid) is in place; face recognition comes next. See the
> [roadmap](#roadmap).

## What works today

- Add folders on internal drives, USB drives and network shares (UNC paths
  or mapped drives). Files are indexed in place, never moved or copied.
- Drives are recognised by volume serial number (or share name), so a USB
  disk that comes back as `E:` instead of `F:` is still the same disk.
  Photos on a disconnected drive stay in the library, greyed out.
- JPEG plus RAW (CR2, CR3, NEF, ARW, DNG, ORF, RAF, RW2), shown from the
  camera's embedded preview. A RAW and a JPEG with the same name show once.
- EXIF: capture date, camera, orientation, size and GPS.
- Moved and renamed files are recognised by a content hash, so anything
  attached to a photo follows it.
- Thumbnails are made once and kept in a local cache, so scrolling never
  touches USB drives or the NAS. The grid only draws what is on screen.
- Scans run in the background on all cores but one, and can be paused or
  stopped. Re-scans only open new or changed files.

## Building

You need Rust (stable) and, on Windows, the WebView2 runtime (preinstalled
on Windows 10 and 11).

```sh
cargo run -p lazee-viewer            # run the app
cargo test --workspace               # run the tests
cargo tauri build                    # installer (needs `cargo install tauri-cli`)
```

The UI is plain HTML, CSS and JavaScript in `ui/` with no build step.

To time indexing without the UI:

```sh
cargo run --release -p lazee-core --example index -- "D:\Photos"
```

## Layout

| Path | What it is |
| --- | --- |
| `crates/core` | Library index, volumes, EXIF, RAW previews, thumbnails, scanner |
| `app` | Tauri 2 desktop app: commands and the thumbnail protocol |
| `ui` | The interface |

The library lives in `%LOCALAPPDATA%\io.github.bck539.lazeeviewer`:
`library.sqlite` holds the index and `thumbs.sqlite` the thumbnails.

## Roadmap

1. **Library and grid** (done): indexing, volumes, thumbnails.
2. **Viewer:** full-screen view with zoom and pan, folder watching, NAS
   rescans.
3. **Face model spike:** compare redistributable models (YuNet + SFace and
   others) on real photos.
4. **Faces:** background detection, embeddings, unnamed clusters, face boxes.
5. **People:** import names from Windows Photo Gallery and IrfanView; name,
   merge and split people; auto-tagging with a review queue.
6. **Search:** people with AND/OR, date and folder filters, timeline,
   optional XMP write-back (off by default).

## Privacy

Face data never leaves your PC, face scanning will be opt-in, and your
photo files are never modified unless you turn on metadata write-back.

## Licence

GPL-3.0. See [LICENSE](LICENSE).
