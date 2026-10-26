//! What the UI reads about faces: boxes on a photo, groups of look-alikes
//! and their thumbnails, and simple settings.

use crate::library::Library;
use crate::Result;
use rusqlite::{params, OptionalExtension};

/// A face on a photo. The box is in fractions of the photo as displayed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FaceBox {
    pub id: i64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub group: Option<i64>,
}

/// A group of faces that look like the same person.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Group {
    pub id: i64,
    pub faces: i64,
    pub photos: i64,
    /// The clearest face in the group, to show for it.
    pub cover: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FaceStats {
    /// Photos that can be looked at for faces.
    pub photos: i64,
    /// Of those, the ones already looked at with `models`.
    pub scanned: i64,
    pub faces: i64,
    pub models: Option<String>,
}

impl Library {
    pub fn photo_faces(&self, file_id: i64) -> Result<Vec<FaceBox>> {
        let conn = self.reader();
        let mut stmt =
            conn.prepare_cached("SELECT id, x, y, w, h, cluster_id FROM faces WHERE file_id = ?1 ORDER BY x")?;
        let rows = stmt.query_map([file_id], |r| {
            Ok(FaceBox { id: r.get(0)?, x: r.get(1)?, y: r.get(2)?, w: r.get(3)?, h: r.get(4)?, group: r.get(5)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Groups with at least `min_faces` faces, biggest first.
    pub fn groups(&self, min_faces: i64) -> Result<Vec<Group>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT c.id, count(*), count(DISTINCT f.file_id),
                    (SELECT id FROM faces WHERE cluster_id = c.id ORDER BY score * w DESC LIMIT 1)
             FROM clusters c JOIN faces f ON f.cluster_id = c.id
             WHERE c.hidden = 0
             GROUP BY c.id HAVING count(*) >= ?1
             ORDER BY count(*) DESC, c.id",
        )?;
        let rows = stmt.query_map([min_faces], |r| {
            Ok(Group { id: r.get(0)?, faces: r.get(1)?, photos: r.get(2)?, cover: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// A face's square thumbnail, as JPEG bytes.
    pub fn face_chip(&self, face_id: i64) -> Result<Option<Vec<u8>>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached("SELECT jpeg FROM thumbs.face_chips WHERE face_id = ?1")?;
        Ok(stmt.query_row([face_id], |r| r.get(0)).optional()?)
    }

    pub fn face_stats(&self) -> Result<FaceStats> {
        let conn = self.reader();
        let models: Option<String> =
            conn.query_row("SELECT value FROM settings WHERE key = 'face_models'", [], |r| r.get(0)).optional()?;
        let (photos, scanned, faces) = conn.query_row(
            "SELECT (SELECT count(*) FROM files WHERE paired_with IS NULL AND error IS NULL),
                    (SELECT count(*) FROM face_scans s JOIN files f ON f.id = s.file_id
                     WHERE s.model = ?1 AND s.content_hash = f.content_hash),
                    (SELECT count(*) FROM faces)",
            [&models],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(FaceStats { photos, scanned, faces, models })
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.reader();
        Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.writer();
        match value {
            Some(v) => conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, v])?,
            None => conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    /// Deletes every face, group and face thumbnail, for when face finding
    /// is switched off. Overwrites the deleted data on disk too.
    pub fn forget_faces(&self) -> Result<()> {
        let conn = self.writer();
        conn.execute_batch(
            "PRAGMA secure_delete = ON;
             BEGIN;
             DELETE FROM faces; DELETE FROM clusters; DELETE FROM face_scans; DELETE FROM thumbs.face_chips;
             DELETE FROM settings WHERE key = 'face_models';
             COMMIT;
             PRAGMA secure_delete = OFF;",
        )?;
        Ok(())
    }
}
