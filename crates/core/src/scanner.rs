//! Indexing source folders: find photos, read their metadata, make
//! thumbnails, and notice files that were added, changed, moved or deleted.
//!
//! A scan walks each online source, compares what it finds with the index,
//! and only opens files that are new or changed. Work runs on a thread pool
//! that leaves one core free, and a single writer thread commits results in
//! batches. Files are opened once, and only their head and tail are read
//! when a thumbnail for the same content already exists.

use crate::formats::{self, FileKind};
use crate::hash::{self, ContentHash};
use crate::library::Library;
use crate::meta::{self, Meta};
use crate::thumbs::{self, Thumbnail};
use crate::volume;
use crate::Result;
use rayon::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

/// Lets the UI pause or stop a running scan.
#[derive(Default)]
pub struct ScanControl {
    cancel: AtomicBool,
    paused: AtomicBool,
}

impl ScanControl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
    /// Blocks while paused. Returns false if the scan was cancelled.
    pub(crate) fn checkpoint(&self) -> bool {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(Duration::from_millis(200));
        }
        !self.is_cancelled()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanPhase {
    Walking,
    Processing,
    Finishing,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub source: String,
    /// Photos found so far in this source.
    pub found: u64,
    /// New or changed photos that need reading.
    pub to_process: u64,
    pub processed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanSummary {
    pub added: u64,
    pub updated: u64,
    pub moved: u64,
    pub removed: u64,
    pub unchanged: u64,
    pub failed: u64,
    /// Sources skipped because their drive is not connected.
    pub offline: Vec<String>,
    pub cancelled: bool,
}

/// A file as the index knows it.
struct Known {
    id: i64,
    size: i64,
    mtime: i64,
    hash: ContentHash,
    /// Has a thumbnail, or failed to read last time and hasn't changed since.
    done: bool,
}

/// A file found on disk that needs reading.
struct Todo {
    path: PathBuf,
    rel_dir: String,
    name: String,
    kind: FileKind,
    size: u64,
    mtime: i64,
    known_id: Option<i64>,
}

/// The result of reading one file.
struct Processed {
    todo: Todo,
    hash: ContentHash,
    meta: Meta,
    thumb: Option<Thumbnail>,
    error: Option<String>,
}

/// Scans the given sources, or all of them when `only` is `None`.
pub fn scan(
    lib: &Library,
    only: Option<&[i64]>,
    ctl: &ScanControl,
    on_progress: &(dyn Fn(&ScanProgress) + Sync),
) -> Result<ScanSummary> {
    let mut summary = ScanSummary::default();
    let roots = lib.refresh_volumes();
    let sources: Vec<_> = lib.sources()?.into_iter().filter(|s| only.is_none_or(|ids| ids.contains(&s.id))).collect();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("scan-{i}"))
        .build()
        .map_err(|e| crate::Error::Invalid(e.to_string()))?;

    for source in sources {
        if ctl.is_cancelled() {
            break;
        }
        let Some(root) = roots.get(&source.volume_key) else {
            summary.offline.push(source.display_path.clone());
            continue;
        };
        let dir = volume::join(root, &source.rel_path);
        let mut conn = lib.writer();
        scan_source(&mut conn, &pool, ctl, &source, root, &dir, &mut summary, on_progress)?;
    }

    let conn = lib.writer();
    on_progress(&ScanProgress {
        phase: ScanPhase::Finishing,
        source: String::new(),
        found: 0,
        to_process: 0,
        processed: 0,
    });
    pair_raw_with_jpeg(&conn)?;
    if summary.removed > 0 {
        conn.execute("DELETE FROM thumbs.thumbs WHERE content_hash NOT IN (SELECT content_hash FROM files)", [])?;
    }
    summary.cancelled = ctl.is_cancelled();
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn scan_source(
    conn: &mut Connection,
    pool: &rayon::ThreadPool,
    ctl: &ScanControl,
    source: &crate::Source,
    root: &Path,
    dir: &Path,
    summary: &mut ScanSummary,
    on_progress: &(dyn Fn(&ScanProgress) + Sync),
) -> Result<()> {
    let label = source.display_path.clone();
    let mut known = load_known(conn, source.volume_id, &source.rel_path)?;
    let thumbs_have: HashSet<ContentHash> = load_thumb_hashes(conn)?;

    // Walk the folder tree and sort files into unchanged and to-do.
    let mut todo = Vec::new();
    let mut failed_dirs: Vec<String> = Vec::new();
    let mut found = 0u64;
    let at_root = volume::is_root(dir);
    let walker = walkdir::WalkDir::new(dir).follow_links(false).into_iter().filter_entry(|e| {
        if e.depth() == 0 || !e.file_type().is_dir() {
            return true;
        }
        let name = e.file_name().to_string_lossy();
        let skip = if at_root && e.depth() == 1 { formats::skip_root_dir(&name) } else { formats::skip_dir(&name) };
        !skip && !hidden_system(e)
    });
    for entry in walker {
        if !ctl.checkpoint() {
            return Ok(());
        }
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                // An unreadable folder must not look like a folder whose
                // photos were all deleted.
                if let Some(p) = err.path().and_then(|p| p.strip_prefix(root).ok()) {
                    failed_dirs.push(volume::rel_to_string(p).to_lowercase());
                }
                if err.depth() == 0 {
                    return Err(std::io::Error::other(err.to_string()).into());
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(kind) = formats::classify(entry.path()) else { continue };
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel_dir = rel.parent().map(volume::rel_to_string).unwrap_or_default();
        let Ok(md) = entry.metadata() else {
            failed_dirs.push(rel_dir.to_lowercase());
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let size = md.len();
        let mtime =
            md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        found += 1;
        if found % 500 == 0 {
            on_progress(&ScanProgress {
                phase: ScanPhase::Walking,
                source: label.clone(),
                found,
                to_process: todo.len() as u64,
                processed: 0,
            });
        }

        let key = (rel_dir.to_lowercase(), name.to_lowercase());
        match known.remove(&key) {
            Some(k) if k.size == size as i64 && k.mtime == mtime && k.done => summary.unchanged += 1,
            Some(k) => {
                todo.push(Todo { path: entry.into_path(), rel_dir, name, kind, size, mtime, known_id: Some(k.id) })
            }
            None => todo.push(Todo { path: entry.into_path(), rel_dir, name, kind, size, mtime, known_id: None }),
        }
    }

    // Whatever is left in `known` was not found: deleted or moved away.
    // Files under folders we could not read are left alone.
    known.retain(|(rel_dir, _), _| !failed_dirs.iter().any(|f| rel_dir == f || rel_dir.starts_with(&format!("{f}/"))));
    let mut gone_by_hash: HashMap<ContentHash, Vec<i64>> = HashMap::new();
    for k in known.values() {
        gone_by_hash.entry(k.hash).or_default().push(k.id);
    }

    // Read new and changed files in parallel; write results here in batches.
    let total = todo.len() as u64;
    let processed = AtomicU64::new(0);
    on_progress(&ScanProgress {
        phase: ScanPhase::Processing,
        source: label.clone(),
        found,
        to_process: total,
        processed: 0,
    });
    let (tx, rx) = crossbeam_channel::bounded::<Processed>(64);
    let mut folder_ids: HashMap<String, i64> = HashMap::new();
    let mut moved_ids: HashSet<i64> = HashSet::new();

    std::thread::scope(|s| -> Result<()> {
        s.spawn(|| {
            pool.install(|| {
                todo.into_par_iter().for_each_with(tx, |tx, t| {
                    if !ctl.checkpoint() {
                        return;
                    }
                    let _ = tx.send(process(t, &thumbs_have));
                });
            });
        });

        let mut batch = Vec::with_capacity(200);
        let mut last_report = std::time::Instant::now();
        loop {
            let next = rx.recv_timeout(Duration::from_millis(250));
            let done = matches!(next, Err(crossbeam_channel::RecvTimeoutError::Disconnected));
            if let Ok(p) = next {
                batch.push(p);
            }
            if batch.len() >= 200 || (done || last_report.elapsed() > Duration::from_millis(500)) && !batch.is_empty() {
                let n = batch.len() as u64;
                let tx = conn.transaction()?;
                for p in batch.drain(..) {
                    write_file(&tx, source.volume_id, p, &mut folder_ids, &mut gone_by_hash, &mut moved_ids, summary)?;
                }
                tx.commit()?;
                let done_count = processed.fetch_add(n, Ordering::Relaxed) + n;
                on_progress(&ScanProgress {
                    phase: ScanPhase::Processing,
                    source: label.clone(),
                    found,
                    to_process: total,
                    processed: done_count,
                });
                last_report = std::time::Instant::now();
            }
            if done {
                break;
            }
        }
        Ok(())
    })?;

    if ctl.is_cancelled() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    for k in known.values() {
        if !moved_ids.contains(&k.id) {
            tx.execute("DELETE FROM files WHERE id = ?1", [k.id])?;
            summary.removed += 1;
        }
    }
    tx.execute(
        "DELETE FROM folders WHERE volume_id = ?1 AND id NOT IN (SELECT folder_id FROM files)",
        [source.volume_id],
    )?;
    tx.execute("UPDATE sources SET last_scan = strftime('%Y-%m-%dT%H:%M:%S', 'now') WHERE id = ?1", [source.id])?;
    tx.commit()?;
    Ok(())
}

/// Folders Windows marks both hidden and system, such as recycle bins and
/// restore points under any name. Hidden alone is left alone: people hide
/// folders of their own photos.
#[cfg(windows)]
fn hidden_system(e: &walkdir::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN_SYSTEM: u32 = 0x2 | 0x4;
    e.metadata().is_ok_and(|m| m.file_attributes() & HIDDEN_SYSTEM == HIDDEN_SYSTEM)
}

#[cfg(not(windows))]
fn hidden_system(_e: &walkdir::DirEntry) -> bool {
    false
}

fn load_known(conn: &Connection, volume_id: i64, rel: &str) -> Result<HashMap<(String, String), Known>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, fo.rel_path, f.name, f.size, f.mtime, f.content_hash,
                f.error IS NOT NULL OR EXISTS (SELECT 1 FROM thumbs.thumbs t WHERE t.content_hash = f.content_hash)
         FROM files f JOIN folders fo ON fo.id = f.folder_id
         WHERE fo.volume_id = ?1
           AND (?2 = '' OR fo.rel_path = ?2 OR substr(fo.rel_path, 1, length(?2) + 1) = ?2 || '/' COLLATE NOCASE)",
    )?;
    let rows = stmt.query_map(params![volume_id, rel], |r| {
        let dir: String = r.get(1)?;
        let name: String = r.get(2)?;
        let hash: Vec<u8> = r.get(5)?;
        Ok((
            (dir.to_lowercase(), name.to_lowercase()),
            Known {
                id: r.get(0)?,
                size: r.get(3)?,
                mtime: r.get(4)?,
                hash: hash.try_into().unwrap_or_default(),
                done: r.get(6)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn load_thumb_hashes(conn: &Connection) -> Result<HashSet<ContentHash>> {
    let mut stmt = conn.prepare("SELECT content_hash FROM thumbs.thumbs")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    Ok(rows.filter_map(|h| h.ok()?.try_into().ok()).collect())
}

/// Reads one file. Never fails: problems are recorded on the result so the
/// photo still shows up in the library.
fn process(todo: Todo, thumbs_have: &HashSet<ContentHash>) -> Processed {
    let mut out =
        Processed { todo, hash: [0; 16], meta: Meta { orientation: 1, ..Meta::default() }, thumb: None, error: None };
    if let Err(e) = read_file(&mut out, thumbs_have) {
        log::warn!("could not read {}: {e}", out.todo.path.display());
        out.error = Some(e.to_string());
    }
    out
}

fn read_file(out: &mut Processed, thumbs_have: &HashSet<ContentHash>) -> Result<()> {
    let mut file = File::open(&out.todo.path)?;
    let len = file.metadata()?.len();
    out.hash = hash::from_file(&mut file, len)?;
    let need_thumb = !thumbs_have.contains(&out.hash);

    match out.todo.kind {
        FileKind::Jpeg if need_thumb => {
            let mut data = Vec::with_capacity(len as usize);
            file.seek(SeekFrom::Start(0))?;
            file.read_to_end(&mut data)?;
            out.meta = meta::read_bytes(&data);
            out.thumb = Some(thumbs::from_jpeg(&data, out.meta.orientation)?);
        }
        FileKind::Jpeg => {
            file.seek(SeekFrom::Start(0))?;
            out.meta = meta::read(&mut BufReader::new(&mut file));
        }
        FileKind::Raw => {
            out.meta = raw_meta(&mut file, len)?;
            if need_thumb || out.meta.taken_at.is_none() {
                let preview = crate::raw::extract_preview(&mut file, len)?
                    .ok_or_else(|| crate::Error::Decode("no embedded preview found".into()))?;
                let meta = std::mem::take(&mut out.meta);
                out.meta = meta.or(meta::read_bytes(&preview));
                if need_thumb {
                    out.thumb = Some(thumbs::from_jpeg(&preview, out.meta.orientation)?);
                }
            }
        }
    }
    if let Some(t) = &out.thumb {
        // The preview's size is not the sensor size, so only use it as a
        // last resort.
        out.meta.width = out.meta.width.or(Some(t.source_width));
        out.meta.height = out.meta.height.or(Some(t.source_height));
    }
    Ok(())
}

fn raw_meta(file: &mut File, len: u64) -> Result<Meta> {
    let mut head = vec![0u8; len.min(16) as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)?;
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        let blocks = crate::raw::cr3_exif_blocks(file, len)?;
        return Ok(blocks.into_iter().map(meta::read_tiff).fold(Meta { orientation: 1, ..Meta::default() }, Meta::or));
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        // The EXIF of TIFF-based RAWs sits near the start of the file.
        let mut buf = vec![0u8; len.min(1024 * 1024) as usize];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut buf)?;
        return Ok(meta::read_tiff(buf));
    }
    Ok(Meta { orientation: 1, ..Meta::default() })
}

fn folder_id(tx: &Connection, volume_id: i64, rel_dir: &str, cache: &mut HashMap<String, i64>) -> Result<i64> {
    let key = rel_dir.to_lowercase();
    if let Some(&id) = cache.get(&key) {
        return Ok(id);
    }
    tx.execute("INSERT OR IGNORE INTO folders (volume_id, rel_path) VALUES (?1, ?2)", params![volume_id, rel_dir])?;
    let id = tx.query_row(
        "SELECT id FROM folders WHERE volume_id = ?1 AND rel_path = ?2",
        params![volume_id, rel_dir],
        |r| r.get(0),
    )?;
    cache.insert(key, id);
    Ok(id)
}

fn write_file(
    tx: &Connection,
    volume_id: i64,
    p: Processed,
    folders: &mut HashMap<String, i64>,
    gone_by_hash: &mut HashMap<ContentHash, Vec<i64>>,
    moved_ids: &mut HashSet<i64>,
    summary: &mut ScanSummary,
) -> Result<()> {
    let t = &p.todo;
    let folder = folder_id(tx, volume_id, &t.rel_dir, folders)?;
    let stem = Path::new(&t.name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let m = &p.meta;
    let sort_date = m.taken_at.clone().unwrap_or_else(|| unix_to_iso(t.mtime));
    let (lat, lon) = m.gps.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    let has_thumb = p.thumb.is_some() || p.error.is_none();

    if let Some(th) = &p.thumb {
        tx.execute(
            "INSERT OR REPLACE INTO thumbs.thumbs (content_hash, width, height, jpeg) VALUES (?1, ?2, ?3, ?4)",
            params![&p.hash[..], th.width, th.height, th.jpeg],
        )?;
    }

    // A new file whose content matches one that vanished elsewhere in this
    // source was moved or renamed: keep its row so everything attached to
    // it (faces and names, later) follows the file.
    let mut id = t.known_id;
    if id.is_none() && p.error.is_none() {
        if let Some(ids) = gone_by_hash.get_mut(&p.hash) {
            if let Some(old) = ids.pop() {
                moved_ids.insert(old);
                id = Some(old);
                summary.moved += 1;
            }
        }
    } else if id.is_some() {
        summary.updated += 1;
    }
    if p.error.is_some() {
        summary.failed += 1;
    }

    let values = params![
        folder,
        t.name,
        stem,
        t.kind.as_str(),
        t.size as i64,
        t.mtime,
        &p.hash[..],
        m.taken_at,
        sort_date,
        m.camera_make,
        m.camera_model,
        m.orientation,
        m.width,
        m.height,
        lat,
        lon,
        has_thumb,
        p.error,
    ];
    match id {
        Some(id) => {
            let n = tx.execute(
                "UPDATE files SET folder_id = ?1, name = ?2, stem = ?3, kind = ?4, size = ?5, mtime = ?6,
                    content_hash = ?7, taken_at = ?8, sort_date = ?9, camera_make = ?10, camera_model = ?11,
                    orientation = ?12, width = ?13, height = ?14, gps_lat = ?15, gps_lon = ?16,
                    has_thumb = ?17, error = ?18
                 WHERE id = ?19",
                rusqlite::params_from_iter(values.iter().copied().chain(std::iter::once(&id as &dyn rusqlite::ToSql))),
            )?;
            if n == 0 {
                return Err(crate::Error::Invalid(format!("file row {id} disappeared during the scan")));
            }
        }
        None => {
            // A file with the same name may exist if it vanished and came
            // back with new content; replace it.
            let existing: Option<i64> = tx
                .query_row("SELECT id FROM files WHERE folder_id = ?1 AND name = ?2", params![folder, t.name], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(old) = existing {
                tx.execute("DELETE FROM files WHERE id = ?1", [old])?;
            }
            tx.execute(
                "INSERT INTO files (folder_id, name, stem, kind, size, mtime, content_hash, taken_at, sort_date,
                    camera_make, camera_model, orientation, width, height, gps_lat, gps_lon, has_thumb, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                values,
            )?;
            summary.added += 1;
        }
    }
    Ok(())
}

fn pair_raw_with_jpeg(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE files SET paired_with = (
             SELECT j.id FROM files j
             WHERE j.folder_id = files.folder_id AND j.stem = files.stem AND j.kind = 'jpeg'
             LIMIT 1)
         WHERE kind = 'raw'",
        [],
    )?;
    Ok(())
}

/// Seconds since 1970 to `YYYY-MM-DDTHH:MM:SS` in UTC.
pub(crate) fn unix_to_iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days, after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(unix_to_iso(0), "1970-01-01T00:00:00");
        assert_eq!(unix_to_iso(1_436_034_600), "2015-07-04T18:30:00");
        assert_eq!(unix_to_iso(951_782_400), "2000-02-29T00:00:00");
    }
}
