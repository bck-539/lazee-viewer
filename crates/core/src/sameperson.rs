//! "Are these the same person?" Faces of one person often end up in a few
//! groups (a beard, glasses, years apart). This finds groups that look
//! alike, asks about them, and merges groups the user says are one person.

use crate::faces::embed::{normalize, similarity};
use crate::facescan::{clusters_version, from_blob, to_blob, CLUSTERS_VERSION};
use crate::library::Library;
use crate::naming::refresh;
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

/// How alike two groups must be to ask, when the models don't say.
const ASK_THRESHOLD: f32 = 0.36;

/// At most this many unnamed groups are compared, biggest first, so the
/// question stays quick on a huge library.
const MAX_GROUPS: usize = 2000;

/// Faces shown for each side of a question.
const SAMPLE: usize = 6;

/// An unnamed group, and a group or person it looks like.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Question {
    pub group: i64,
    pub faces: Vec<i64>,
    pub photos: i64,
    /// The other side is either another unnamed group...
    pub other_group: Option<i64>,
    /// ...or someone already named.
    pub person_id: Option<i64>,
    pub name: Option<String>,
    pub other_faces: Vec<i64>,
    pub other_photos: i64,
    pub similarity: f32,
}

/// A face, for picking faces out of a group or person.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FaceItem {
    pub id: i64,
    pub file_id: i64,
    /// The name on it is a suggestion, waiting for review.
    pub suggested: bool,
}

fn pair(a: i64, b: i64) -> (i64, i64) {
    (a.min(b), a.max(b))
}

/// The unnamed faces of a group, clearest first.
fn group_faces(conn: &Connection, group: i64, limit: usize) -> Result<(Vec<i64>, i64)> {
    let faces = conn
        .prepare_cached(
            "SELECT id FROM faces WHERE cluster_id = ?1 AND person_id IS NULL ORDER BY score * w DESC LIMIT ?2",
        )?
        .query_map(params![group, limit as i64], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let photos = conn.query_row(
        "SELECT count(DISTINCT file_id) FROM faces WHERE cluster_id = ?1 AND person_id IS NULL",
        [group],
        |r| r.get(0),
    )?;
    Ok((faces, photos))
}

/// Someone's named faces, the ones they named themselves first.
fn person_faces(conn: &Connection, person: i64, limit: usize) -> Result<(Vec<i64>, i64)> {
    let faces = conn
        .prepare_cached(
            "SELECT id FROM faces WHERE person_id = ?1 AND person_source != 'auto'
             ORDER BY person_source = 'user' DESC, score * w DESC LIMIT ?2",
        )?
        .query_map(params![person, limit as i64], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let photos = conn.query_row(
        "SELECT count(*) FROM (
            SELECT file_id FROM faces WHERE person_id = ?1 AND person_source != 'auto'
            UNION SELECT file_id FROM tags WHERE person_id = ?1)",
        [person],
        |r| r.get(0),
    )?;
    Ok((faces, photos))
}

/// Puts groups together into the biggest of them, and works out its
/// average face again.
pub(crate) fn merge(conn: &Connection, groups: &[i64]) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;
    let mut sized = Vec::new();
    for &g in groups {
        let n: Option<i64> =
            tx.query_row("SELECT count(*) FROM faces WHERE cluster_id = ?1", [g], |r| r.get(0)).optional()?;
        let exists: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM clusters WHERE id = ?1)", [g], |r| r.get(0))?;
        if exists {
            sized.push((n.unwrap_or(0), g));
        }
    }
    sized.sort_by_key(|&(n, g)| (std::cmp::Reverse(n), g));
    sized.dedup_by_key(|x| x.1);
    let Some(&(_, keep)) = sized.first() else { return Err(Error::Invalid("those groups are gone".into())) };
    for &(_, g) in &sized[1..] {
        tx.execute("UPDATE faces SET cluster_id = ?1 WHERE cluster_id = ?2", [keep, g])?;
        // "Not the same person" carries over to the merged group.
        tx.execute(
            "INSERT OR IGNORE INTO cluster_rejects (a, b)
             SELECT min(?1, x), max(?1, x) FROM
                (SELECT CASE WHEN a = ?2 THEN b ELSE a END AS x FROM cluster_rejects WHERE a = ?2 OR b = ?2)
             WHERE x != ?1",
            [keep, g],
        )?;
        tx.execute("DELETE FROM clusters WHERE id = ?1", [g])?;
    }
    let mut sum: Vec<f32> = Vec::new();
    let mut size = 0i64;
    {
        let mut stmt = tx.prepare("SELECT embedding FROM faces WHERE cluster_id = ?1")?;
        for blob in stmt.query_map([keep], |r| r.get::<_, Vec<u8>>(0))? {
            let e = from_blob(&blob?);
            if sum.is_empty() {
                sum = vec![0.0; e.len()];
            }
            if e.len() == sum.len() {
                for (s, x) in sum.iter_mut().zip(&e) {
                    *s += x;
                }
                size += 1;
            }
        }
    }
    if size > 0 {
        tx.execute(
            "UPDATE clusters SET centroid = ?1, size = ?2 WHERE id = ?3",
            params![to_blob(&normalize(&sum)), size, keep],
        )?;
    }
    let version = clusters_version(&tx)? + 1;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![CLUSTERS_VERSION, version.to_string()],
    )?;
    tx.commit()?;
    Ok(keep)
}

impl Library {
    /// Pairs of groups that may be one person, likeliest first. Each
    /// unnamed group with at least `min_faces` faces is asked about once,
    /// with whatever it looks most like: another unnamed group or someone
    /// already named.
    pub fn same_person_questions(&self, min_faces: i64, limit: usize) -> Result<Vec<Question>> {
        let conn = self.reader();
        let setting = |key: &str| -> Result<Option<String>> {
            Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
        };
        if setting("face_models")?.is_none() {
            return Ok(Vec::new());
        }
        let threshold = setting("face_match_threshold")?.and_then(|v| v.parse().ok()).unwrap_or(ASK_THRESHOLD);

        let unnamed: Vec<(i64, Vec<f32>)> = conn
            .prepare(
                "SELECT c.id, c.centroid FROM clusters c
                 JOIN faces f ON f.cluster_id = c.id AND f.person_id IS NULL
                 WHERE c.hidden = 0 AND c.person_id IS NULL
                 GROUP BY c.id HAVING count(*) >= ?1
                 ORDER BY count(*) DESC, c.id LIMIT ?2",
            )?
            .query_map(params![min_faces, MAX_GROUPS as i64], |r| Ok((r.get(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?))))?
            .collect::<std::result::Result<_, _>>()?;
        let named: Vec<(i64, Vec<f32>)> = conn
            .prepare("SELECT person_id, centroid FROM clusters WHERE hidden = 0 AND person_id IS NOT NULL")?
            .query_map([], |r| Ok((r.get(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?))))?
            .collect::<std::result::Result<_, _>>()?;
        let group_rejects: HashSet<(i64, i64)> = conn
            .prepare("SELECT a, b FROM cluster_rejects")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let person_rejects: HashSet<(i64, i64)> = conn
            .prepare(
                "SELECT DISTINCT f.cluster_id, r.person_id FROM face_rejects r JOIN faces f ON f.id = r.face_id
                 WHERE f.cluster_id IS NOT NULL",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;

        enum Other {
            Group(i64),
            Person(i64),
        }
        let mut best: Vec<(f32, i64, Other)> = Vec::new();
        let mut asked: HashSet<(i64, i64)> = HashSet::new();
        for (i, (g, c)) in unnamed.iter().enumerate() {
            let mut top: Option<(f32, Other)> = None;
            for (j, (h, d)) in unnamed.iter().enumerate() {
                if i == j || d.len() != c.len() || group_rejects.contains(&pair(*g, *h)) {
                    continue;
                }
                let s = similarity(c, d);
                if s >= threshold && top.as_ref().is_none_or(|t| s > t.0) {
                    top = Some((s, Other::Group(*h)));
                }
            }
            for (p, d) in &named {
                if d.len() != c.len() || person_rejects.contains(&(*g, *p)) {
                    continue;
                }
                let s = similarity(c, d);
                if s >= threshold && top.as_ref().is_none_or(|t| s > t.0) {
                    top = Some((s, Other::Person(*p)));
                }
            }
            if let Some((s, other)) = top {
                // Two groups that are each other's closest are one question.
                if let Other::Group(h) = other {
                    if !asked.insert(pair(*g, h)) {
                        continue;
                    }
                }
                best.push((s, *g, other));
            }
        }
        best.sort_by(|a, b| b.0.total_cmp(&a.0));
        best.truncate(limit);

        let mut out = Vec::with_capacity(best.len());
        for (s, g, other) in best {
            let (faces, photos) = group_faces(&conn, g, SAMPLE)?;
            let mut q = Question {
                group: g,
                faces,
                photos,
                other_group: None,
                person_id: None,
                name: None,
                other_faces: Vec::new(),
                other_photos: 0,
                similarity: s,
            };
            match other {
                Other::Group(h) => {
                    q.other_group = Some(h);
                    (q.other_faces, q.other_photos) = group_faces(&conn, h, SAMPLE)?;
                }
                Other::Person(p) => {
                    q.person_id = Some(p);
                    q.name = Some(conn.query_row("SELECT name FROM people WHERE id = ?1", [p], |r| r.get(0))?);
                    (q.other_faces, q.other_photos) = person_faces(&conn, p, SAMPLE)?;
                }
            }
            out.push(q);
        }
        Ok(out)
    }

    /// Puts unnamed groups together as one person. Returns the group they
    /// are now.
    pub fn merge_groups(&self, groups: &[i64]) -> Result<i64> {
        let conn = self.writer();
        let keep = merge(&conn, groups)?;
        refresh(&conn)?;
        Ok(keep)
    }

    /// "Yes, same person": merges the two groups, or names the group as the
    /// person. Returns the person, if there is one.
    pub fn same_person(&self, group: i64, other_group: Option<i64>, person: Option<i64>) -> Result<Option<i64>> {
        match (other_group, person) {
            (Some(other), _) => {
                self.merge_groups(&[group, other])?;
                Ok(None)
            }
            (None, Some(p)) => {
                let name: String =
                    self.reader().query_row("SELECT name FROM people WHERE id = ?1", [p], |r| r.get(0))?;
                self.name_group(group, &name).map(Some)
            }
            (None, None) => Err(Error::Invalid("same person as whom?".into())),
        }
    }

    /// "No, not the same person": never asks about the two again.
    pub fn not_same_person(&self, group: i64, other_group: Option<i64>, person: Option<i64>) -> Result<()> {
        let conn = self.writer();
        if let Some(other) = other_group {
            let (a, b) = pair(group, other);
            conn.execute("INSERT OR IGNORE INTO cluster_rejects (a, b) VALUES (?1, ?2)", [a, b])?;
        }
        if let Some(p) = person {
            conn.execute(
                "INSERT OR IGNORE INTO face_rejects (face_id, person_id)
                 SELECT id, ?2 FROM faces WHERE cluster_id = ?1 AND person_id IS NULL",
                [group, p],
            )?;
            refresh(&conn)?;
        }
        Ok(())
    }

    /// Every face in an unnamed group, or named (or suggested) as someone,
    /// clearest first.
    pub fn faces_of(&self, group: Option<i64>, person: Option<i64>) -> Result<Vec<FaceItem>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT id, file_id, coalesce(person_source = 'auto', 0) FROM faces
             WHERE (?1 IS NOT NULL AND cluster_id = ?1 AND person_id IS NULL)
                OR (?2 IS NOT NULL AND person_id = ?2)
             ORDER BY person_source = 'auto', score * w DESC",
        )?;
        let rows = stmt.query_map(params![group, person], |r| {
            Ok(FaceItem { id: r.get(0)?, file_id: r.get(1)?, suggested: r.get(2)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library with three groups of faces: two of one person (1 and 2)
    /// and one of someone else (3), plus file 1..=6.
    fn library() -> (tempfile::TempDir, Library, [i64; 3]) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let conn = lib.writer();
        conn.execute_batch(
            "INSERT INTO volumes (id, key, label, kind) VALUES (1, 'v', 'v', 'local');
             INSERT INTO folders (id, volume_id, rel_path) VALUES (1, 1, '');
             INSERT INTO settings (key, value) VALUES ('face_models', 'test'), ('face_match_threshold', '0.5'),
                    ('face_suggest_threshold', '0.9');",
        )
        .unwrap();
        for f in 1..=6 {
            conn.execute(
                "INSERT INTO files (id, folder_id, name, stem, kind, size, mtime, content_hash, sort_date)
                 VALUES (?1, 1, ?1 || '.jpg', ?1, 'jpeg', 1, 0, x'00', '2020')",
                [f],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO face_scans (file_id, model, content_hash, faces) VALUES (?1, 'test', x'00', 1)",
                [f],
            )
            .unwrap();
        }
        // Person A looks along x, a little turned towards y in the second
        // group; person B looks along z.
        let a1 = normalize(&[1.0, 0.0, 0.0]);
        let a2 = normalize(&[1.0, 0.8, 0.0]);
        let b = normalize(&[0.0, 0.0, 1.0]);
        let mut ids = [0; 3];
        for (i, (e, files)) in [(a1, [1, 2]), (a2, [3, 4]), (b, [5, 6])].into_iter().enumerate() {
            conn.execute("INSERT INTO clusters (centroid, size) VALUES (?1, 2)", [to_blob(&e)]).unwrap();
            ids[i] = conn.last_insert_rowid();
            for f in files {
                conn.execute(
                    "INSERT INTO faces (file_id, x, y, w, h, score, embedding, cluster_id)
                     VALUES (?1, 0, 0, 0.2, 0.2, 0.9, ?2, ?3)",
                    params![f, to_blob(&e), ids[i]],
                )
                .unwrap();
            }
        }
        drop(conn);
        (dir, lib, ids)
    }

    #[test]
    fn asks_about_look_alike_groups_and_merges_them() {
        let (_dir, lib, [a1, a2, b]) = library();
        let q = lib.same_person_questions(2, 10).unwrap();
        assert_eq!(q.len(), 1, "{q:?}");
        assert_eq!(pair(q[0].group, q[0].other_group.unwrap()), pair(a1, a2));
        assert_eq!(q[0].photos, 2);

        // "No" is remembered.
        lib.not_same_person(a1, Some(a2), None).unwrap();
        assert!(lib.same_person_questions(2, 10).unwrap().is_empty());

        // Merging anyway puts all four faces in one group, and the old
        // group is gone.
        let keep = lib.merge_groups(&[a2, a1]).unwrap();
        let groups = lib.groups(1).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.iter().find(|g| g.id == keep).unwrap().faces, 4);
        assert!(groups.iter().all(|g| g.id == keep || g.id == b));
        assert_eq!(lib.faces_of(Some(keep), None).unwrap().len(), 4);
        let version = clusters_version(&lib.reader()).unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn asks_whether_a_group_is_someone_named() {
        let (_dir, lib, [a1, a2, b]) = library();
        let person = lib.name_group(a1, "Anna").unwrap();
        let q = lib.same_person_questions(2, 10).unwrap();
        let about_a2 = q.iter().find(|q| q.group == a2).expect("asks about the second group");
        assert_eq!(about_a2.person_id, Some(person));
        assert_eq!(about_a2.name.as_deref(), Some("Anna"));
        assert_eq!(about_a2.other_photos, 2);
        assert!(q.iter().all(|q| q.group != b));

        lib.same_person(a2, None, Some(person)).unwrap();
        assert_eq!(lib.faces_of(None, Some(person)).unwrap().len(), 4);
        assert!(lib.same_person_questions(2, 10).unwrap().is_empty());
    }

    #[test]
    fn picked_faces_move_to_someone_else() {
        let (_dir, lib, [_, _, b]) = library();
        let faces: Vec<i64> = lib.faces_of(Some(b), None).unwrap().iter().map(|f| f.id).collect();
        let bea = lib.name_faces(&faces[..1], "Bea").unwrap();
        // The rest of the group is now suggested as her.
        let hers = lib.faces_of(None, Some(bea)).unwrap();
        assert_eq!(hers.iter().map(|f| f.suggested).collect::<Vec<_>>(), [false, true]);
        assert!(lib.faces_of(Some(b), None).unwrap().is_empty());
        lib.unname_faces(&faces[..1]).unwrap();
        assert!(lib.faces_of(None, Some(bea)).unwrap().is_empty());
    }

    #[test]
    fn pairs_are_ordered() {
        assert_eq!(pair(5, 2), (2, 5));
        assert_eq!(pair(2, 5), (2, 5));
    }
}
