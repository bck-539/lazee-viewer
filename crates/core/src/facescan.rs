//! The face scan: looks for faces in every photo that hasn't been looked at
//! with the current models, and sorts them into groups of look-alikes.
//!
//! It runs after the file scan, in the background, newest photos first, and
//! can be paused or stopped like any scan. Photos are decoded on other
//! cores while the models run, and each photo is read once.

use crate::faces::embed::{normalize, similarity};
use crate::faces::{Engine, Found};
use crate::library::Library;
use crate::scanner::ScanControl;
use crate::{raw, thumbs, volume, Result};
use image::RgbImage;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;

/// Faces smaller than this many pixels across, in the original photo, are
/// ignored: too small to recognise reliably.
pub const MIN_FACE_PX: f32 = 40.0;
/// Photos are decoded with at least this many pixels on the long side.
const DECODE_EDGE: u32 = 1600;
/// Size of the face thumbnails shown on the People page.
const CHIP_PX: u32 = 128;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FaceProgress {
    pub done: u64,
    pub total: u64,
    pub faces: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FaceSummary {
    pub photos: u64,
    pub faces: u64,
    pub failed: u64,
    pub cancelled: bool,
}

struct Todo {
    file_id: i64,
    path: PathBuf,
    kind: String,
    orientation: u8,
    hash: Vec<u8>,
}

struct Decoded {
    todo: Todo,
    image: Result<(RgbImage, f32)>,
}

/// One photo's faces, ready to write: boxes as fractions of the photo,
/// with their embeddings and thumbnails.
struct Analyzed {
    todo: Todo,
    faces: Result<Vec<(Found, Vec<u8>)>>,
}

/// Photos written per transaction. The library is locked only while a
/// batch is written, so browsing and file scans aren't held up.
const BATCH: usize = 20;

pub fn scan(
    lib: &Library,
    engine: &mut Engine,
    ctl: &ScanControl,
    on_progress: &(dyn Fn(&FaceProgress) + Sync),
) -> Result<FaceSummary> {
    let key = engine.key();
    let (todo, mut clusters) = {
        let conn = lib.writer();
        forget_other_models(&conn, &key)?;
        (load_todo(&conn, &key, &lib.roots_snapshot())?, Clusters::load(&conn, engine.embedder.cluster_threshold())?)
    };
    let total = todo.len() as u64;
    let mut summary = FaceSummary::default();
    on_progress(&FaceProgress { done: 0, total, faces: 0 });

    let (tx, rx) = crossbeam_channel::bounded::<Decoded>(4);
    let decoders = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(2).clamp(1, 3);
    let queue = std::sync::Mutex::new(todo.into_iter());
    std::thread::scope(|s| -> Result<()> {
        for _ in 0..decoders {
            let tx = tx.clone();
            let queue = &queue;
            s.spawn(move || loop {
                if !ctl.checkpoint() {
                    return;
                }
                let Some(t) = queue.lock().unwrap_or_else(|e| e.into_inner()).next() else { return };
                let image = decode(&t);
                if tx.send(Decoded { todo: t, image }).is_err() {
                    return;
                }
            });
        }
        drop(tx);

        let mut batch = Vec::with_capacity(BATCH);
        for d in rx.iter() {
            if !ctl.checkpoint() {
                break;
            }
            let faces = d.image.and_then(|(img, scale)| analyze(engine, &img, scale));
            match &faces {
                Ok(f) => summary.faces += f.len() as u64,
                Err(e) => {
                    log::warn!("faces in {}: {e}", d.todo.path.display());
                    summary.failed += 1;
                }
            }
            summary.photos += 1;
            batch.push(Analyzed { todo: d.todo, faces });
            if batch.len() >= BATCH {
                write(lib, &key, &mut batch, &mut clusters)?;
                on_progress(&FaceProgress { done: summary.photos, total, faces: summary.faces });
            }
        }
        // Stops the decoders if the loop ended early.
        drop(rx);
        write(lib, &key, &mut batch, &mut clusters)
    })?;

    tidy(&lib.writer())?;
    on_progress(&FaceProgress { done: summary.photos, total, faces: summary.faces });
    summary.cancelled = ctl.is_cancelled();
    Ok(summary)
}

fn analyze(engine: &mut Engine, img: &RgbImage, scale: f32) -> Result<Vec<(Found, Vec<u8>)>> {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let mut out = Vec::new();
    for mut f in engine.analyze(img, MIN_FACE_PX * scale)? {
        let chip = chip(img, &f.face)?;
        let b = &mut f.face;
        b.w = (b.w / iw).min(1.0);
        b.h = (b.h / ih).min(1.0);
        b.x = (b.x / iw).clamp(0.0, 1.0);
        b.y = (b.y / ih).clamp(0.0, 1.0);
        out.push((f, chip));
    }
    Ok(out)
}

fn write(lib: &Library, key: &str, batch: &mut Vec<Analyzed>, clusters: &mut Clusters) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut conn = lib.writer();
    let txn = conn.transaction()?;
    for a in batch.drain(..) {
        // The file may have gone while it was being looked at.
        let exists: bool =
            txn.query_row("SELECT EXISTS (SELECT 1 FROM files WHERE id = ?1)", [a.todo.file_id], |r| r.get(0))?;
        if !exists {
            continue;
        }
        match a.faces {
            Ok(found) => store(&txn, &a.todo, key, &found, clusters)?,
            Err(e) => store_error(&txn, &a.todo, key, &e.to_string())?,
        }
    }
    clusters.save(&txn)?;
    txn.commit()?;
    Ok(())
}

/// Embeddings from different models can't be compared, so switching
/// models starts the face data over.
fn forget_other_models(conn: &Connection, key: &str) -> Result<()> {
    let current: Option<String> =
        conn.query_row("SELECT value FROM settings WHERE key = 'face_models'", [], |r| r.get(0)).optional()?;
    if current.as_deref() == Some(key) {
        return Ok(());
    }
    if current.is_some() {
        log::info!("face models changed from {current:?} to {key}; starting face data over");
    }
    conn.execute_batch(
        "DELETE FROM faces; DELETE FROM clusters; DELETE FROM face_scans; DELETE FROM thumbs.face_chips;",
    )?;
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES ('face_models', ?1)", [key])?;
    Ok(())
}

fn load_todo(conn: &Connection, key: &str, roots: &HashMap<String, PathBuf>) -> Result<Vec<Todo>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, v.key, fo.rel_path, f.name, f.kind, f.orientation, f.content_hash
         FROM files f
         JOIN folders fo ON fo.id = f.folder_id
         JOIN volumes v ON v.id = fo.volume_id
         LEFT JOIN face_scans s ON s.file_id = f.id
         WHERE f.paired_with IS NULL AND f.error IS NULL
           AND (s.file_id IS NULL OR s.model != ?1 OR s.content_hash != f.content_hash)
         ORDER BY f.sort_date DESC",
    )?;
    let rows = stmt.query_map([key], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, u8>(5)?,
            r.get::<_, Vec<u8>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file_id, vol, rel, name, kind, orientation, hash) = row?;
        // Photos on disconnected drives wait until the drive is back.
        if let Some(root) = roots.get(&vol) {
            out.push(Todo { file_id, path: volume::join(root, &rel).join(name), kind, orientation, hash });
        }
    }
    Ok(out)
}

/// Decodes a photo upright, at a size the detector can use. Returns the
/// image and its scale relative to the original.
fn decode(t: &Todo) -> Result<(RgbImage, f32)> {
    let jpeg = if t.kind == "raw" {
        let mut f = std::fs::File::open(&t.path)?;
        let len = f.metadata()?.len();
        raw::extract_preview(&mut f, len)?.ok_or_else(|| crate::Error::Decode("no embedded preview".into()))?
    } else {
        std::fs::read(&t.path)?
    };
    let (mut img, w, h) = thumbs::decode_long_edge(&jpeg, DECODE_EDGE)?;
    let scale = img.width().max(img.height()) as f32 / w.max(h).max(1) as f32;
    if let Some(o) = image::metadata::Orientation::from_exif(t.orientation) {
        img.apply_orientation(o);
    }
    Ok((img.to_rgb8(), scale))
}

fn store(tx: &Connection, t: &Todo, key: &str, found: &[(Found, Vec<u8>)], clusters: &mut Clusters) -> Result<()> {
    let old: Vec<(i64, Option<i64>)> = tx
        .prepare_cached("SELECT id, cluster_id FROM faces WHERE file_id = ?1")?
        .query_map([t.file_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (id, cluster) in old {
        tx.execute("DELETE FROM thumbs.face_chips WHERE face_id = ?1", [id])?;
        if let Some(c) = cluster {
            clusters.shrink(c);
        }
    }
    tx.execute("DELETE FROM faces WHERE file_id = ?1", [t.file_id])?;

    for (f, chip) in found {
        let b = &f.face;
        let cluster = clusters.assign(tx, &f.embedding)?;
        tx.execute(
            "INSERT INTO faces (file_id, x, y, w, h, score, embedding, cluster_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![t.file_id, b.x, b.y, b.w, b.h, b.score, to_blob(&f.embedding), cluster],
        )?;
        let face_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO thumbs.face_chips (face_id, jpeg) VALUES (?1, ?2)", params![face_id, chip])?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO face_scans (file_id, model, content_hash, faces, error) VALUES (?1, ?2, ?3, ?4, NULL)",
        params![t.file_id, key, t.hash, found.len() as i64],
    )?;
    Ok(())
}

fn store_error(tx: &Connection, t: &Todo, key: &str, error: &str) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO face_scans (file_id, model, content_hash, faces, error) VALUES (?1, ?2, ?3, 0, ?4)",
        params![t.file_id, key, t.hash, error],
    )?;
    Ok(())
}

/// A square face thumbnail with a little room around the face.
fn chip(img: &RgbImage, b: &crate::faces::Face) -> Result<Vec<u8>> {
    let side = (b.w.max(b.h) * 1.4).max(8.0);
    let cx = b.x + b.w / 2.0;
    let cy = b.y + b.h / 2.0;
    let x0 = (cx - side / 2.0).clamp(0.0, (img.width() as f32 - 1.0).max(0.0)) as u32;
    let y0 = (cy - side / 2.0).clamp(0.0, (img.height() as f32 - 1.0).max(0.0)) as u32;
    let w = (side as u32).min(img.width() - x0).max(1);
    let h = (side as u32).min(img.height() - y0).max(1);
    let crop = image::imageops::crop_imm(img, x0, y0, w, h).to_image();
    let small = image::imageops::resize(&crop, CHIP_PX, CHIP_PX, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&small)
        .map_err(|e| crate::Error::Decode(e.to_string()))?;
    Ok(out)
}

/// Drops face data for photos that are gone, and empty groups.
fn tidy(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM thumbs.face_chips WHERE face_id NOT IN (SELECT id FROM faces);
         UPDATE clusters SET size = (SELECT count(*) FROM faces WHERE cluster_id = clusters.id);
         DELETE FROM clusters WHERE size = 0;",
    )?;
    Ok(())
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Groups of look-alike faces, kept in memory during a scan. A face joins
/// the group whose average face it is most like, if close enough; otherwise
/// it starts a group of its own.
struct Clusters {
    threshold: f32,
    items: Vec<Cluster>,
}

struct Cluster {
    id: i64,
    /// Running sum of member embeddings; its direction is the average face.
    sum: Vec<f32>,
    size: i64,
    dirty: bool,
}

impl Clusters {
    fn load(conn: &Connection, threshold: f32) -> Result<Self> {
        let mut stmt = conn.prepare("SELECT id, centroid, size FROM clusters WHERE size > 0")?;
        let items = stmt
            .query_map([], |r| {
                let centroid = from_blob(&r.get::<_, Vec<u8>>(1)?);
                let size: i64 = r.get(2)?;
                Ok(Cluster {
                    id: r.get(0)?,
                    sum: centroid.iter().map(|x| x * size as f32).collect(),
                    size,
                    dirty: false,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(Clusters { threshold, items })
    }

    fn assign(&mut self, tx: &Connection, e: &[f32]) -> Result<i64> {
        let best = self
            .items
            .iter_mut()
            .filter(|c| c.size > 0)
            .map(|c| {
                let sim = similarity(&normalize(&c.sum), e);
                (sim, c)
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((sim, c)) = best {
            if sim >= self.threshold {
                for (s, x) in c.sum.iter_mut().zip(e) {
                    *s += x;
                }
                c.size += 1;
                c.dirty = true;
                return Ok(c.id);
            }
        }
        tx.execute("INSERT INTO clusters (centroid, size) VALUES (?1, 1)", [to_blob(e)])?;
        let id = tx.last_insert_rowid();
        self.items.push(Cluster { id, sum: e.to_vec(), size: 1, dirty: false });
        Ok(id)
    }

    /// A face left a group; its exact embedding is gone, so only the count
    /// changes. The average drifts a little until the next full regroup.
    fn shrink(&mut self, id: i64) {
        if let Some(c) = self.items.iter_mut().find(|c| c.id == id) {
            c.size = (c.size - 1).max(0);
            c.dirty = true;
        }
    }

    fn save(&mut self, tx: &Connection) -> Result<()> {
        for c in self.items.iter_mut().filter(|c| c.dirty) {
            tx.execute(
                "UPDATE clusters SET centroid = ?1, size = ?2 WHERE id = ?3",
                params![to_blob(&normalize(&c.sum)), c.size, c.id],
            )?;
            c.dirty = false;
        }
        Ok(())
    }
}
