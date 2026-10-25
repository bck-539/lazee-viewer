// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lazee_core::{Library, PhotoInfo, PhotoList, ScanControl, ScanProgress, SortOrder, Source};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::http::{header, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    lib: Arc<Library>,
    scan: Mutex<Option<Arc<ScanControl>>>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
fn sources(state: State<AppState>) -> CmdResult<Vec<Source>> {
    state.lib.sources().map_err(err)
}

#[tauri::command]
async fn refresh_volumes(state: State<'_, AppState>) -> CmdResult<Vec<Source>> {
    let lib = state.lib.clone();
    tauri::async_runtime::spawn_blocking(move || {
        lib.refresh_volumes();
        lib.sources()
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
fn add_source(state: State<AppState>, path: String) -> CmdResult<Source> {
    state.lib.add_source(path.as_ref()).map_err(err)
}

#[tauri::command]
fn remove_source(state: State<AppState>, id: i64) -> CmdResult<()> {
    if state.scan.lock().unwrap().is_some() {
        return Err("Wait for the scan to finish, or stop it, before removing a folder.".into());
    }
    state.lib.remove_source(id).map_err(err)
}

#[tauri::command]
fn photo_list(state: State<AppState>, sort: SortOrder, source: Option<i64>) -> CmdResult<PhotoList> {
    state.lib.photo_list(sort, source).map_err(err)
}

#[tauri::command]
fn photo_info(state: State<AppState>, id: i64) -> CmdResult<Option<PhotoInfo>> {
    state.lib.photo_info(id).map_err(err)
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    let mut slot = state.scan.lock().unwrap();
    if slot.is_some() {
        return Err("A scan is already running.".into());
    }
    let ctl = Arc::new(ScanControl::default());
    *slot = Some(ctl.clone());
    let lib = state.lib.clone();
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || {
            // Progress arrives far more often than the UI needs it.
            let last = Mutex::new(Instant::now() - Duration::from_secs(1));
            let on_progress = |p: &ScanProgress| {
                let mut last = last.lock().unwrap();
                if last.elapsed() >= Duration::from_millis(150) {
                    *last = Instant::now();
                    let _ = app.emit("scan-progress", p);
                }
            };
            let result = lib.scan(&ctl, &on_progress).map_err(err);
            *app.state::<AppState>().scan.lock().unwrap() = None;
            let _ = app.emit("scan-finished", result);
        })
        .map_err(err)?;
    Ok(())
}

#[tauri::command]
fn pause_scan(state: State<AppState>, paused: bool) {
    if let Some(ctl) = state.scan.lock().unwrap().as_ref() {
        ctl.set_paused(paused);
    }
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    if let Some(ctl) = state.scan.lock().unwrap().as_ref() {
        ctl.cancel();
    }
}

/// Serves `thumb://localhost/<photo id>` (`http://thumb.localhost/<id>` on
/// Windows) from the thumbnail cache, so the grid never touches the photos'
/// own drives.
fn thumbnail_response(lib: &Library, path: &str) -> Response<Vec<u8>> {
    let found = path.trim_start_matches('/').parse::<i64>().ok().map(|id| lib.thumbnail(id));
    match found {
        Some(Ok(Some(jpeg))) => Response::builder()
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "max-age=600")
            .body(jpeg),
        Some(Err(e)) => {
            log::warn!("thumbnail {path}: {e}");
            Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Vec::new())
        }
        _ => Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()),
    }
    .expect("static response parts are valid")
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_local_data_dir()?;
            log::info!("library data in {}", dir.display());
            let lib = Library::open(&dir)?;
            app.manage(AppState { lib: Arc::new(lib), scan: Mutex::new(None) });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, request, responder| {
            let lib = ctx.app_handle().state::<AppState>().lib.clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || responder.respond(thumbnail_response(&lib, &path)));
        })
        .invoke_handler(tauri::generate_handler![
            sources,
            refresh_volumes,
            add_source,
            remove_source,
            photo_list,
            photo_info,
            start_scan,
            pause_scan,
            cancel_scan,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lazee Viewer");
}
