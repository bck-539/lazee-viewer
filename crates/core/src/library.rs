//! The library: source folders, the photo index and the thumbnail cache,
//! behind one handle the app keeps for its lifetime.

use crate::scanner::{self, ScanControl, ScanProgress, ScanSummary};
use crate::{db, volume, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, RwLock};

pub struct Library {
    /// Used by scans and changes. Scans hold it for their whole run.
    writer: Mutex<Connection>,
    /// Used for everything the UI reads, so browsing never waits on a scan.
    reader: Mutex<Connection>,
    /// Where each known volume is mounted right now.
    roots: RwLock<HashMap<String, PathBuf>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub id: i64,
    pub volume_id: i64,
    pub volume_key: String,
    pub volume_label: String,
    pub volume_kind: volume::VolumeKind,
    pub rel_path: String,
    /// Full path where the folder is now, or where it was last seen.
    pub display_path: String,
    pub online: bool,
    pub photo_count: i64,
    pub last_scan: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    NewestFirst,
    OldestFirst,
    Name,
    Folder,
}

/// Everything the grid needs: photo ids in display order, plus the ids
/// whose drive is not connected (shown greyed out).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PhotoList {
    pub ids: Vec<i64>,
    pub offline: Vec<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PhotoInfo {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub path: Option<String>,
    pub kind: String,
    pub size: i64,
    pub taken_at: Option<String>,
    pub camera: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub gps: Option<(f64, f64)>,
    pub online: bool,
    pub error: Option<String>,
}

/// The full-size image for the viewer.
pub enum FullImage {
    /// JPEG bytes, ready to show. For a RAW file this is its embedded
    /// preview, tagged with the RAW's orientation.
    Jpeg(Vec<u8>),
    /// The photo's drive is not connected; holds the drive's name.
    Offline(String),
    /// The photo is not in the library, or no longer on disk.
    Missing,
}

impl Library {
    /// Opens or creates the library stored in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let writer = db::open(data_dir)?;
        let reader = db::open(data_dir)?;
        let lib = Library { writer: Mutex::new(writer), reader: Mutex::new(reader), roots: RwLock::default() };
        lib.refresh_volumes();
        Ok(lib)
    }

    pub(crate) fn writer(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn reader(&self) -> MutexGuard<'_, Connection> {
        self.reader.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-checks which volumes are connected and where. Cheap for local
    /// drives; network shares are probed with a short timeout.
    pub fn refresh_volumes(&self) -> HashMap<String, PathBuf> {
        let keys: Vec<String> = self
            .reader()
            .prepare("SELECT key FROM volumes")
            .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let roots = volume::resolve(&keys);
        *self.roots.write().unwrap_or_else(|e| e.into_inner()) = roots.clone();
        roots
    }

    pub(crate) fn roots_snapshot(&self) -> HashMap<String, PathBuf> {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn root_of(&self, key: &str) -> Option<PathBuf> {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    /// Adds a folder to the library. It is indexed on the next scan.
    pub fn add_source(&self, path: &Path) -> Result<Source> {
        if !path.is_dir() {
            return Err(Error::Invalid(format!("{} is not a folder", path.display())));
        }
        let loc = volume::locate(path)?;
        let conn = self.writer();
        conn.execute(
            "INSERT INTO volumes (key, label, kind) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET label = excluded.label",
            params![loc.volume.key, loc.volume.label, loc.volume.kind.as_str()],
        )?;
        let volume_id: i64 =
            conn.query_row("SELECT id FROM volumes WHERE key = ?1", [&loc.volume.key], |r| r.get(0))?;

        // Nested sources would index the same files twice.
        let existing: Vec<String> = conn
            .prepare("SELECT rel_path FROM sources WHERE volume_id = ?1")?
            .query_map([volume_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let new = loc.rel.to_lowercase();
        for e in existing {
            let e = e.to_lowercase();
            if e == new || is_under(&new, &e) {
                return Err(Error::Invalid(format!("{} is already in the library", path.display())));
            }
            if is_under(&e, &new) {
                return Err(Error::Invalid(format!(
                    "{} contains a folder that is already in the library; remove that one first",
                    path.display()
                )));
            }
        }
        conn.execute("INSERT INTO sources (volume_id, rel_path) VALUES (?1, ?2)", params![volume_id, loc.rel])?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.roots.write().unwrap_or_else(|e| e.into_inner()).insert(loc.volume.key, loc.root);
        self.sources()?.into_iter().find(|s| s.id == id).ok_or_else(|| Error::Invalid("source vanished".into()))
    }

    /// Removes a folder and everything indexed under it. The photos
    /// themselves are not touched.
    pub fn remove_source(&self, id: i64) -> Result<()> {
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let Some((volume_id, rel)) = tx
            .query_row("SELECT volume_id, rel_path FROM sources WHERE id = ?1", [id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
        else {
            return Ok(());
        };
        tx.execute(
            "DELETE FROM files WHERE folder_id IN (
                 SELECT id FROM folders WHERE volume_id = ?1
                 AND (?2 = '' OR rel_path = ?2 OR substr(rel_path, 1, length(?2) + 1) = ?2 || '/' COLLATE NOCASE))",
            params![volume_id, rel],
        )?;
        tx.execute("DELETE FROM folders WHERE id NOT IN (SELECT folder_id FROM files)", [])?;
        tx.execute("DELETE FROM sources WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM thumbs.thumbs WHERE content_hash NOT IN (SELECT content_hash FROM files)", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn sources(&self) -> Result<Vec<Source>> {
        let conn = self.reader();
        let mut stmt = conn.prepare(
            "SELECT s.id, v.id, v.key, v.label, v.kind, s.rel_path, s.last_scan,
                    (SELECT count(*) FROM files f JOIN folders fo ON fo.id = f.folder_id
                     WHERE fo.volume_id = v.id AND f.paired_with IS NULL
                       AND (s.rel_path = '' OR fo.rel_path = s.rel_path
                            OR substr(fo.rel_path, 1, length(s.rel_path) + 1) = s.rel_path || '/' COLLATE NOCASE))
             FROM sources s JOIN volumes v ON v.id = s.volume_id
             ORDER BY v.label, s.rel_path",
        )?;
        let rows = stmt.query_map([], |r| {
            let key: String = r.get(2)?;
            let label: String = r.get(3)?;
            let rel: String = r.get(5)?;
            let root = self.root_of(&key);
            let display_path = match &root {
                Some(root) => volume::join(root, &rel).to_string_lossy().into_owned(),
                None if rel.is_empty() => label.clone(),
                None => format!("{label}: {rel}"),
            };
            Ok(Source {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                volume_key: key,
                volume_label: label,
                volume_kind: volume::VolumeKind::parse(&r.get::<_, String>(4)?),
                rel_path: rel,
                display_path,
                online: root.is_some(),
                photo_count: r.get(7)?,
                last_scan: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Photo ids in display order, optionally limited to one source. RAW
    /// files with a JPEG of the same name appear once, as the JPEG.
    pub fn photo_list(&self, sort: SortOrder, source: Option<i64>) -> Result<PhotoList> {
        self.photos_of(sort, source, None)
    }

    /// Like [`Library::photo_list`], optionally limited to photos with a
    /// face from one group of look-alikes.
    pub fn photos_of(&self, sort: SortOrder, source: Option<i64>, group: Option<i64>) -> Result<PhotoList> {
        let order = match sort {
            SortOrder::NewestFirst => "f.sort_date DESC, f.name",
            SortOrder::OldestFirst => "f.sort_date ASC, f.name",
            SortOrder::Name => "f.name, fo.rel_path",
            SortOrder::Folder => "fo.rel_path, f.name",
        };
        let sql = format!(
            "SELECT f.id, v.key FROM files f
             JOIN folders fo ON fo.id = f.folder_id
             JOIN volumes v ON v.id = fo.volume_id
             LEFT JOIN sources s ON s.id = ?1
             WHERE f.paired_with IS NULL
               AND (?1 IS NULL OR (fo.volume_id = s.volume_id AND (s.rel_path = '' OR fo.rel_path = s.rel_path
                    OR substr(fo.rel_path, 1, length(s.rel_path) + 1) = s.rel_path || '/' COLLATE NOCASE)))
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM faces WHERE file_id = f.id AND cluster_id = ?2))
             ORDER BY {order}"
        );
        let roots = self.roots.read().unwrap_or_else(|e| e.into_inner()).clone();
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([source, group])?;
        let mut list = PhotoList::default();
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            list.ids.push(id);
            if !roots.contains_key(r.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?) {
                list.offline.push(id);
            }
        }
        Ok(list)
    }

    pub fn photo_info(&self, id: i64) -> Result<Option<PhotoInfo>> {
        let conn = self.reader();
        let row = conn
            .query_row(
                "SELECT f.name, fo.rel_path, v.key, v.label, f.kind, f.size, f.taken_at, f.camera_make,
                        f.camera_model, f.width, f.height, f.gps_lat, f.gps_lon, f.error
                 FROM files f JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
                 WHERE f.id = ?1",
                [id],
                |r| {
                    let make: Option<String> = r.get(7)?;
                    let model: Option<String> = r.get(8)?;
                    let lat: Option<f64> = r.get(11)?;
                    let lon: Option<f64> = r.get(12)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        PhotoInfo {
                            id,
                            name: String::new(),
                            folder: String::new(),
                            path: None,
                            kind: r.get(4)?,
                            size: r.get(5)?,
                            taken_at: r.get(6)?,
                            camera: camera_name(make, model),
                            width: r.get(9)?,
                            height: r.get(10)?,
                            gps: lat.zip(lon),
                            online: false,
                            error: r.get(13)?,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((name, rel_dir, key, label, mut info)) = row else { return Ok(None) };
        match self.root_of(&key) {
            Some(root) => {
                let folder = volume::join(&root, &rel_dir);
                info.path = Some(folder.join(&name).to_string_lossy().into_owned());
                info.folder = folder.to_string_lossy().into_owned();
                info.online = true;
            }
            None => info.folder = format!("{label}: {rel_dir}"),
        }
        info.name = name;
        Ok(Some(info))
    }

    /// The cached grid thumbnail for a photo, as JPEG bytes.
    pub fn thumbnail(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT t.jpeg FROM files f JOIN thumbs.thumbs t ON t.content_hash = f.content_hash WHERE f.id = ?1",
        )?;
        Ok(stmt.query_row([id], |r| r.get(0)).optional()?)
    }

    /// Reads a photo for full-screen viewing. The original JPEG is passed
    /// through untouched; the WebView decodes it faster than we could.
    pub fn full_image(&self, id: i64) -> Result<FullImage> {
        let row = self
            .reader()
            .query_row(
                "SELECT f.name, fo.rel_path, v.key, v.label, f.kind, f.orientation
                 FROM files f JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
                 WHERE f.id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, u8>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((name, rel_dir, key, label, kind, orientation)) = row else { return Ok(FullImage::Missing) };
        let Some(root) = self.root_of(&key) else { return Ok(FullImage::Offline(label)) };
        let path = volume::join(&root, &rel_dir).join(name);
        let result = if kind == "raw" {
            std::fs::File::open(&path).and_then(|mut f| {
                let len = f.metadata()?.len();
                crate::raw::extract_preview(&mut f, len)
            })
        } else {
            std::fs::read(&path).map(Some)
        };
        match result {
            Ok(Some(jpeg)) if kind == "raw" => Ok(FullImage::Jpeg(crate::raw::set_orientation(&jpeg, orientation))),
            Ok(Some(jpeg)) => Ok(FullImage::Jpeg(jpeg)),
            Ok(None) => Ok(FullImage::Missing),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FullImage::Missing),
            // The drive went away since we last checked.
            Err(_) if !root.exists() => Ok(FullImage::Offline(label)),
            Err(e) => Err(e.into()),
        }
    }

    /// Folders worth watching for changes: sources on connected local and
    /// USB drives. Network shares are rescanned on a timer instead, since
    /// change notifications over SMB are unreliable.
    pub fn watch_targets(&self) -> Result<Vec<(i64, PathBuf)>> {
        let roots = self.roots.read().unwrap_or_else(|e| e.into_inner()).clone();
        Ok(self
            .sources()?
            .into_iter()
            .filter(|s| s.volume_kind != volume::VolumeKind::Network)
            .filter_map(|s| Some((s.id, volume::join(roots.get(&s.volume_key)?, &s.rel_path))))
            .collect())
    }

    /// Indexes every connected source. Blocks until done; run it on a
    /// background thread.
    pub fn scan(&self, ctl: &ScanControl, on_progress: &(dyn Fn(&ScanProgress) + Sync)) -> Result<ScanSummary> {
        scanner::scan(self, None, ctl, on_progress)
    }

    /// Like [`Library::scan`], limited to some sources.
    pub fn scan_sources(
        &self,
        ids: &[i64],
        ctl: &ScanControl,
        on_progress: &(dyn Fn(&ScanProgress) + Sync),
    ) -> Result<ScanSummary> {
        scanner::scan(self, Some(ids), ctl, on_progress)
    }
}

fn is_under(path: &str, dir: &str) -> bool {
    dir.is_empty() && !path.is_empty() || path.starts_with(dir) && path[dir.len()..].starts_with('/')
}

fn camera_name(make: Option<String>, model: Option<String>) -> Option<String> {
    match (make, model) {
        // Models usually repeat the maker ("Canon EOS 80D").
        (Some(make), Some(model)) if model.to_lowercase().starts_with(&make.to_lowercase()) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (make, model) => make.or(model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting() {
        assert!(is_under("a/b", "a"));
        assert!(is_under("a/b", ""));
        assert!(!is_under("ab", "a"));
        assert!(!is_under("a", "a"));
    }

    #[test]
    fn camera_names() {
        assert_eq!(camera_name(Some("Canon".into()), Some("Canon EOS 80D".into())).as_deref(), Some("Canon EOS 80D"));
        assert_eq!(
            camera_name(Some("NIKON CORPORATION".into()), Some("NIKON D750".into())).as_deref(),
            Some("NIKON CORPORATION NIKON D750")
        );
        assert_eq!(camera_name(None, Some("X100V".into())).as_deref(), Some("X100V"));
    }
}
