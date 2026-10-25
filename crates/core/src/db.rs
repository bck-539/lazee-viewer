//! The library database: one SQLite file for the index, plus a second one
//! for thumbnail blobs so the index stays small and quick to back up.
//!
//! Both live in the app's local data folder on the PC's own disk, never next
//! to the photos.

use crate::Result;
use rusqlite::Connection;
use std::path::Path;

const SCHEMA: &[&str] = &[
    // Version 1: volumes, sources, folders and files.
    r#"
    CREATE TABLE volumes (
        id      INTEGER PRIMARY KEY,
        key     TEXT NOT NULL UNIQUE,
        label   TEXT NOT NULL,
        kind    TEXT NOT NULL
    );
    CREATE TABLE sources (
        id        INTEGER PRIMARY KEY,
        volume_id INTEGER NOT NULL REFERENCES volumes(id),
        rel_path  TEXT NOT NULL,
        added_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now')),
        last_scan TEXT,
        UNIQUE (volume_id, rel_path)
    );
    CREATE TABLE folders (
        id        INTEGER PRIMARY KEY,
        volume_id INTEGER NOT NULL REFERENCES volumes(id),
        rel_path  TEXT NOT NULL COLLATE NOCASE,
        UNIQUE (volume_id, rel_path)
    );
    CREATE TABLE files (
        id           INTEGER PRIMARY KEY,
        folder_id    INTEGER NOT NULL REFERENCES folders(id),
        name         TEXT NOT NULL COLLATE NOCASE,
        stem         TEXT NOT NULL COLLATE NOCASE,
        kind         TEXT NOT NULL,
        size         INTEGER NOT NULL,
        mtime        INTEGER NOT NULL,
        content_hash BLOB NOT NULL,
        taken_at     TEXT,
        sort_date    TEXT NOT NULL,
        camera_make  TEXT,
        camera_model TEXT,
        orientation  INTEGER NOT NULL DEFAULT 1,
        width        INTEGER,
        height       INTEGER,
        gps_lat      REAL,
        gps_lon      REAL,
        -- For a RAW file with a JPEG of the same name next to it: the JPEG.
        -- The grid shows the pair once.
        paired_with  INTEGER REFERENCES files(id) ON DELETE SET NULL,
        has_thumb    INTEGER NOT NULL DEFAULT 0,
        error        TEXT,
        UNIQUE (folder_id, name)
    );
    CREATE INDEX files_sort_date ON files(sort_date);
    CREATE INDEX files_hash ON files(content_hash);
    CREATE INDEX files_stem ON files(folder_id, stem);
    "#,
];

const THUMBS_SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS thumbs.thumbs (
        content_hash BLOB PRIMARY KEY,
        width        INTEGER NOT NULL,
        height       INTEGER NOT NULL,
        jpeg         BLOB NOT NULL
    ) WITHOUT ROWID;
"#;

/// Opens the index with the thumbnail store attached as `thumbs`, and brings
/// the schema up to date.
pub fn open(dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(dir)?;
    let conn = Connection::open(dir.join("library.sqlite"))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA cache_size = -32000;",
    )?;
    let thumbs = dir.join("thumbs.sqlite");
    conn.execute("ATTACH DATABASE ?1 AS thumbs", [thumbs.to_string_lossy()])?;
    conn.execute_batch("PRAGMA thumbs.journal_mode = WAL; PRAGMA thumbs.synchronous = NORMAL;")?;
    conn.execute_batch(THUMBS_SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in SCHEMA.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_twice() {
        let dir = tempfile::tempdir().unwrap();
        drop(open(dir.path()).unwrap());
        let conn = open(dir.path()).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, SCHEMA.len());
        conn.execute("SELECT count(*) FROM thumbs.thumbs", []).ok();
    }
}
