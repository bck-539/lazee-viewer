//! Finding photos that exist more than once, and deleting the extra copies.
//!
//! Two files are copies when their pictures are byte-for-byte the same. For
//! JPEGs the metadata segments (EXIF, XMP, ICC, comments) are left out of the
//! comparison, so a copy that another program tagged or re-dated still
//! counts. Other files must match completely. A RAW and its JPEG are never
//! copies of each other.
//!
//! Comparing whole files means reading them, which is slow over a network,
//! so it happens in two steps. First a cheap key per file: for a JPEG a hash
//! of its last few KiB, which is picture data and survives metadata edits;
//! for a RAW the content hash the index already has. Only files that share a
//! key with another file are then read in full. Both results are kept, and
//! only worked out again when a file changes.
//!
//! The user can mark folders as *leading* (the copy there is the one to keep)
//! or *backup* (copies there are expected and never picked for deletion).
//! Deleting sends files to the Recycle Bin; drives without one (network
//! shares, USB sticks) get a `.Deleted by Lazee Viewer` folder in the photo
//! folder instead, so nothing is ever deleted outright.

use crate::formats::FileKind;
use crate::hash::ContentHash;
use crate::library::Library;
use crate::scanner::ScanControl;
use crate::volume::{self, VolumeKind};
use crate::{Error, Result};
use rayon::prelude::*;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

/// Setting that keeps duplicate keys up to date after each scan, once the
/// user has looked for duplicates.
pub const SETTING: &str = "duplicates";

/// Where files go on drives without a Recycle Bin. Starts with a dot, so
/// scans skip it.
pub const ASIDE_FOLDER: &str = ".Deleted by Lazee Viewer";

/// How much of a JPEG's end the quick key covers.
const TAIL: u64 = 8 * 1024;

pub fn enabled(lib: &Library) -> bool {
    lib.setting(SETTING).ok().flatten().is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Copies here are kept.
    Leading,
    /// Copies here are expected and never picked for deletion.
    Backup,
    /// Anywhere else.
    Other,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Leading => "leading",
            Role::Backup => "backup",
            Role::Other => "other",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "leading" => Role::Leading,
            "backup" => Role::Backup,
            _ => Role::Other,
        }
    }

    /// Which copy to keep first.
    fn rank(self) -> u8 {
        match self {
            Role::Leading => 0,
            Role::Other => 1,
            Role::Backup => 2,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DupFolder {
    pub id: i64,
    pub role: Role,
    pub path: String,
    pub online: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DupProgress {
    /// `quick` while reading file ends, `full` while comparing whole files.
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DupSummary {
    /// Photos that have at least one extra copy.
    pub groups: u64,
    /// Extra copies, beyond one of each photo.
    pub copies: u64,
    /// Files that could not be read.
    pub failed: u64,
    pub cancelled: bool,
}

/// One photo and all its copies, the one to keep first.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DupGroup {
    pub key: String,
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub taken_at: Option<String>,
    pub copies: Vec<DupCopy>,
    /// Every extra copy is in a backup folder, so nothing needs doing.
    pub expected: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DupCopy {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub path: Option<String>,
    pub online: bool,
    pub role: Role,
    pub size: i64,
    /// Last changed, as `YYYY-MM-DDTHH:MM:SS` in UTC.
    pub modified: String,
    /// Picked for deletion unless the user says otherwise.
    pub suggested: bool,
    /// The drive has no Recycle Bin, so deleting moves it aside.
    pub no_recycle_bin: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DeleteSummary {
    /// Sent to the Recycle Bin.
    pub recycled: u64,
    /// Moved into a `.Deleted by Lazee Viewer` folder; holds those folders.
    pub moved_aside: u64,
    pub aside_folders: Vec<String>,
    pub bytes: u64,
    /// Face names copied over to the copy that was kept.
    pub names_kept: u64,
    /// Files left alone, with the reason.
    pub failed: Vec<(String, String)>,
}

/// A JPEG's picture without its metadata, or the whole of any other file,
/// hashed. JPEGs that can't be parsed are hashed whole.
pub fn picture_hash(kind: FileKind, path: &Path) -> std::io::Result<ContentHash> {
    let mut h = blake3::Hasher::new();
    h.update(kind.as_str().as_bytes());
    match kind {
        FileKind::Jpeg => {
            let data = std::fs::read(path)?;
            let mut picture = blake3::Hasher::new();
            if jpeg_picture(&data, &mut picture) {
                h.update(b":picture:");
                h.update(picture.finalize().as_bytes());
            } else {
                h.update(b":whole:");
                h.update(&data);
            }
        }
        FileKind::Raw => {
            h.update(b":whole:");
            std::io::copy(&mut File::open(path)?, &mut h)?;
        }
    }
    let mut out = [0; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    Ok(out)
}

/// Feeds a JPEG's segments to `h`, skipping comments and application
/// segments other than Adobe's APP14 (which changes how colours decode).
/// Everything from the first scan on is picture data and goes in whole.
/// Returns false if this doesn't look like a JPEG.
fn jpeg_picture(data: &[u8], h: &mut blake3::Hasher) -> bool {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return false;
    }
    let mut i = 2;
    loop {
        if data.get(i) != Some(&0xFF) {
            return false;
        }
        while data.get(i) == Some(&0xFF) {
            i += 1;
        }
        let Some(&marker) = data.get(i) else { return false };
        i += 1;
        match marker {
            0x01 | 0xD0..=0xD8 => continue,
            0xD9 => return true,
            _ => {}
        }
        let Some(len) = data.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else { return false };
        if len < 2 || i + len > data.len() {
            return false;
        }
        if marker == 0xDA {
            h.update(&data[i - 2..]);
            return true;
        }
        let metadata = (0xE0..=0xEF).contains(&marker) && marker != 0xEE || marker == 0xFE;
        if !metadata {
            h.update(&[0xFF, marker]);
            h.update(&data[i..i + len]);
        }
        i += len;
    }
}

/// The quick key: a hash of the file's last few KiB.
fn tail_key(path: &Path) -> std::io::Result<ContentHash> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    let mut h = blake3::Hasher::new();
    h.update(b"jpeg-tail");
    h.update(&buf);
    let mut out = [0; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    Ok(out)
}

fn kind_of(s: &str) -> FileKind {
    if s == "raw" {
        FileKind::Raw
    } else {
        FileKind::Jpeg
    }
}

/// A file as the duplicate finder sees it.
struct Entry {
    id: i64,
    kind: FileKind,
    hash: ContentHash,
    path: Option<PathBuf>,
    quick: Option<ContentHash>,
    picture: Option<ContentHash>,
}

fn hash16(v: Vec<u8>) -> ContentHash {
    v.try_into().unwrap_or_default()
}

/// Brings the quick keys and picture hashes up to date. Runs after scans.
pub fn find(lib: &Library, ctl: &ScanControl, on_progress: &(dyn Fn(&DupProgress) + Sync)) -> Result<DupSummary> {
    let roots = lib.roots_snapshot();
    let mut entries: Vec<Entry> = {
        let conn = lib.reader();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.kind, f.content_hash, fo.rel_path, f.name, v.key, d.quick, d.picture
             FROM files f JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
             LEFT JOIN dup_hashes d ON d.file_id = f.id AND d.content_hash = f.content_hash
             WHERE f.error IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            let rel: String = r.get(3)?;
            let name: String = r.get(4)?;
            let key: String = r.get(5)?;
            Ok(Entry {
                id: r.get(0)?,
                kind: kind_of(&r.get::<_, String>(1)?),
                hash: hash16(r.get(2)?),
                path: roots.get(&key).map(|root| volume::join(root, &rel).join(name)),
                quick: r.get::<_, Option<Vec<u8>>>(6)?.map(hash16),
                picture: r.get::<_, Option<Vec<u8>>>(7)?.map(hash16),
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let mut summary = DupSummary::default();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("dupes-{i}"))
        .build()
        .map_err(|e| Error::Invalid(e.to_string()))?;

    // Step 1: quick keys. A RAW's content hash already is one.
    let mut fresh: Vec<(i64, ContentHash, ContentHash)> = Vec::new();
    for e in entries.iter_mut().filter(|e| e.quick.is_none() && e.kind == FileKind::Raw) {
        e.quick = Some(e.hash);
        fresh.push((e.id, e.hash, e.hash));
    }
    save_quick(lib, &fresh)?;
    let todo: Vec<usize> =
        (0..entries.len()).filter(|&i| entries[i].quick.is_none() && entries[i].path.is_some()).collect();
    let total = todo.len() as u64;
    let done = AtomicU64::new(0);
    let report = |phase| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 50 == 0 || n == total {
            on_progress(&DupProgress { phase, done: n, total });
        }
    };
    on_progress(&DupProgress { phase: "quick", done: 0, total });
    for chunk in todo.chunks(500) {
        if !ctl.checkpoint() {
            summary.cancelled = true;
            return Ok(summary);
        }
        let keys: Vec<(usize, std::io::Result<ContentHash>)> = pool.install(|| {
            chunk
                .par_iter()
                .map(|&i| {
                    let key = tail_key(entries[i].path.as_ref().expect("filtered on path"));
                    report("quick");
                    (i, key)
                })
                .collect()
        });
        let mut fresh = Vec::with_capacity(keys.len());
        for (i, key) in keys {
            match key {
                Ok(k) => {
                    entries[i].quick = Some(k);
                    fresh.push((entries[i].id, entries[i].hash, k));
                }
                Err(e) => {
                    log::warn!("duplicates: could not read {}: {e}", entries[i].path.as_ref().unwrap().display());
                    summary.failed += 1;
                }
            }
        }
        save_quick(lib, &fresh)?;
    }

    // Step 2: read in full only files that share a quick key with another.
    let mut by_quick: HashMap<(FileKind, ContentHash), Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(q) = e.quick {
            by_quick.entry((e.kind, q)).or_default().push(i);
        }
    }
    let todo: Vec<usize> = by_quick
        .values()
        .filter(|v| v.len() > 1)
        .flatten()
        .copied()
        .filter(|&i| entries[i].picture.is_none() && entries[i].path.is_some())
        .collect();
    let total = todo.len() as u64;
    done.store(0, Ordering::Relaxed);
    let report = |phase| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 10 == 0 || n == total {
            on_progress(&DupProgress { phase, done: n, total });
        }
    };
    on_progress(&DupProgress { phase: "full", done: 0, total });
    for chunk in todo.chunks(100) {
        if !ctl.checkpoint() {
            summary.cancelled = true;
            return Ok(summary);
        }
        let hashes: Vec<(usize, std::io::Result<ContentHash>)> = pool.install(|| {
            chunk
                .par_iter()
                .map(|&i| {
                    let e = &entries[i];
                    let h = picture_hash(e.kind, e.path.as_ref().expect("filtered on path"));
                    report("full");
                    (i, h)
                })
                .collect()
        });
        let conn = lib.writer();
        let tx = conn.unchecked_transaction()?;
        for (i, h) in hashes {
            match h {
                Ok(h) => {
                    entries[i].picture = Some(h);
                    tx.execute(
                        "UPDATE dup_hashes SET picture = ?1 WHERE file_id = ?2",
                        params![&h[..], entries[i].id],
                    )?;
                }
                Err(e) => {
                    log::warn!("duplicates: could not read {}: {e}", entries[i].path.as_ref().unwrap().display());
                    summary.failed += 1;
                }
            }
        }
        tx.commit()?;
    }

    let mut by_picture: HashMap<ContentHash, u64> = HashMap::new();
    for p in entries.iter().filter_map(|e| e.picture) {
        *by_picture.entry(p).or_default() += 1;
    }
    for &n in by_picture.values().filter(|&&n| n > 1) {
        summary.groups += 1;
        summary.copies += n - 1;
    }
    Ok(summary)
}

fn save_quick(lib: &Library, rows: &[(i64, ContentHash, ContentHash)]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let conn = lib.writer();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO dup_hashes (file_id, content_hash, quick, picture) VALUES (?1, ?2, ?3, NULL)",
        )?;
        for (id, hash, quick) in rows {
            stmt.execute(params![id, &hash[..], &quick[..]])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// A folder role as stored: which volume, where on it, and the role.
struct RoleFolder {
    volume_id: i64,
    rel: String,
    role: Role,
}

fn role_folders(conn: &Connection) -> Result<Vec<RoleFolder>> {
    let mut stmt = conn.prepare("SELECT volume_id, rel_path, role FROM dup_folders")?;
    let rows = stmt.query_map([], |r| {
        Ok(RoleFolder {
            volume_id: r.get(0)?,
            rel: r.get::<_, String>(1)?.to_lowercase(),
            role: Role::parse(&r.get::<_, String>(2)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The role of a folder: that of the closest marked folder above it.
fn role_of(folders: &[RoleFolder], volume_id: i64, rel: &str) -> Role {
    let rel = rel.to_lowercase();
    folders
        .iter()
        .filter(|f| {
            f.volume_id == volume_id
                && (f.rel.is_empty() || rel == f.rel || rel.starts_with(&f.rel) && rel[f.rel.len()..].starts_with('/'))
        })
        .max_by_key(|f| f.rel.len())
        .map_or(Role::Other, |f| f.role)
}

/// Seconds since 1970 to `YYYY-MM-DDTHH:MM:SS` in UTC.
fn iso(secs: i64) -> String {
    crate::scanner::unix_to_iso(secs)
}

impl Library {
    /// Folders marked as leading or backup.
    pub fn dup_folders(&self) -> Result<Vec<DupFolder>> {
        let roots = self.roots_snapshot();
        let conn = self.reader();
        let mut stmt = conn.prepare(
            "SELECT d.rowid, d.role, d.rel_path, v.key, v.label FROM dup_folders d JOIN volumes v ON v.id = d.volume_id
             ORDER BY d.role DESC, v.label, d.rel_path",
        )?;
        let rows = stmt.query_map([], |r| {
            let rel: String = r.get(2)?;
            let key: String = r.get(3)?;
            let label: String = r.get(4)?;
            let root = roots.get(&key);
            Ok(DupFolder {
                id: r.get(0)?,
                role: Role::parse(&r.get::<_, String>(1)?),
                path: match root {
                    Some(root) => volume::join(root, &rel).to_string_lossy().into_owned(),
                    None if rel.is_empty() => label,
                    None => format!("{label}: {rel}"),
                },
                online: root.is_some(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Marks a folder as leading or backup. A folder has one role; marking
    /// it again changes it.
    pub fn set_dup_folder(&self, path: &Path, role: Role) -> Result<()> {
        if role == Role::Other {
            return Err(Error::Invalid("a folder is either leading or backup".into()));
        }
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
        conn.execute(
            "INSERT OR REPLACE INTO dup_folders (volume_id, rel_path, role) VALUES (?1, ?2, ?3)",
            params![volume_id, loc.rel, role.as_str()],
        )?;
        Ok(())
    }

    pub fn remove_dup_folder(&self, id: i64) -> Result<()> {
        self.writer().execute("DELETE FROM dup_folders WHERE rowid = ?1", [id])?;
        Ok(())
    }

    /// Every photo with copies, as last worked out by [`find`]. The copy to
    /// keep comes first: one in a leading folder, else one outside backup
    /// folders, preferring connected drives, then the biggest file (most
    /// metadata, such as tags), then the shortest path.
    pub fn duplicates(&self) -> Result<Vec<DupGroup>> {
        let roots = self.roots_snapshot();
        let conn = self.reader();
        let folders = role_folders(&conn)?;
        let mut stmt = conn.prepare(
            "WITH current AS (
                 SELECT d.file_id, d.picture FROM dup_hashes d JOIN files f ON f.id = d.file_id
                 WHERE d.content_hash = f.content_hash AND d.picture IS NOT NULL AND f.error IS NULL),
             shared AS (SELECT picture FROM current GROUP BY picture HAVING count(*) > 1)
             SELECT c.picture, f.id, f.kind, f.name, fo.rel_path, v.id, v.key, v.label, v.kind, f.size, f.mtime,
                    f.taken_at, f.width, f.height
             FROM current c JOIN shared USING (picture) JOIN files f ON f.id = c.file_id
             JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
             ORDER BY c.picture",
        )?;
        let mut rows = stmt.query([])?;
        let mut groups: Vec<DupGroup> = Vec::new();
        let mut last: Option<Vec<u8>> = None;
        while let Some(r) = rows.next()? {
            let picture: Vec<u8> = r.get(0)?;
            let rel: String = r.get(4)?;
            let name: String = r.get(3)?;
            let volume_id: i64 = r.get(5)?;
            let key: String = r.get(6)?;
            let label: String = r.get(7)?;
            let root = roots.get(&key);
            let (folder, path) = match root {
                Some(root) => {
                    let dir = volume::join(root, &rel);
                    let path = dir.join(&name).to_string_lossy().into_owned();
                    (dir.to_string_lossy().into_owned(), Some(path))
                }
                None => (format!("{label}: {rel}"), None),
            };
            let copy = DupCopy {
                id: r.get(1)?,
                name,
                folder,
                path,
                online: root.is_some(),
                role: role_of(&folders, volume_id, &rel),
                size: r.get(9)?,
                modified: iso(r.get(10)?),
                suggested: false,
                no_recycle_bin: !has_recycle_bin(VolumeKind::parse(&r.get::<_, String>(8)?)),
            };
            if last.as_ref() != Some(&picture) {
                groups.push(DupGroup {
                    key: picture.iter().map(|b| format!("{b:02x}")).collect(),
                    kind: r.get(2)?,
                    width: r.get(12)?,
                    height: r.get(13)?,
                    taken_at: r.get(11)?,
                    copies: Vec::new(),
                    expected: false,
                });
                last = Some(picture);
            }
            groups.last_mut().expect("pushed above").copies.push(copy);
        }
        for g in &mut groups {
            g.copies.sort_by(|a, b| {
                (a.role.rank(), !a.online, std::cmp::Reverse(a.size), a.folder.len() + a.name.len(), &a.folder, &a.name)
                    .cmp(&(
                        b.role.rank(),
                        !b.online,
                        std::cmp::Reverse(b.size),
                        b.folder.len() + b.name.len(),
                        &b.folder,
                        &b.name,
                    ))
            });
            for c in g.copies.iter_mut().skip(1) {
                c.suggested = c.role != Role::Backup && c.online;
            }
            g.expected = g.copies.iter().skip(1).all(|c| c.role == Role::Backup);
        }
        groups.sort_by(|a, b| (&a.copies[0].folder, &a.copies[0].name).cmp(&(&b.copies[0].folder, &b.copies[0].name)));
        Ok(groups)
    }

    /// Deletes the given copies: to the Recycle Bin, or aside on drives that
    /// have none. A file is left alone if it changed since the last scan, or
    /// if no other copy of it would be left on a connected drive. Names on
    /// faces in a deleted copy move to the same faces in a copy that stays.
    pub fn delete_duplicates(&self, ids: &[i64]) -> Result<DeleteSummary> {
        let doomed: HashSet<i64> = ids.iter().copied().collect();
        let roots = self.roots_snapshot();
        let sources = self.sources()?;

        struct Copy {
            id: i64,
            picture: Vec<u8>,
            path: Option<PathBuf>,
            size: i64,
            mtime: i64,
            volume_key: String,
            volume_kind: VolumeKind,
            rel: String,
        }
        let mut by_picture: HashMap<Vec<u8>, Vec<Copy>> = HashMap::new();
        {
            let conn = self.reader();
            let mut stmt = conn.prepare(
                "SELECT f.id, d.picture, fo.rel_path, f.name, v.key, v.kind, f.size, f.mtime
                 FROM dup_hashes d JOIN files f ON f.id = d.file_id AND f.content_hash = d.content_hash
                 JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
                 WHERE d.picture IN (SELECT d2.picture FROM dup_hashes d2 WHERE d2.file_id = ?1)",
            )?;
            let mut seen = HashSet::new();
            for &id in ids {
                if seen.contains(&id) {
                    continue;
                }
                let rows = stmt.query_map([id], |r| {
                    let rel: String = r.get(2)?;
                    let name: String = r.get(3)?;
                    let key: String = r.get(4)?;
                    Ok(Copy {
                        id: r.get(0)?,
                        picture: r.get(1)?,
                        path: roots.get(&key).map(|root| volume::join(root, &rel).join(&name)),
                        size: r.get(6)?,
                        mtime: r.get(7)?,
                        volume_key: key,
                        volume_kind: VolumeKind::parse(&r.get::<_, String>(5)?),
                        rel,
                    })
                })?;
                for c in rows {
                    let c = c?;
                    if seen.insert(c.id) {
                        by_picture.entry(c.picture.clone()).or_default().push(c);
                    }
                }
            }
        }

        let mut summary = DeleteSummary::default();
        let mut deleted: Vec<(i64, i64)> = Vec::new();
        let label = |c: &Copy| c.path.as_ref().map_or_else(|| c.rel.clone(), |p| p.to_string_lossy().into_owned());
        for copies in by_picture.values() {
            let unchanged = |c: &Copy| {
                c.path.as_ref().and_then(|p| p.metadata().ok()).is_some_and(|m| {
                    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
                    m.len() as i64 == c.size && mtime.map_or(0, |d| d.as_secs() as i64) == c.mtime
                })
            };
            let keeper = copies.iter().find(|c| !doomed.contains(&c.id) && unchanged(c));
            for c in copies.iter().filter(|c| doomed.contains(&c.id)) {
                let Some(keeper) = keeper else {
                    summary.failed.push((label(c), "no other copy of this photo is on a connected drive".into()));
                    continue;
                };
                let Some(path) = c.path.as_ref() else {
                    summary.failed.push((label(c), "its drive is not connected".into()));
                    continue;
                };
                if !unchanged(c) {
                    summary.failed.push((label(c), "it changed or moved since the last scan".into()));
                    continue;
                }
                let result = if has_recycle_bin(c.volume_kind) {
                    to_recycle_bin(path).map(|_| None)
                } else {
                    // The aside folder goes in the photo folder the file is
                    // in, so it stays on the same drive and is a quick move.
                    let source = sources
                        .iter()
                        .filter(|s| s.volume_key == c.volume_key && is_within(&c.rel, &s.rel_path))
                        .max_by_key(|s| s.rel_path.len());
                    match (source, roots.get(&c.volume_key)) {
                        (Some(s), Some(root)) => move_aside(path, &volume::join(root, &s.rel_path), &c.rel).map(Some),
                        _ => Err(std::io::Error::other("it is not in a library folder")),
                    }
                };
                match result {
                    Ok(aside) => {
                        if let Some(dir) = aside {
                            summary.moved_aside += 1;
                            let dir = dir.to_string_lossy().into_owned();
                            if !summary.aside_folders.contains(&dir) {
                                summary.aside_folders.push(dir);
                            }
                        } else {
                            summary.recycled += 1;
                        }
                        summary.bytes += c.size as u64;
                        deleted.push((c.id, keeper.id));
                    }
                    Err(e) => summary.failed.push((label(c), e.to_string())),
                }
            }
        }

        let mut conn = self.writer();
        let tx = conn.transaction()?;
        for &(id, keeper) in &deleted {
            summary.names_kept += carry_names(&tx, id, keeper)?;
            tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
        }
        tx.execute("DELETE FROM folders WHERE id NOT IN (SELECT folder_id FROM files)", [])?;
        tx.execute("DELETE FROM thumbs.thumbs WHERE content_hash NOT IN (SELECT content_hash FROM files)", [])?;
        tx.commit()?;
        Ok(summary)
    }
}

/// Copies names given to faces in `from` onto the matching faces in `to`,
/// where those have none. Both are the same picture, so the same face sits
/// in the same place.
fn carry_names(tx: &Connection, from: i64, to: i64) -> Result<u64> {
    /// Id, box, person and who named them.
    type Face = (i64, [f64; 4], Option<i64>, Option<String>);
    let faces = |file: i64, named: bool| -> Result<Vec<Face>> {
        let mut stmt = tx.prepare_cached(
            "SELECT id, x, y, w, h, person_id, person_source FROM faces WHERE file_id = ?1
             AND (?2 = (person_id IS NOT NULL AND person_source != 'auto'))",
        )?;
        let rows = stmt.query_map(params![file, named], |r| {
            Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?], r.get(5)?, r.get(6)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let mut open = faces(to, false)?;
    let mut n = 0;
    for (_, b, person, source) in faces(from, true)? {
        let best = open
            .iter()
            .enumerate()
            .map(|(i, f)| (i, overlap(&b, &f.1)))
            .filter(|&(_, o)| o > 0.5)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            let face = open.remove(i).0;
            tx.execute(
                "UPDATE faces SET person_id = ?1, person_source = ?2 WHERE id = ?3",
                params![person, source, face],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

/// Intersection over union of two `[x, y, w, h]` boxes.
fn overlap(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

fn is_within(rel: &str, dir: &str) -> bool {
    let (rel, dir) = (rel.to_lowercase(), dir.to_lowercase());
    dir.is_empty() || rel == dir || rel.starts_with(&dir) && rel[dir.len()..].starts_with('/')
}

/// Network shares and USB sticks have no Recycle Bin: Windows deletes from
/// them outright.
fn has_recycle_bin(kind: VolumeKind) -> bool {
    cfg!(windows) && kind == VolumeKind::Fixed
}

/// Moves `file` into the aside folder under `source_root`, keeping its
/// place below the source. Returns the aside folder.
fn move_aside(file: &Path, source_root: &Path, rel_dir: &str) -> std::io::Result<PathBuf> {
    let aside = source_root.join(ASIDE_FOLDER);
    let below = file.parent().and_then(|p| p.strip_prefix(source_root).ok()).unwrap_or(Path::new(""));
    let dir = aside.join(below);
    std::fs::create_dir_all(&dir)?;
    let name = file.file_name().ok_or_else(|| std::io::Error::other(format!("{rel_dir} has no file name")))?;
    let mut target = dir.join(name);
    let stem = file.file_stem().unwrap_or(name).to_string_lossy().into_owned();
    let ext = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 2;
    while target.exists() {
        target = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    std::fs::rename(file, &target)?;
    Ok(aside)
}

#[cfg(windows)]
fn to_recycle_bin(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
    };
    // A list of paths, each ending in a nul, and one more nul at the end.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `op` points at `from`, which outlives the call and is
    // double-nul terminated as the API requires.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted != 0 {
        return Err(std::io::Error::other(format!("Windows could not move it to the Recycle Bin (code {code:#x})")));
    }
    if path.exists() {
        return Err(std::io::Error::other("Windows did not move it to the Recycle Bin"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn to_recycle_bin(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("there is no Recycle Bin here"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(seed: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * seed, y as u8, seed]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
        out
    }

    fn with_segment(jpeg: &[u8], marker: u8, body: &[u8]) -> Vec<u8> {
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, marker]);
        out.extend(((body.len() + 2) as u16).to_be_bytes());
        out.extend(body);
        out.extend(&jpeg[2..]);
        out
    }

    fn picture(data: &[u8]) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        assert!(jpeg_picture(data, &mut h));
        h.finalize()
    }

    #[test]
    fn metadata_does_not_count() {
        let a = jpeg(3);
        let tagged = with_segment(&a, 0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>");
        let commented = with_segment(&tagged, 0xFE, b"hello");
        assert_eq!(picture(&a), picture(&tagged));
        assert_eq!(picture(&a), picture(&commented));
        assert_ne!(picture(&a), picture(&jpeg(4)));
        // Adobe's segment changes how colours decode, so it counts.
        assert_ne!(picture(&a), picture(&with_segment(&a, 0xEE, b"Adobe\0\x64\0\0\0\0\x01")));
        assert!(!jpeg_picture(b"not a jpeg", &mut blake3::Hasher::new()));
        assert!(!jpeg_picture(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF], &mut blake3::Hasher::new()));
    }

    #[test]
    fn roles_follow_the_closest_folder() {
        let folders = vec![
            RoleFolder { volume_id: 1, rel: "photos".into(), role: Role::Leading },
            RoleFolder { volume_id: 1, rel: "photos/backup".into(), role: Role::Backup },
        ];
        assert_eq!(role_of(&folders, 1, "Photos/2015"), Role::Leading);
        assert_eq!(role_of(&folders, 1, "photos/Backup/2015"), Role::Backup);
        assert_eq!(role_of(&folders, 1, "photosx"), Role::Other);
        assert_eq!(role_of(&folders, 2, "photos"), Role::Other);
    }

    #[test]
    fn names_move_to_the_kept_copy() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(dir.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO volumes VALUES (1, 'k', 'k', 'fixed');
             INSERT INTO folders VALUES (1, 1, '');
             INSERT INTO files (id, folder_id, name, stem, kind, size, mtime, content_hash, sort_date)
                 VALUES (1, 1, 'a.jpg', 'a', 'jpeg', 1, 0, x'00', ''), (2, 1, 'b.jpg', 'b', 'jpeg', 1, 0, x'01', '');
             INSERT INTO people (id, name) VALUES (1, 'Anna'), (2, 'Ben');
             INSERT INTO faces (id, file_id, x, y, w, h, score, embedding, person_id, person_source) VALUES
                 (1, 1, 0.1, 0.1, 0.2, 0.2, 1, x'', 1, 'user'),
                 (2, 1, 0.6, 0.1, 0.2, 0.2, 1, x'', 2, 'auto'),
                 (3, 2, 0.11, 0.1, 0.2, 0.2, 1, x'', NULL, NULL),
                 (4, 2, 0.6, 0.1, 0.2, 0.2, 1, x'', NULL, NULL);",
        )
        .unwrap();
        assert_eq!(carry_names(&conn, 1, 2).unwrap(), 1);
        let named: Vec<(i64, Option<i64>)> = conn
            .prepare("SELECT id, person_id FROM faces WHERE file_id = 2 ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(named, [(3, Some(1)), (4, None)], "only names, not suggestions, move");
    }

    #[test]
    fn boxes_overlap() {
        assert!((overlap(&[0.1, 0.1, 0.2, 0.2], &[0.1, 0.1, 0.2, 0.2]) - 1.0).abs() < 1e-9);
        assert_eq!(overlap(&[0.0, 0.0, 0.1, 0.1], &[0.5, 0.5, 0.1, 0.1]), 0.0);
    }
}
