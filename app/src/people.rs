//! Commands for naming people and reviewing suggestions. They run off the
//! UI thread, since they wait for the library while a scan is writing.

use crate::{background, err, AppState, CmdResult};
use lazee_core::naming::{Person, Suggestion};
use lazee_core::sameperson::{FaceItem, Question};
use lazee_core::Library;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Groups with fewer faces are not shown on the People page.
pub const MIN_GROUP: i64 = 2;

/// Saves changed names into the photos, when the user turned that on.
fn names_changed(app: &AppHandle, lib: &Library) {
    if lazee_core::writeback::enabled(lib) {
        let _ = background::request_scan(app, Some(Vec::new()));
    }
}

async fn changing<T: Send + 'static>(
    app: AppHandle,
    state: State<'_, AppState>,
    f: impl FnOnce(&Library) -> lazee_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let lib = state.lib.clone();
    let out = blocking(state, f).await?;
    names_changed(&app, &lib);
    Ok(out)
}

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
pub async fn name_group(app: AppHandle, state: State<'_, AppState>, group: i64, name: String) -> CmdResult<i64> {
    changing(app, state, move |lib| lib.name_group(group, &name)).await
}

#[tauri::command]
pub async fn name_face(app: AppHandle, state: State<'_, AppState>, face: i64, name: String) -> CmdResult<i64> {
    changing(app, state, move |lib| lib.name_face(face, &name)).await
}

#[tauri::command]
pub async fn unname_face(app: AppHandle, state: State<'_, AppState>, face: i64) -> CmdResult<()> {
    changing(app, state, move |lib| lib.unname_face(face)).await
}

#[tauri::command]
pub async fn accept_suggestion(
    app: AppHandle,
    state: State<'_, AppState>,
    person: i64,
    faces: Vec<i64>,
) -> CmdResult<()> {
    changing(app, state, move |lib| lib.accept_suggestion(person, &faces)).await
}

#[tauri::command]
pub async fn reject_suggestion(
    app: AppHandle,
    state: State<'_, AppState>,
    person: i64,
    faces: Vec<i64>,
) -> CmdResult<()> {
    changing(app, state, move |lib| lib.reject_suggestion(person, &faces)).await
}

#[tauri::command]
pub async fn rename_person(app: AppHandle, state: State<'_, AppState>, person: i64, name: String) -> CmdResult<i64> {
    changing(app, state, move |lib| lib.rename_person(person, &name)).await
}

#[tauri::command]
pub async fn merge_people(app: AppHandle, state: State<'_, AppState>, from: i64, into: i64) -> CmdResult<()> {
    changing(app, state, move |lib| lib.merge_people(from, into)).await
}

#[tauri::command]
pub async fn delete_person(app: AppHandle, state: State<'_, AppState>, person: i64) -> CmdResult<()> {
    changing(app, state, move |lib| lib.delete_person(person)).await
}

#[tauri::command]
pub async fn name_faces(app: AppHandle, state: State<'_, AppState>, faces: Vec<i64>, name: String) -> CmdResult<i64> {
    changing(app, state, move |lib| lib.name_faces(&faces, &name)).await
}

#[tauri::command]
pub async fn unname_faces(app: AppHandle, state: State<'_, AppState>, faces: Vec<i64>) -> CmdResult<()> {
    changing(app, state, move |lib| lib.unname_faces(&faces)).await
}

#[tauri::command]
pub async fn faces_of(state: State<'_, AppState>, group: Option<i64>, person: Option<i64>) -> CmdResult<Vec<FaceItem>> {
    blocking(state, move |lib| lib.faces_of(group, person)).await
}

#[tauri::command]
pub async fn same_person_questions(state: State<'_, AppState>) -> CmdResult<Vec<Question>> {
    blocking(state, |lib| lib.same_person_questions(MIN_GROUP, 50)).await
}

#[tauri::command]
pub async fn merge_groups(app: AppHandle, state: State<'_, AppState>, groups: Vec<i64>) -> CmdResult<i64> {
    changing(app, state, move |lib| lib.merge_groups(&groups)).await
}

#[tauri::command]
pub async fn same_person(
    app: AppHandle,
    state: State<'_, AppState>,
    group: i64,
    other_group: Option<i64>,
    person: Option<i64>,
) -> CmdResult<Option<i64>> {
    changing(app, state, move |lib| lib.same_person(group, other_group, person)).await
}

#[tauri::command]
pub async fn not_same_person(
    app: AppHandle,
    state: State<'_, AppState>,
    group: i64,
    other_group: Option<i64>,
    person: Option<i64>,
) -> CmdResult<()> {
    changing(app, state, move |lib| lib.not_same_person(group, other_group, person)).await
}
