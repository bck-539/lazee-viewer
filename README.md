# Lazee Viewer

A fast, lightweight photo viewer for Windows that finds the faces in your
library, groups them by person, and lets you find every photo of someone in
one search. Everything runs on your own PC: no cloud, no accounts, no
telemetry.

Built for big libraries on modest hardware: 200,000+ JPEG and RAW photos
spread over internal disks, USB drives and a NAS.

> **Status:** first complete version. Every milestone on the
> [roadmap](#roadmap) is in place; expect rough edges.

## Download

Get the latest `lazee-viewer-…-windows-x64.zip` from
[Releases](https://github.com/bck-539/lazee-viewer/releases), unzip it
anywhere and run `lazee-viewer.exe`. It needs the Microsoft Edge WebView2
runtime, which Windows 10 and 11 already have; everything else, including
the Visual C++ runtime that face finding uses, is in the zip. The exe isn't code-signed
yet, so Windows SmartScreen may ask you to confirm the first start.

## What works today

- Add folders on internal drives, USB drives and network shares (UNC paths
  or mapped drives), or add a whole drive or share at once. Files are indexed
  in place, never moved or copied. On a whole drive, Windows, Program Files,
  ProgramData, AppData, recycle bins and other system folders are skipped,
  and folders Windows won't let the app read are left out without stopping
  the scan. Adding a drive that already has folders in the library folds them
  into it, keeping their faces and names.
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
- Local and USB folders are watched, so new, moved and deleted photos show
  up by themselves. Network shares are rescanned every hour, and a drive
  is rescanned as soon as it is plugged back in.
- Full-screen viewer: double-click a photo or press Enter. Arrow keys,
  Page Up/Down or a swipe move between photos; the mouse wheel zooms
  around the pointer; drag to pan; `0` fits to the screen, `1` shows 100%,
  `F` goes full screen, `Esc` goes back. The thumbnail shows instantly
  while the full photo loads, and the next photos are preloaded.
- A photo on a disconnected drive still opens from its thumbnail, with a
  note saying which drive to connect.
- Faces (opt-in, under *Settings*): after new photos are read, a
  background scan finds the faces in them and groups look-alikes into
  unnamed people. *People* shows one face per group; click it to see every
  photo of that person. `B` in the viewer shows the face boxes. The face
  scan pauses and stops with the file scan, and turning faces off deletes
  all face data.
- Tags you already have are read on every scan, without changing the
  files: people tagged in Windows Photo Gallery (with their face areas),
  Metadata Working Group face regions (Lightroom, digiKam and others),
  IrfanView's IPTC keywords and captions, JPEG comments, Windows keywords,
  and `.xmp` sidecars next to RAW files. Tagged names become people, and a
  name with a face area goes on the face found there.
- Naming people: type a name on an unnamed group, or click a face in the
  viewer to name or correct it. Faces that look like someone you named are
  suggested; *Review suggestions* asks "Is this them?" with Yes and No.
  "No" is remembered, so a face is never suggested as that person again.
  Renaming someone to another person's name merges the two. Names you
  give survive a photo being rescanned, or a switch of face models.
- One person in several groups: *People* asks "Are these the same
  person?" about groups that look alike, or "Is this Anna?" about a group
  that looks like someone named. *Merge people…* puts any groups and
  people you pick together. *Pick faces…* on a person or group lets you
  pick single faces (Shift+click for a range) and move them to someone
  else, or take them out.
- Search: the search box matches people, folders, file names, keywords
  and captions; every word must match. *Filters* picks people (all of
  them in one photo, or any of them) and a date range.
- Timeline: a strip along the grid shows years and months; click or drag
  on it to jump through the library.
- Writing names into the photos (off by default, under *Settings*): when
  turned on, the names you give are written into each photo as standard
  XMP face regions and keywords, which Lightroom, digiKam and Windows
  understand. JPEGs are changed in place without touching the image data;
  RAW files get an `.xmp` sidecar. Only photos whose names differ from
  the library are written.

## Face models are plug-ins

Face finding uses two models: a *detector* that finds faces and five
landmarks (eyes, nose, mouth corners), and a *recogniser* (embedder) that
turns each face into a vector so faces of the same person can be matched.
Both run on [ONNX Runtime](https://onnxruntime.ai), on the CPU.

Each model is a folder with the `.onnx` file, its licence and a
`model.toml` describing it. The app looks for model folders in `models`
next to `lazee-viewer.exe` and in `%LOCALAPPDATA%\io.github.bck539.lazeeviewer\models`.
To try another model, drop its folder in either place and pick it in
*Settings*; no rebuild is needed. Switching models makes the face
scan look at every photo again, since different models' results can't be
compared.

The app ships with:

| Model | Kind | Licence |
| --- | --- | --- |
| [YuNet 2023mar](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) | detector | MIT |
| [SFace 2021dec](https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface) | recogniser | Apache-2.0 |

`models/*/model.toml` are the examples to copy, and
`crates/core/src/faces/pack.rs` documents every field. A recogniser in the
same family (112×112 ArcFace-aligned input, one embedding output) works by
writing a `model.toml`; a detector with a different output layout needs a
decoder added in `crates/core/src/faces/detect.rs`.

The `.onnx` files are not kept in git: `scripts/fetch-models.sh`
downloads them and checks their SHA-256.

## Building

You need Rust (stable) and, on Windows, the WebView2 runtime (preinstalled
on Windows 10 and 11).

```sh
scripts/fetch-models.sh              # download the face models
cargo run -p lazee-viewer            # run the app
cargo test --workspace               # run the tests
cargo tauri build                    # installer (needs `cargo install tauri-cli`)
```

The UI is plain HTML, CSS and JavaScript in `ui/` with no build step.

Face finding needs ONNX Runtime 1.20 or newer: put `onnxruntime.dll` from
the [Windows release](https://github.com/microsoft/onnxruntime/releases)
next to the exe, or point `ORT_DYLIB_PATH` at it. Without it everything
else works and *Settings* says what is missing. The face tests also
need `LAZEE_FACE_PHOTOS`, a folder with OpenCV's sample `lena.jpg` and
`messi5.jpg`; they are skipped otherwise.

To time indexing (and, with `ORT_DYLIB_PATH` set, face finding) without the UI:

```sh
cargo run --release -p lazee-core --example index -- "D:\Photos"
```

To publish a release, push a tag such as `v0.2.0`, or run the CI workflow
from the Actions tab with the version. CI builds the app, zips it and
attaches the zip to a new GitHub release.

## Layout

| Path | What it is |
| --- | --- |
| `crates/core` | Library index, volumes, EXIF, RAW previews, thumbnails, scanner, faces |
| `models` | Face model packs (manifests and licences; weights are downloaded) |
| `app` | Tauri 2 desktop app: commands and the thumbnail protocol |
| `ui` | The interface |

The library lives in `%LOCALAPPDATA%\io.github.bck539.lazeeviewer`:
`library.sqlite` holds the index and `thumbs.sqlite` the thumbnails.

## Roadmap

1. **Library and grid** (done): indexing, volumes, thumbnails.
2. **Viewer** (done): full-screen view with zoom and pan, folder watching,
   NAS rescans.
3. **Face model spike** (done): YuNet + SFace, run from Rust, as plug-ins.
4. **Faces** (done): background detection, embeddings, unnamed groups, face
   boxes, People page.
5. **People** (done): import names from Windows Photo Gallery and
   IrfanView; name, merge and correct people; suggestions with a review
   queue.
6. **Search** (done): people with AND/OR, date and folder filters, timeline,
   optional XMP write-back (off by default).

## Privacy

Face data never leaves your PC, face scanning is opt-in, and your
photo files are never modified unless you turn on metadata write-back.

## Licence

GPL-3.0. See [LICENSE](LICENSE).
