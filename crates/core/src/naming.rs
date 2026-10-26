//! People: names on faces, from the user or from tags in the photos, and
//! suggestions for faces nobody has named yet.
//!
//! A face's name has a source: `user` (named in the app), `import` (a name
//! tag with a face area, saved by another program) or `auto` (a suggestion
//! waiting for review). Suggestions are worked out again whenever names
//! change, so they never pile up stale.

use crate::faces::embed::{normalize, similarity};
use crate::facescan::from_blob;
use crate::library::Library;
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// Default similarity for suggesting a name, when the models don't say.
const SUGGEST_THRESHOLD: f32 = 0.40;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub photos: i64,
    /// A face to show for them, if any face is named.
    pub cover: Option<i64>,
    /// Faces suggested as this person, waiting for review.
    pub suggestions: i64,
}

/// A group of faces suggested as one person.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub person_id: i64,
    pub name: String,
    pub faces: Vec<i64>,
    pub photos: i64,
}

/// Tidies a name typed by the user or found in a tag.
fn clean(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The person with this name, created if new. Names match ignoring case.
pub(crate) fn person_id(conn: &Connection, name: &str) -> Result<i64> {
    let name = clean(name);
    if name.is_empty() {
        return Err(Error::Invalid("a name can't be empty".into()));
    }
    conn.execute("INSERT OR IGNORE INTO people (name) VALUES (?1)", [&name])?;
    let (id, known): (i64, String) =
        conn.query_row("SELECT id, name FROM people WHERE name = ?1", [&name], |r| Ok((r.get(0)?, r.get(1)?)))?;
    // "anna" in one photo and "Anna" in another: keep the capitalised one,
    // whichever photo was read first.
    if known != name && !known.chars().any(char::is_uppercase) && name.chars().any(char::is_uppercase) {
        conn.execute("UPDATE people SET name = ?1 WHERE id = ?2", params![name, id])?;
    }
    Ok(id)
}

/// An area as fractions of the photo's width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn iou(&self, o: &Rect) -> f32 {
        let ix = ((self.x + self.w).min(o.x + o.w) - self.x.max(o.x)).max(0.0);
        let iy = ((self.y + self.h).min(o.y + o.h) - self.y.max(o.y)).max(0.0);
        let inter = ix * iy;
        let union = self.w * self.h + o.w * o.h - inter;
        if union > 0.0 {
            inter / union
        } else {
            0.0
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// How well a tagged area and a found face agree: their overlap, or a
    /// little credit when each one's centre lies inside the other, since
    /// programs draw face boxes of quite different sizes.
    fn agreement(&self, face: &Rect) -> f32 {
        let iou = self.iou(face);
        let centred = self.contains(face.x + face.w / 2.0, face.y + face.h / 2.0)
            && face.contains(self.x + self.w / 2.0, self.y + self.h / 2.0);
        if centred {
            iou.max(0.15)
        } else {
            iou
        }
    }

    /// This area, given on the photo as stored, on the photo as shown after
    /// applying its EXIF orientation.
    pub(crate) fn oriented(&self, orientation: u8) -> Rect {
        let map = |x: f32, y: f32| match orientation {
            2 => (1.0 - x, y),
            3 => (1.0 - x, 1.0 - y),
            4 => (x, 1.0 - y),
            5 => (y, x),
            6 => (1.0 - y, x),
            7 => (1.0 - y, 1.0 - x),
            8 => (y, 1.0 - x),
            _ => (x, y),
        };
        let (ax, ay) = map(self.x, self.y);
        let (bx, by) = map(self.x + self.w, self.y + self.h);
        Rect { x: ax.min(bx), y: ay.min(by), w: (ax - bx).abs(), h: (ay - by).abs() }
    }
}

/// Pairs areas with faces, best agreement first, each used once. Returns
/// (area index, face index) pairs.
fn pair(areas: &[Vec<Rect>], faces: &[Rect], min: f32) -> Vec<(usize, usize)> {
    let mut scored = Vec::new();
    for (ai, variants) in areas.iter().enumerate() {
        for (fi, f) in faces.iter().enumerate() {
            let s = variants.iter().map(|a| a.agreement(f)).fold(0.0, f32::max);
            if s >= min {
                scored.push((s, ai, fi));
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (mut used_a, mut used_f) = (HashSet::new(), HashSet::new());
    let mut out = Vec::new();
    for (_, ai, fi) in scored {
        if !used_a.contains(&ai) && !used_f.contains(&fi) {
            used_a.insert(ai);
            used_f.insert(fi);
            out.push((ai, fi));
        }
    }
    out
}

fn file_faces(conn: &Connection, file_id: i64) -> Result<Vec<(i64, Rect, Option<String>)>> {
    let mut stmt = conn.prepare_cached("SELECT id, x, y, w, h, person_source FROM faces WHERE file_id = ?1")?;
    let rows = stmt.query_map([file_id], |r| {
        Ok((r.get(0)?, Rect { x: r.get(1)?, y: r.get(2)?, w: r.get(3)?, h: r.get(4)? }, r.get(5)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Puts the names from a photo's tags on the faces found in it. A name the
/// user gave in the app always wins over a tag.
pub(crate) fn link_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_id = NULL, person_source = NULL WHERE file_id = ?1 AND person_source = 'import'",
        [file_id],
    )?;
    conn.execute("UPDATE tags SET face_id = NULL WHERE file_id = ?1", [file_id])?;
    let faces = file_faces(conn, file_id)?;
    if faces.is_empty() {
        return Ok(());
    }
    let orientation: u8 = conn.query_row("SELECT orientation FROM files WHERE id = ?1", [file_id], |r| r.get(0))?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, person_id, x, y, w, h FROM tags
         WHERE file_id = ?1 AND kind = 'person' AND person_id IS NOT NULL AND x IS NOT NULL",
    )?;
    let tags: Vec<(i64, i64, Rect)> = stmt
        .query_map([file_id], |r| {
            Ok((r.get(0)?, r.get(1)?, Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? }))
        })?
        .collect::<std::result::Result<_, _>>()?;
    // Programs disagree on whether areas are given before or after the
    // photo is turned upright, so both are tried.
    let areas: Vec<Vec<Rect>> = tags.iter().map(|(_, _, a)| vec![*a, a.oriented(orientation)]).collect();
    let rects: Vec<Rect> = faces.iter().map(|f| f.1).collect();
    for (ai, fi) in pair(&areas, &rects, 0.1) {
        let (tag_id, person, _) = tags[ai];
        let (face_id, _, source) = &faces[fi];
        conn.execute("UPDATE tags SET face_id = ?1 WHERE id = ?2", [face_id, &tag_id])?;
        if source.as_deref() != Some("user") {
            conn.execute(
                "UPDATE faces SET person_id = ?1, person_source = 'import' WHERE id = ?2",
                [person, *face_id],
            )?;
        }
    }
    Ok(())
}

/// Names the user gave to a photo's faces, kept while its faces are found
/// again (after the file changed, or with other models).
pub(crate) fn user_names(conn: &Connection, file_id: i64) -> Result<Vec<(Rect, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT x, y, w, h, person_id FROM faces WHERE file_id = ?1 AND person_source = 'user' AND person_id IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([file_id], |r| Ok((Rect { x: r.get(0)?, y: r.get(1)?, w: r.get(2)?, h: r.get(3)? }, r.get(4)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Puts names kept by [`user_names`] back on the matching new faces.
pub(crate) fn restore_names(conn: &Connection, file_id: i64, names: &[(Rect, i64)]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let faces = file_faces(conn, file_id)?;
    let areas: Vec<Vec<Rect>> = names.iter().map(|(r, _)| vec![*r]).collect();
    let rects: Vec<Rect> = faces.iter().map(|f| f.1).collect();
    for (ai, fi) in pair(&areas, &rects, 0.3) {
        conn.execute(
            "UPDATE faces SET person_id = ?1, person_source = 'user' WHERE id = ?2",
            [names[ai].1, faces[fi].0],
        )?;
    }
    Ok(())
}

/// Works out the suggestions again from the names given so far.
///
/// A group of look-alike faces belongs to whoever most of its named faces
/// are; its other faces are suggested as that person. A group with no
/// names is suggested as the person whose named faces it looks most like,
/// if close enough. Faces the user said are not someone are never
/// suggested as them again.
pub(crate) fn refresh(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE faces SET person_id = NULL, person_source = NULL WHERE person_source = 'auto'", [])?;
    tx.execute("UPDATE clusters SET person_id = NULL WHERE person_id IS NOT NULL", [])?;
    let setting = |key: &str| -> Result<Option<String>> {
        Ok(tx.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    };
    let Some(model) = setting("face_models")? else { return Ok(tx.commit()?) };
    let threshold = setting("face_suggest_threshold")?.and_then(|v| v.parse().ok()).unwrap_or(SUGGEST_THRESHOLD);

    // Named faces from the current models.
    let mut stmt = tx.prepare(
        "SELECT f.cluster_id, f.person_id, f.embedding FROM faces f
         JOIN face_scans s ON s.file_id = f.file_id AND s.model = ?1
         WHERE f.person_source IN ('user', 'import') AND f.person_id IS NOT NULL",
    )?;
    let mut votes: HashMap<i64, HashMap<i64, usize>> = HashMap::new();
    let mut prototypes: HashMap<(i64, Option<i64>), Vec<f32>> = HashMap::new();
    let mut unclustered = 0i64;
    let rows = stmt.query_map([&model], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get(1)?, r.get::<_, Vec<u8>>(2)?)))?;
    for row in rows {
        let (cluster, person, blob): (Option<i64>, i64, Vec<u8>) = row?;
        let e = from_blob(&blob);
        if let Some(c) = cluster {
            *votes.entry(c).or_default().entry(person).or_default() += 1;
        }
        // One average face per person and group, so someone photographed
        // as a child and as an adult matches both.
        let key = (
            person,
            cluster.or_else(|| {
                unclustered -= 1;
                Some(unclustered)
            }),
        );
        let sum = prototypes.entry(key).or_insert_with(|| vec![0.0; e.len()]);
        if sum.len() == e.len() {
            for (s, x) in sum.iter_mut().zip(&e) {
                *s += x;
            }
        }
    }
    drop(stmt);
    let prototypes: Vec<(i64, Vec<f32>)> = prototypes.into_iter().map(|((p, _), sum)| (p, normalize(&sum))).collect();

    let mut rejected: HashSet<(i64, i64)> = HashSet::new();
    let mut stmt = tx.prepare(
        "SELECT DISTINCT f.cluster_id, r.person_id FROM face_rejects r JOIN faces f ON f.id = r.face_id
         WHERE f.cluster_id IS NOT NULL",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        rejected.insert(row?);
    }
    drop(stmt);

    let mut owner = HashMap::new();
    let mut stmt = tx.prepare("SELECT id, centroid FROM clusters WHERE hidden = 0")?;
    let clusters: Vec<(i64, Vec<u8>)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    for (id, centroid) in clusters {
        if let Some(v) = votes.get(&id) {
            let (&person, _) = v.iter().max_by_key(|(p, n)| (**n, -**p)).expect("votes are never empty");
            tx.execute("UPDATE clusters SET person_id = ?1 WHERE id = ?2", [person, id])?;
            owner.insert(id, person);
            continue;
        }
        let c = from_blob(&centroid);
        let best = prototypes
            .iter()
            .filter(|(p, e)| e.len() == c.len() && !rejected.contains(&(id, *p)))
            .map(|(p, e)| (similarity(e, &c), *p))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((sim, person)) = best {
            if sim >= threshold {
                owner.insert(id, person);
            }
        }
    }
    let mut update = tx.prepare(
        "UPDATE faces SET person_id = ?1, person_source = 'auto'
         WHERE cluster_id = ?2 AND person_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM face_rejects r WHERE r.face_id = faces.id AND r.person_id = ?1)",
    )?;
    for (cluster, person) in owner {
        update.execute([person, cluster])?;
    }
    drop(update);
    Ok(tx.commit()?)
}

impl Library {
    /// Everyone with at least one photo or suggestion, most photographed
    /// first.
    pub fn people(&self) -> Result<Vec<Person>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT p.id, p.name,
                (SELECT count(*) FROM (
                    SELECT file_id FROM faces WHERE person_id = p.id AND person_source != 'auto'
                    UNION SELECT file_id FROM tags WHERE person_id = p.id)),
                (SELECT id FROM faces WHERE person_id = p.id AND person_source != 'auto'
                    ORDER BY person_source = 'user' DESC, score * w DESC LIMIT 1),
                (SELECT count(*) FROM faces WHERE person_id = p.id AND person_source = 'auto')
             FROM people p",
        )?;
        let mut out: Vec<Person> = stmt
            .query_map([], |r| {
                Ok(Person {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    photos: r.get(2)?,
                    cover: r.get(3)?,
                    suggestions: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|p| p.photos > 0 || p.suggestions > 0)
            .collect();
        out.sort_by(|a, b| b.photos.cmp(&a.photos).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    /// Faces suggested as someone, grouped as they were found, biggest
    /// groups first.
    pub fn suggestions(&self, person: Option<i64>) -> Result<Vec<Suggestion>> {
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(
            "SELECT f.person_id, p.name, f.cluster_id, f.id, f.file_id FROM faces f JOIN people p ON p.id = f.person_id
             WHERE f.person_source = 'auto' AND (?1 IS NULL OR f.person_id = ?1)
             ORDER BY f.score * f.w DESC",
        )?;
        let mut groups: Vec<Suggestion> = Vec::new();
        let mut index: HashMap<(i64, Option<i64>), (usize, HashSet<i64>)> = HashMap::new();
        let rows = stmt.query_map([person], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get(3)?, r.get(4)?))
        })?;
        for row in rows {
            let (person_id, name, cluster, face, file): (i64, String, Option<i64>, i64, i64) = row?;
            let (i, files) = index.entry((person_id, cluster)).or_insert_with(|| {
                groups.push(Suggestion { person_id, name, faces: Vec::new(), photos: 0 });
                (groups.len() - 1, HashSet::new())
            });
            groups[*i].faces.push(face);
            files.insert(file);
            groups[*i].photos = files.len() as i64;
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g.faces.len()));
        Ok(groups)
    }

    /// Names a whole group of look-alike faces.
    pub fn name_group(&self, group: i64, name: &str) -> Result<i64> {
        let conn = self.writer();
        let person = person_id(&conn, name)?;
        conn.execute(
            "DELETE FROM face_rejects WHERE person_id = ?1 AND face_id IN (SELECT id FROM faces WHERE cluster_id = ?2)",
            [person, group],
        )?;
        conn.execute(
            "UPDATE faces SET person_id = ?1, person_source = 'user'
             WHERE cluster_id = ?2 AND (person_id IS NULL OR person_source = 'auto')",
            [person, group],
        )?;
        refresh(&conn)?;
        Ok(person)
    }

    /// Names one face.
    pub fn name_face(&self, face: i64, name: &str) -> Result<i64> {
        self.name_faces(&[face], name)
    }

    /// Names faces picked by the user, such as a few from a group that
    /// are someone else.
    pub fn name_faces(&self, faces: &[i64], name: &str) -> Result<i64> {
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        let person = person_id(&tx, name)?;
        for &face in faces {
            tx.execute("DELETE FROM face_rejects WHERE face_id = ?1 AND person_id = ?2", [face, person])?;
            tx.execute("UPDATE faces SET person_id = ?1, person_source = 'user' WHERE id = ?2", [person, face])?;
        }
        tx.commit()?;
        refresh(&conn)?;
        Ok(person)
    }

    /// "That's not them": takes the name off a face, makes sure it is not
    /// suggested as that person again, and takes it out of its group.
    pub fn unname_face(&self, face: i64) -> Result<()> {
        self.unname_faces(&[face])
    }

    pub fn unname_faces(&self, faces: &[i64]) -> Result<()> {
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        for &face in faces {
            tx.execute(
                "INSERT OR IGNORE INTO face_rejects (face_id, person_id)
                 SELECT id, person_id FROM faces WHERE id = ?1 AND person_id IS NOT NULL",
                [face],
            )?;
            tx.execute(
                "UPDATE faces SET person_id = NULL, person_source = NULL, cluster_id = NULL WHERE id = ?1",
                [face],
            )?;
        }
        tx.commit()?;
        refresh(&conn)
    }

    /// Confirms suggested faces as the person they were suggested as.
    pub fn accept_suggestion(&self, person: i64, faces: &[i64]) -> Result<()> {
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        for f in faces {
            tx.execute(
                "UPDATE faces SET person_source = 'user' WHERE id = ?1 AND person_id = ?2 AND person_source = 'auto'",
                [*f, person],
            )?;
        }
        tx.commit()?;
        refresh(&conn)
    }

    /// Turns down suggested faces: they are not that person.
    pub fn reject_suggestion(&self, person: i64, faces: &[i64]) -> Result<()> {
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        for f in faces {
            tx.execute("INSERT OR IGNORE INTO face_rejects (face_id, person_id) VALUES (?1, ?2)", [*f, person])?;
            tx.execute(
                "UPDATE faces SET person_id = NULL, person_source = NULL
                 WHERE id = ?1 AND person_id = ?2 AND person_source = 'auto'",
                [*f, person],
            )?;
        }
        tx.commit()?;
        refresh(&conn)
    }

    /// Renames someone. Renaming to a name someone else has merges the two.
    pub fn rename_person(&self, person: i64, name: &str) -> Result<i64> {
        let name = clean(name);
        if name.is_empty() {
            return Err(Error::Invalid("a name can't be empty".into()));
        }
        let other: Option<i64> = self
            .writer()
            .query_row("SELECT id FROM people WHERE name = ?1 AND id != ?2", params![name, person], |r| r.get(0))
            .optional()?;
        match other {
            Some(other) => {
                self.merge_people(person, other)?;
                Ok(other)
            }
            None => {
                self.writer().execute("UPDATE people SET name = ?1 WHERE id = ?2", params![name, person])?;
                Ok(person)
            }
        }
    }

    /// Moves everything named `from` over to `into`, and forgets `from`.
    pub fn merge_people(&self, from: i64, into: i64) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE faces SET person_id = ?2 WHERE person_id = ?1", [from, into])?;
        tx.execute("UPDATE tags SET person_id = ?2 WHERE person_id = ?1", [from, into])?;
        tx.execute("UPDATE OR IGNORE face_rejects SET person_id = ?2 WHERE person_id = ?1", [from, into])?;
        tx.execute("DELETE FROM people WHERE id = ?1", [from])?;
        tx.commit()?;
        refresh(&conn)
    }

    /// Forgets a person: their faces become unnamed. Tags in the photos
    /// themselves are left alone.
    pub fn delete_person(&self, person: i64) -> Result<()> {
        let conn = self.writer();
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE faces SET person_id = NULL, person_source = NULL WHERE person_id = ?1", [person])?;
        tx.execute("UPDATE tags SET person_id = NULL WHERE person_id = ?1", [person])?;
        tx.execute("DELETE FROM people WHERE id = ?1", [person])?;
        tx.commit()?;
        refresh(&conn)
    }

    /// Re-does the suggestions, such as after a face scan.
    pub fn refresh_suggestions(&self) -> Result<()> {
        refresh(&self.writer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_turns_areas_upright() {
        let a = Rect { x: 0.1, y: 0.2, w: 0.3, h: 0.1 };
        // Rotated 90° clockwise to show: the top left corner goes top right.
        let r = a.oriented(6);
        assert!((r.x - 0.7).abs() < 1e-6 && (r.y - 0.1).abs() < 1e-6);
        assert!((r.w - 0.1).abs() < 1e-6 && (r.h - 0.3).abs() < 1e-6);
        let r = a.oriented(1);
        assert_eq!((r.x, r.y), (a.x, a.y));
    }

    #[test]
    fn pairs_best_matches_once() {
        let face = |x| Rect { x, y: 0.1, w: 0.2, h: 0.2 };
        let areas = vec![vec![Rect { x: 0.12, y: 0.12, w: 0.2, h: 0.2 }], vec![face(0.6)], vec![face(0.95)]];
        let faces = vec![face(0.1), face(0.55)];
        let mut p = pair(&areas, &faces, 0.1);
        p.sort();
        assert_eq!(p, vec![(0, 0), (1, 1)]);
    }
}
