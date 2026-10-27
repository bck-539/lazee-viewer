//! Face detection and recognition, on the user's own PC.
//!
//! Models are plug-ins (see [`pack`]): a detector finds faces and their
//! landmarks, and an embedder turns each aligned face into a vector. Both
//! run on ONNX Runtime, which is loaded from a DLL at run time so the app
//! starts, and browsing works, even when it is missing.

pub mod align;
pub mod detect;
pub mod embed;
pub mod pack;

use crate::{Error, Result};
use image::RgbImage;
use ort::environment::Environment;
use pack::{Kind, Pack};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use detect::Detector;
pub use embed::Embedder;
pub use ort::environment::Environment as Runtime;

/// A face in pixel coordinates of the image it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Left eye, right eye, nose tip, left and right mouth corners.
    pub landmarks: [(f32, f32); 5],
}

static ENVIRONMENT: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

/// Loads ONNX Runtime from `library` the first time it is called; later
/// calls return the same runtime, whatever path they pass.
pub fn runtime(library: &Path) -> Result<&'static Environment> {
    ENVIRONMENT
        .get_or_init(|| {
            let builder = ort::init_from(library).map_err(|e| match load_problem(library) {
                Some(why) => format!("cannot load {}: {why}", library.display()),
                None => format!("cannot load {}: {e}", library.display()),
            })?;
            builder.with_name("lazee").build().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| Error::Invalid(format!("ONNX Runtime: {e}")))
}

/// The Microsoft Visual C++ runtime DLLs that ONNX Runtime needs. The app
/// ships them next to the exe, because many PCs only have an older copy.
pub const VC_RUNTIME: [&str; 4] = ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "msvcp140_1.dll"];

#[cfg(windows)]
const VC_REDIST_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// Says in plain words why Windows won't load `library`, by loading it
/// again and looking at the error code. `None` if it loads now.
#[cfg(windows)]
fn load_problem(library: &Path) -> Option<String> {
    let code = try_load(library)?;
    let reinstall = format!(
        "This usually means the Microsoft Visual C++ runtime is missing or too old. Lazee Viewer \
         brings its own copy ({}) next to lazee-viewer.exe: unzip all files of the download into \
         one folder, or install the runtime from {VC_REDIST_URL}.",
        VC_RUNTIME.join(", ")
    );
    Some(match code {
        // ERROR_MOD_NOT_FOUND: a DLL it depends on is missing.
        126 => {
            let missing: Vec<&str> = VC_RUNTIME.iter().copied().filter(|n| try_load(Path::new(n)).is_some()).collect();
            if missing.is_empty() {
                format!("Windows can't find a file it needs (error 126). {reinstall}")
            } else {
                format!("Windows can't find {} (error 126). {reinstall}", missing.join(", "))
            }
        }
        // ERROR_BAD_EXE_FORMAT
        193 => "it isn't a 64-bit Windows DLL (error 193); use onnxruntime.dll from the Lazee Viewer download.".into(),
        // ERROR_DLL_INIT_FAILED, ERROR_PROC_NOT_FOUND: usually an older runtime DLL is picked up.
        1114 | 127 => format!("it doesn't start (error {code}). {reinstall}"),
        // ERROR_ACCESS_DENIED: often an antivirus quarantining the file.
        5 => "Windows denied access to it (error 5); an antivirus program may be blocking it.".into(),
        _ => format!("Windows error {code}. {reinstall}"),
    })
}

#[cfg(not(windows))]
fn load_problem(_library: &Path) -> Option<String> {
    None
}

/// Loads and frees a DLL; returns the Windows error code if it fails.
#[cfg(windows)]
fn try_load(library: &Path) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError};
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryExW;
    let wide: Vec<u16> = library.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path; the module is freed at once.
    unsafe {
        let module = LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0);
        if module.is_null() {
            return Some(GetLastError());
        }
        FreeLibrary(module);
    }
    None
}

/// Where to look for the ONNX Runtime library: next to the app first, then
/// wherever `ORT_DYLIB_PATH` points.
pub fn runtime_candidates(app_dir: &Path) -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let mut out = vec![app_dir.join(name)];
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        out.push(PathBuf::from(p));
    }
    out
}

/// A detector and an embedder, ready to use.
pub struct Engine {
    pub detector: Detector,
    pub embedder: Embedder,
}

/// One face found in a photo, with its embedding.
#[derive(Debug, Clone)]
pub struct Found {
    pub face: Face,
    pub embedding: Vec<f32>,
}

impl Engine {
    pub fn new(env: &Environment, detector: Pack, embedder: Pack, threads: usize) -> Result<Self> {
        if detector.manifest.kind != Kind::Detector {
            return Err(Error::Invalid(format!("{} is not a face detector", detector.id())));
        }
        if embedder.manifest.kind != Kind::Embedder {
            return Err(Error::Invalid(format!("{} is not a face embedder", embedder.id())));
        }
        let session = |pack: &Pack| -> Result<ort::session::Session> {
            let fail = |e: &dyn std::fmt::Display| Error::Invalid(format!("cannot load model {}: {e}", pack.id()));
            let builder = ort::session::Session::builder(env).map_err(|e| fail(&e))?;
            let mut builder = builder.with_intra_threads(threads.max(1)).map_err(|e| fail(&e))?;
            builder.commit_from_file(pack.model_path()).map_err(|e| fail(&e))
        };
        Ok(Engine {
            detector: Detector::new(detector.clone(), session(&detector)?),
            embedder: Embedder::new(embedder.clone(), session(&embedder)?),
        })
    }

    /// Identifies the model pair. Faces found with a different pair are
    /// looked for again, so switching models never mixes embeddings that
    /// can't be compared.
    pub fn key(&self) -> String {
        format!("{}+{}", self.detector.id(), self.embedder.id())
    }

    /// Finds faces at least `min_size` pixels across (in `img`'s pixels)
    /// and computes their embeddings.
    pub fn analyze(&mut self, img: &RgbImage, min_size: f32) -> Result<Vec<Found>> {
        let faces: Vec<Face> = self.detector.detect(img)?.into_iter().filter(|f| f.w.min(f.h) >= min_size).collect();
        let embeddings = self.embedder.embed(img, &faces)?;
        Ok(faces.into_iter().zip(embeddings).map(|(face, embedding)| Found { face, embedding }).collect())
    }
}
