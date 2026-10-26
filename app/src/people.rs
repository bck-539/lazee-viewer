//! Commands for naming people and reviewing suggestions. They run off the
//! UI thread, since they wait for the library while a scan is writing.

use crate::{err, AppState, CmdResult};
use lazee_core::naming::{Person, Suggestion};
use lazee_core::Library;
use std::sync::Arc;
use tauri::State;

async fn blocking<T: Send + 'static>(
    state: State<'_, AppState>,
    f: impl FnOnce(&Library) -> lazee_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let lib: Arc<Library> = state.lib.clone();
    tauri::async_runtime::spawn_blocking(move || f(&lib)).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn people(state: State<'_, AppState>) -> CmdResult<Vec<Person>> {
    blocking(state, |lib| lib.people()).await
}

#[tauri::command]
pub async fn suggestions(state: State<'_, AppState>, person: Option<i64>) -> CmdResult<Vec<Suggestion>> {
    blocking(state, move |lib| lib.suggestions(person)).await
}

#[tauri::command]
pub async fn name_group(state: State<'_, AppState>, group: i64, name: String) -> CmdResult<i64> {
    blocking(state, move |lib| lib.name_group(group, &name)).await
}

#[tauri::command]
pub async fn name_face(state: State<'_, AppState>, face: i64, name: String) -> CmdResult<i64> {
    blocking(state, move |lib| lib.name_face(face, &name)).await
}

#[tauri::command]
pub async fn unname_face(state: State<'_, AppState>, face: i64) -> CmdResult<()> {
    blocking(state, move |lib| lib.unname_face(face)).await
}

#[tauri::command]
pub async fn accept_suggestion(state: State<'_, AppState>, person: i64, faces: Vec<i64>) -> CmdResult<()> {
    blocking(state, move |lib| lib.accept_suggestion(person, &faces)).await
}

#[tauri::command]
pub async fn reject_suggestion(state: State<'_, AppState>, person: i64, faces: Vec<i64>) -> CmdResult<()> {
    blocking(state, move |lib| lib.reject_suggestion(person, &faces)).await
}

#[tauri::command]
pub async fn rename_person(state: State<'_, AppState>, person: i64, name: String) -> CmdResult<i64> {
    blocking(state, move |lib| lib.rename_person(person, &name)).await
}

#[tauri::command]
pub async fn merge_people(state: State<'_, AppState>, from: i64, into: i64) -> CmdResult<()> {
    blocking(state, move |lib| lib.merge_people(from, into)).await
}

#[tauri::command]
pub async fn delete_person(state: State<'_, AppState>, person: i64) -> CmdResult<()> {
    blocking(state, move |lib| lib.delete_person(person)).await
}
