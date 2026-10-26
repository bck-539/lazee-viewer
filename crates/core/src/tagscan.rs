//! The tag scan: reads the names, keywords and captions other programs
//! saved in each photo, once per version of the file.
//!
//! People named in the tags become people in the library, and a name with a
//! face area is put on the face found there, so years of tagging in Windows
//! Photo Gallery or IrfanView carry over.

use crate::library::Library;
use crate::scanner::ScanControl;
use crate::tags::{self, Kind};
use crate::{naming, volume, Result};
use rayon::prelude::*;
use rusqlite::{params, Connection};
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TagProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TagSummary {
    pub photos: u64,
    pub tags: u64,
    pub people: u64,
    pub failed: u64,
    pub cancelled: bool,
}

struct Todo {
    file_id: i64,
    path: PathBuf,
    raw: bool,
    hash: Vec<u8>,
}

/// Photos read per transaction.
const BATCH: usize = 100;

pub fn scan(lib: &Library, ctl: &ScanControl, on_progress: &(dyn Fn(&TagProgress) + Sync)) -> Result<TagSummary> {
    let todo = load_todo(&lib.reader(), &lib.roots_snapshot())?;
    let total = todo.len() as u64;
    let mut summary = TagSummary::default();
    if todo.is_empty() {
        return Ok(summary);
    }
    let people_before = count_people(&lib.reader())?;
    on_progress(&TagProgress { done: 0, total });
    // Reading tags is mostly waiting on the disk; a few threads keep a
    // network drive busy without flooding it.
    let pool =
        rayon::ThreadPoolBuilder::new().num_threads(4).build().map_err(|e| crate::Error::Invalid(e.to_string()))?;
    for chunk in todo.chunks(BATCH) {
        if !ctl.checkpoint() {
            break;
        }
        let read: Vec<_> =
            pool.install(|| chunk.par_iter().map(|t| (t, tags::read(&t.path, t.raw))).collect::<Vec<_>>());
        let mut conn = lib.writer();
        let tx = conn.transaction()?;
        for (t, result) in read {
            match result {
                Ok(found) => {
                    summary.tags += found.len() as u64;
                    store(&tx, t, &found)?;
                }
                Err(e) => {
                    log::warn!("tags in {}: {e}", t.path.display());
                    summary.failed += 1;
                    tx.execute(
                        "INSERT OR REPLACE INTO tag_scans (file_id, content_hash, error) VALUES (?1, ?2, ?3)",
                        params![t.file_id, t.hash, e.to_string()],
                    )?;
                }
            }
            summary.photos += 1;
        }
        tx.commit()?;
        drop(conn);
        on_progress(&TagProgress { done: summary.photos, total });
    }
    let conn = lib.writer();
    summary.people = count_people(&conn)?.saturating_sub(people_before);
    naming::refresh(&conn)?;
    summary.cancelled = ctl.is_cancelled();
    Ok(summary)
}

fn count_people(conn: &Connection) -> Result<u64> {
    Ok(conn.query_row("SELECT count(*) FROM people", [], |r| r.get::<_, i64>(0))? as u64)
}

fn load_todo(conn: &Connection, roots: &std::collections::HashMap<String, PathBuf>) -> Result<Vec<Todo>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, v.key, fo.rel_path, f.name, f.kind, f.content_hash
         FROM files f
         JOIN folders fo ON fo.id = f.folder_id
         JOIN volumes v ON v.id = fo.volume_id
         LEFT JOIN tag_scans s ON s.file_id = f.id
         WHERE f.paired_with IS NULL AND f.error IS NULL
           AND (s.file_id IS NULL OR s.content_hash != f.content_hash)
         ORDER BY f.sort_date DESC, f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Vec<u8>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file_id, vol, rel, name, kind, hash) = row?;
        if let Some(root) = roots.get(&vol) {
            out.push(Todo { file_id, path: volume::join(root, &rel).join(name), raw: kind == "raw", hash });
        }
    }
    Ok(out)
}

fn store(tx: &Connection, t: &Todo, found: &[tags::Tag]) -> Result<()> {
    store_tags(tx, t.file_id, found)?;
    tx.execute(
        "INSERT OR REPLACE INTO tag_scans (file_id, content_hash, error) VALUES (?1, ?2, NULL)",
        params![t.file_id, t.hash],
    )?;
    Ok(())
}

/// Replaces a photo's tags with those just read from it, and puts their
/// names on its faces.
pub(crate) fn store_tags(tx: &Connection, file_id: i64, found: &[tags::Tag]) -> Result<()> {
    tx.execute("DELETE FROM tags WHERE file_id = ?1", [file_id])?;
    for tag in found {
        let person = if tag.kind == Kind::Person { Some(naming::person_id(tx, &tag.value)?) } else { None };
        let a = tag.area;
        tx.execute(
            "INSERT INTO tags (file_id, kind, value, source, x, y, w, h, person_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                file_id,
                tag.kind.as_str(),
                tag.value,
                tag.source,
                a.map(|a| a.x),
                a.map(|a| a.y),
                a.map(|a| a.w),
                a.map(|a| a.h),
                person
            ],
        )?;
    }
    naming::link_file(tx, file_id)
}
