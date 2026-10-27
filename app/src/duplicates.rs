//! Commands for the Duplicates page: leading and backup folders, the list
//! of photos with copies, and deleting the copies the user picked.

use crate::{background, err, AppState, CmdResult};
use lazee_core::duplicates::{DeleteSummary, DupFolder, DupGroup, Role};
use lazee_core::Library;
use std::sync::Arc;
use tauri::{AppHandle, State};

async fn blocking<T: Send + 'static>(
    state: State<'_, AppState>,
    f: impl FnOnce(&Library) -> lazee_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let lib: Arc<Library> = state.lib.clone();
    tauri::async_runtime::spawn_blocking(move || f(&lib)).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn dup_folders(state: State<'_, AppState>) -> CmdResult<Vec<DupFolder>> {
    blocking(state, |lib| lib.dup_folders()).await
}

#[tauri::command]
pub async fn set_dup_folder(state: State<'_, AppState>, path: String, role: Role) -> CmdResult<()> {
    blocking(state, move |lib| lib.set_dup_folder(path.as_ref(), role)).await
}

#[tauri::command]
pub async fn remove_dup_folder(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    blocking(state, move |lib| lib.remove_dup_folder(id)).await
}

/// Looks for duplicates now, and after every scan from now on. Runs with
/// the scans, so it waits for one that is running.
#[tauri::command]
pub fn find_duplicates(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    state.lib.set_setting(lazee_core::duplicates::SETTING, Some("1")).map_err(err)?;
    background::request_scan(&app, Some(Vec::new()))
}

/// Whether duplicates have been looked for before.
#[tauri::command]
pub fn duplicates_enabled(state: State<AppState>) -> bool {
    lazee_core::duplicates::enabled(&state.lib)
}

#[tauri::command]
pub async fn duplicates(state: State<'_, AppState>) -> CmdResult<Vec<DupGroup>> {
    blocking(state, |lib| lib.duplicates()).await
}

#[tauri::command]
pub async fn delete_duplicates(state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<DeleteSummary> {
    if state.scans.is_running() {
        return Err("Wait for the scan to finish, or stop it, before deleting copies.".into());
    }
    blocking(state, move |lib| lib.delete_duplicates(&ids)).await
}
