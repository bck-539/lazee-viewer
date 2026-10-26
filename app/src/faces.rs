//! Face finding in the app: where the models and ONNX Runtime are, which
//! models the user picked, and the engine built from them.

use lazee_core::faces::{self, pack, Engine};
use lazee_core::people::FaceStats;
use lazee_core::Library;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

pub const ENABLED: &str = "faces_enabled";
pub const DETECTOR: &str = "face_detector";
pub const EMBEDDER: &str = "face_embedder";

/// The loaded models, kept between scans since loading takes a while.
#[derive(Default)]
pub struct FaceEngine {
    engine: Mutex<Option<Engine>>,
}

impl FaceEngine {
    /// Drops the loaded models, so the next scan loads the ones now chosen.
    pub fn reset(&self) {
        *self.engine.lock().unwrap() = None;
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PackInfo {
    pub id: String,
    pub name: String,
    pub licence: String,
    pub folder: String,
    /// Why the pack can't be used, such as its model file missing.
    pub problem: Option<String>,
}

/// Everything the settings panel shows about face finding.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FaceSetup {
    pub enabled: bool,
    pub detectors: Vec<PackInfo>,
    pub embedders: Vec<PackInfo>,
    pub detector: Option<String>,
    pub embedder: Option<String>,
    /// Why faces can't be found right now, if they can't.
    pub problem: Option<String>,
    pub stats: FaceStats,
    /// Where the user can add model packs of their own.
    pub user_models: String,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
}

/// Folders holding model packs: next to the app, then in the user's data
/// folder, where people can drop packs of their own.
pub fn pack_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = vec![exe_dir().join("models")];
    if let Ok(data) = app.path().app_local_data_dir() {
        dirs.push(data.join("models"));
    }
    dirs
}

fn runtime() -> Result<&'static faces::Runtime, String> {
    let candidates = faces::runtime_candidates(&exe_dir());
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        return Err(format!("ONNX Runtime is missing; it should be at {}.", candidates[0].display()));
    };
    faces::runtime(path).map_err(|e| e.to_string())
}

pub fn enabled(lib: &Library) -> bool {
    lib.setting(ENABLED).ok().flatten().as_deref() == Some("1")
}

fn info(p: &pack::Pack) -> PackInfo {
    PackInfo {
        id: p.id().to_string(),
        name: p.manifest.name.clone(),
        licence: p.manifest.licence.clone(),
        folder: p.dir.display().to_string(),
        problem: (!p.model_path().exists()).then(|| format!("{} is missing", p.manifest.file)),
    }
}

/// The chosen pack of a kind, or the first usable one.
fn choose<'a>(packs: &'a [pack::Pack], kind: pack::Kind, chosen: Option<&str>) -> Option<&'a pack::Pack> {
    let usable = |p: &&pack::Pack| p.manifest.kind == kind && p.model_path().exists();
    chosen.and_then(|id| packs.iter().filter(usable).find(|p| p.id() == id)).or_else(|| packs.iter().find(usable))
}

pub fn setup(app: &AppHandle, lib: &Library) -> FaceSetup {
    let packs = pack::discover(&pack_dirs(app));
    let of = |kind| packs.iter().filter(|p| p.manifest.kind == kind).map(info).collect::<Vec<_>>();
    let chosen_det = lib.setting(DETECTOR).ok().flatten();
    let chosen_emb = lib.setting(EMBEDDER).ok().flatten();
    let detector = choose(&packs, pack::Kind::Detector, chosen_det.as_deref());
    let embedder = choose(&packs, pack::Kind::Embedder, chosen_emb.as_deref());
    let problem = match (detector, embedder) {
        (None, _) => Some("No face detector model was found in the models folder.".to_string()),
        (_, None) => Some("No face recognition model was found in the models folder.".to_string()),
        _ => runtime().err(),
    };
    FaceSetup {
        enabled: enabled(lib),
        detectors: of(pack::Kind::Detector),
        embedders: of(pack::Kind::Embedder),
        detector: detector.map(|p| p.id().to_string()),
        embedder: embedder.map(|p| p.id().to_string()),
        problem,
        stats: lib.face_stats().unwrap_or_default(),
        user_models: pack_dirs(app).last().map(|d| d.display().to_string()).unwrap_or_default(),
    }
}

/// Runs `f` with the engine for the chosen models, loading them first if
/// needed.
pub fn with_engine<T>(app: &AppHandle, f: impl FnOnce(&mut Engine) -> T) -> Result<T, String> {
    let state = app.state::<crate::AppState>();
    let mut slot = state.faces.engine.lock().unwrap();
    if slot.is_none() {
        let lib = &state.lib;
        let packs = pack::discover(&pack_dirs(app));
        let det = lib.setting(DETECTOR).ok().flatten();
        let emb = lib.setting(EMBEDDER).ok().flatten();
        let detector = choose(&packs, pack::Kind::Detector, det.as_deref()).ok_or("no face detector model")?;
        let embedder = choose(&packs, pack::Kind::Embedder, emb.as_deref()).ok_or("no face recognition model")?;
        // Leave cores for the photo decoders and the rest of the PC.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 4);
        let engine = Engine::new(runtime()?, detector.clone(), embedder.clone(), threads).map_err(|e| e.to_string())?;
        log::info!("face models loaded: {}", engine.key());
        *slot = Some(engine);
    }
    Ok(f(slot.as_mut().expect("engine was just loaded")))
}
