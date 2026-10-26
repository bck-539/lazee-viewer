// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod background;
mod faces;
mod people;

use lazee_core::people::{FaceBox, Group};
use lazee_core::{Filter, FullImage, Library, PhotoInfo, PhotoList, SortOrder, Source};
use std::sync::Arc;
use tauri::http::{header, Response, StatusCode};
use tauri::{AppHandle, Manager, State};

struct AppState {
    lib: Arc<Library>,
    scans: background::Scans,
    watch: background::FolderWatch,
    faces: faces::FaceEngine,
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
fn add_source(app: AppHandle, state: State<AppState>, path: String) -> CmdResult<Source> {
    let source = state.lib.add_source(path.as_ref()).map_err(err)?;
    background::rewatch(&app);
    background::request_scan(&app, Some(vec![source.id]))?;
    Ok(source)
}

#[tauri::command]
fn remove_source(app: AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    if state.scans.is_running() {
        return Err("Wait for the scan to finish, or stop it, before removing a folder.".into());
    }
    state.lib.remove_source(id).map_err(err)?;
    background::rewatch(&app);
    Ok(())
}

#[tauri::command]
fn photo_list(state: State<AppState>, sort: SortOrder, filter: Filter) -> CmdResult<PhotoList> {
    state.lib.photos_of(sort, &filter).map_err(err)
}

#[tauri::command]
fn photo_info(state: State<AppState>, id: i64) -> CmdResult<Option<PhotoInfo>> {
    state.lib.photo_info(id).map_err(err)
}

/// Why the full image for a photo could not be shown, in words for the UI.
#[tauri::command]
async fn photo_problem(state: State<'_, AppState>, id: i64) -> CmdResult<String> {
    let lib = state.lib.clone();
    let problem = tauri::async_runtime::spawn_blocking(move || match lib.full_image(id) {
        Ok(FullImage::Offline(drive)) => format!("Connect {drive} to see the full photo."),
        Ok(FullImage::Missing) => "This photo is no longer where it was. A scan will tidy it up.".into(),
        Ok(FullImage::Jpeg(_)) => "This photo could not be decoded.".into(),
        Err(e) => format!("This photo could not be opened: {e}"),
    })
    .await
    .map_err(err)?;
    Ok(problem)
}

#[tauri::command]
fn start_scan(app: AppHandle) -> CmdResult<()> {
    background::request_scan(&app, None)
}

#[tauri::command]
fn pause_scan(state: State<AppState>, paused: bool) {
    if let Some(ctl) = state.scans.control() {
        ctl.set_paused(paused);
    }
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    state.scans.cancel();
}

#[tauri::command]
fn scan_running(state: State<AppState>) -> bool {
    state.scans.is_running()
}

#[tauri::command]
fn photo_faces(state: State<AppState>, id: i64) -> CmdResult<Vec<FaceBox>> {
    state.lib.photo_faces(id).map_err(err)
}

/// Groups of look-alike faces for the People page. Faces seen only once
/// are left out; they are mostly strangers in the background.
#[tauri::command]
fn face_groups(state: State<AppState>) -> CmdResult<Vec<Group>> {
    state.lib.groups(2).map_err(err)
}

#[tauri::command]
async fn face_setup(app: AppHandle) -> CmdResult<faces::FaceSetup> {
    tauri::async_runtime::spawn_blocking(move || {
        let lib = app.state::<AppState>().lib.clone();
        faces::setup(&app, &lib)
    })
    .await
    .map_err(err)
}

/// Turns face finding on or off, or switches models. Turning it off deletes
/// every face found so far; switching models makes the next scan start the
/// faces over, since different models' results can't be mixed.
#[tauri::command]
fn set_faces(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
    detector: Option<String>,
    embedder: Option<String>,
) -> CmdResult<()> {
    if state.scans.is_running() {
        return Err("Wait for the scan to finish, or stop it, before changing face settings.".into());
    }
    let lib = &state.lib;
    lib.set_setting(faces::DETECTOR, detector.as_deref()).map_err(err)?;
    lib.set_setting(faces::EMBEDDER, embedder.as_deref()).map_err(err)?;
    state.faces.reset();
    if enabled {
        lib.set_setting(faces::ENABLED, Some("1")).map_err(err)?;
        // A scan of no folders goes straight to looking for faces.
        background::request_scan(&app, Some(Vec::new()))
    } else {
        lib.set_setting(faces::ENABLED, None).map_err(err)?;
        lib.forget_faces().map_err(err)
    }
}

/// Turns saving names into the photo files on or off.
#[tauri::command]
fn set_write_back(app: AppHandle, state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let value = enabled.then_some("1");
    state.lib.set_setting(lazee_core::writeback::SETTING, value).map_err(err)?;
    if enabled {
        background::request_scan(&app, Some(Vec::new()))?;
    }
    Ok(())
}

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(status).body(Vec::new()).expect("static response parts are valid")
}

fn jpeg(bytes: Vec<u8>, cache: &str) -> Response<Vec<u8>> {
    Response::builder()
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, cache)
        .body(bytes)
        .expect("static response parts are valid")
}

fn photo_id(path: &str) -> Option<i64> {
    path.trim_start_matches('/').parse().ok()
}

/// Serves `thumb://localhost/<photo id>` (`http://thumb.localhost/<id>` on
/// Windows) from the thumbnail cache, so the grid never touches the photos'
/// own drives.
fn thumbnail_response(lib: &Library, path: &str) -> Response<Vec<u8>> {
    match photo_id(path).map(|id| lib.thumbnail(id)) {
        Some(Ok(Some(bytes))) => jpeg(bytes, "max-age=600"),
        Some(Err(e)) => {
            log::warn!("thumbnail {path}: {e}");
            empty(StatusCode::INTERNAL_SERVER_ERROR)
        }
        _ => empty(StatusCode::NOT_FOUND),
    }
}

/// Serves `photo://localhost/<photo id>`: the full image for the viewer.
fn photo_response(lib: &Library, path: &str) -> Response<Vec<u8>> {
    match photo_id(path).map(|id| lib.full_image(id)) {
        Some(Ok(FullImage::Jpeg(bytes))) => jpeg(bytes, "no-cache"),
        Some(Ok(FullImage::Offline(_))) => empty(StatusCode::SERVICE_UNAVAILABLE),
        Some(Err(e)) => {
            log::warn!("photo {path}: {e}");
            empty(StatusCode::INTERNAL_SERVER_ERROR)
        }
        _ => empty(StatusCode::NOT_FOUND),
    }
}

/// Serves `face://localhost/<face id>`: a face's square thumbnail.
fn face_response(lib: &Library, path: &str) -> Response<Vec<u8>> {
    match photo_id(path).map(|id| lib.face_chip(id)) {
        Some(Ok(Some(bytes))) => jpeg(bytes, "max-age=600"),
        Some(Err(e)) => {
            log::warn!("face {path}: {e}");
            empty(StatusCode::INTERNAL_SERVER_ERROR)
        }
        _ => empty(StatusCode::NOT_FOUND),
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_local_data_dir()?;
            log::info!("library data in {}", dir.display());
            let lib = Library::open(&dir)?;
            app.manage(AppState {
                lib: Arc::new(lib),
                scans: Default::default(),
                watch: Default::default(),
                faces: Default::default(),
            });
            background::rewatch(app.handle());
            background::start_monitor(app.handle().clone());
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, request, responder| {
            let lib = ctx.app_handle().state::<AppState>().lib.clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || responder.respond(thumbnail_response(&lib, &path)));
        })
        .register_asynchronous_uri_scheme_protocol("photo", |ctx, request, responder| {
            let lib = ctx.app_handle().state::<AppState>().lib.clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || responder.respond(photo_response(&lib, &path)));
        })
        .register_asynchronous_uri_scheme_protocol("face", |ctx, request, responder| {
            let lib = ctx.app_handle().state::<AppState>().lib.clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || responder.respond(face_response(&lib, &path)));
        })
        .invoke_handler(tauri::generate_handler![
            sources,
            refresh_volumes,
            add_source,
            remove_source,
            photo_list,
            photo_info,
            photo_problem,
            start_scan,
            pause_scan,
            cancel_scan,
            scan_running,
            photo_faces,
            face_groups,
            face_setup,
            set_faces,
            set_write_back,
            people::people,
            people::suggestions,
            people::name_group,
            people::name_face,
            people::unname_face,
            people::accept_suggestion,
            people::reject_suggestion,
            people::rename_person,
            people::merge_people,
            people::delete_person,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lazee Viewer");
}
