//! Writing names back into the photos, so other programs see them: an
//! optional setting, off by default.
//!
//! Names go into the photo's XMP as Microsoft Photo regions (read by
//! Windows Photo Gallery and Windows itself) and Metadata Working Group
//! regions (Lightroom, digiKam and others). JPEGs get them inside the file;
//! RAW files get an `.xmp` file beside them. Only the region part of the
//! XMP is replaced: keywords, captions and everything else stay. A photo
//! is only written when the names in it differ from the library's, and the
//! image data itself is copied byte for byte.

use crate::library::Library;
use crate::naming::Rect;
use crate::scanner::ScanControl;
use crate::tags::{self, XMP_SIG};
use crate::{hash, volume, Error, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SETTING: &str = "write_back";

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MP: &str = "http://ns.microsoft.com/photo/1.2/";
const MWG_RS: &str = "http://www.metadataworkinggroup.com/schemas/regions/";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct WriteSummary {
    pub written: u64,
    pub failed: u64,
    pub cancelled: bool,
}

/// A name to write, with its face area on the photo as stored (before
/// EXIF rotation), if known.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub name: String,
    pub area: Option<Rect>,
}

struct Todo {
    file_id: i64,
    path: PathBuf,
    raw: bool,
    size: (Option<u32>, Option<u32>),
    regions: Vec<Region>,
}

pub fn enabled(lib: &Library) -> bool {
    lib.setting(SETTING).ok().flatten().as_deref() == Some("1")
}

/// Writes the names of every photo whose names changed in the library.
pub fn run(lib: &Library, ctl: &ScanControl) -> Result<WriteSummary> {
    let mut summary = WriteSummary::default();
    let todo = load_todo(&lib.reader(), &lib.roots_snapshot())?;
    for t in todo {
        if !ctl.checkpoint() {
            break;
        }
        match write_one(lib, &t) {
            Ok(()) => summary.written += 1,
            Err(e) => {
                log::warn!("could not write names to {}: {e}", t.path.display());
                summary.failed += 1;
            }
        }
    }
    summary.cancelled = ctl.is_cancelled();
    Ok(summary)
}

/// The inverse of an EXIF orientation: turns an area on the photo as shown
/// back to the photo as stored.
fn unorient(o: u8) -> u8 {
    match o {
        6 => 8,
        8 => 6,
        o => o,
    }
}

fn load_todo(conn: &Connection, roots: &HashMap<String, PathBuf>) -> Result<Vec<Todo>> {
    // What the library says is in each photo...
    let mut wanted: BTreeMap<i64, Vec<Region>> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT f.file_id, p.name, f.x, f.y, f.w, f.h, fi.orientation FROM faces f
         JOIN people p ON p.id = f.person_id JOIN files fi ON fi.id = f.file_id
         WHERE f.person_source IN ('user', 'import')",
    )?;
    let rows = stmt.query_map([], |r| {
        let rect = Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? };
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, rect.oriented(unorient(r.get(6)?))))
    })?;
    for row in rows {
        let (file, name, area) = row?;
        wanted.entry(file).or_default().push(Region { name, area: Some(area) });
    }
    // ...plus names from the photo's own tags that are not on a found face,
    // keeping the area the tagging program saved.
    let mut stmt = conn.prepare(
        "SELECT t.file_id, p.name, t.x, t.y, t.w, t.h FROM tags t JOIN people p ON p.id = t.person_id
         WHERE t.kind = 'person' AND t.face_id IS NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        let area = match r.get::<_, Option<f32>>(2)? {
            Some(x) => Some(Rect { x, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? }),
            None => None,
        };
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, area))
    })?;
    for row in rows {
        let (file, name, area) = row?;
        let regions = wanted.entry(file).or_default();
        if !regions.iter().any(|g| g.name.eq_ignore_ascii_case(&name)) {
            regions.push(Region { name, area });
        }
    }
    // ...against the names found in the photo when it was last read.
    let mut have: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT file_id, value FROM tags WHERE kind = 'person'")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (file, name) = row?;
        have.entry(file).or_default().insert(name.to_lowercase());
    }
    let mut files: HashSet<i64> = wanted.keys().copied().collect();
    files.extend(have.keys());

    let mut stmt = conn.prepare_cached(
        "SELECT v.key, fo.rel_path, f.name, f.kind, f.width, f.height FROM files f
         JOIN folders fo ON fo.id = f.folder_id JOIN volumes v ON v.id = fo.volume_id
         JOIN tag_scans s ON s.file_id = f.id AND s.content_hash = f.content_hash AND s.error IS NULL
         WHERE f.id = ?1 AND f.paired_with IS NULL AND f.error IS NULL",
    )?;
    let mut out = Vec::new();
    for file in files {
        let regions = wanted.remove(&file).unwrap_or_default();
        let names: HashSet<String> = regions.iter().map(|g| g.name.to_lowercase()).collect();
        if names == have.remove(&file).unwrap_or_default() {
            continue;
        }
        // Only photos whose tags are up to date, so nothing in them is
        // overwritten unseen.
        let Some((vol, rel, name, kind, w, h)) =
            stmt.query_row([file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).ok()
        else {
            continue;
        };
        let (vol, rel, name, kind): (String, String, String, String) = (vol, rel, name, kind);
        if let Some(root) = roots.get(&vol) {
            let path = volume::join(root, &rel).join(name);
            out.push(Todo { file_id: file, path, raw: kind == "raw", size: (w, h), regions });
        }
    }
    out.sort_by_key(|t| t.file_id);
    Ok(out)
}

fn write_one(lib: &Library, t: &Todo) -> Result<()> {
    if t.raw {
        let sidecar = tags::sidecar(&t.path).unwrap_or_else(|| t.path.with_extension("xmp"));
        let old = match std::fs::read(&sidecar) {
            Ok(d) => Some(d),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let xmp = rewrite_xmp(old.as_deref(), &t.regions, t.size)?;
        replace_file(&sidecar, &xmp)?;
    } else {
        let data = std::fs::read(&t.path)?;
        let out = jpeg_with_xmp(&data, &t.regions, t.size)?;
        replace_file(&t.path, &out)?;
    }
    after_write(lib, t)
}

/// Writes next to the file and then swaps it in, so a crash or a full disk
/// never leaves half a photo.
fn replace_file(path: &Path, data: &[u8]) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.lazee-tmp"));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

/// Brings the library up to date with the file just written, so it is not
/// looked at again for faces just because its metadata changed.
fn after_write(lib: &Library, t: &Todo) -> Result<()> {
    let found = tags::read(&t.path, t.raw)?;
    let mut f = std::fs::File::open(&t.path)?;
    let meta = f.metadata()?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let new_hash = hash::from_file(&mut f, len)?;
    let conn = lib.writer();
    let tx = conn.unchecked_transaction()?;
    if !t.raw {
        let old: Vec<u8> = tx.query_row("SELECT content_hash FROM files WHERE id = ?1", [t.file_id], |r| r.get(0))?;
        tx.execute(
            "UPDATE files SET size = ?1, mtime = ?2, content_hash = ?3 WHERE id = ?4",
            params![len as i64, mtime, &new_hash[..], t.file_id],
        )?;
        tx.execute(
            "UPDATE face_scans SET content_hash = ?1 WHERE file_id = ?2 AND content_hash = ?3",
            params![&new_hash[..], t.file_id, old],
        )?;
        // The thumbnail is the same picture.
        tx.execute(
            "INSERT OR IGNORE INTO thumbs.thumbs (content_hash, width, height, jpeg)
             SELECT ?1, width, height, jpeg FROM thumbs.thumbs WHERE content_hash = ?2",
            params![&new_hash[..], old],
        )?;
    }
    crate::tagscan::store_tags(&tx, t.file_id, &found)?;
    tx.execute(
        "UPDATE tag_scans SET content_hash = (SELECT content_hash FROM files WHERE id = ?1) WHERE file_id = ?1",
        [t.file_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// A JPEG with its XMP regions replaced. Everything else is kept as is.
pub fn jpeg_with_xmp(data: &[u8], regions: &[Region], size: (Option<u32>, Option<u32>)) -> Result<Vec<u8>> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(Error::Decode("not a JPEG".into()));
    }
    // Walk the segments before the image data.
    let mut pos = 2;
    let mut xmp_at: Option<(usize, usize)> = None;
    let mut insert_at = 2;
    loop {
        if pos + 4 > data.len() || data[pos] != 0xFF {
            return Err(Error::Decode("unexpected JPEG layout".into()));
        }
        let marker = data[pos + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        let end = pos + 2 + len;
        if len < 2 || end > data.len() {
            return Err(Error::Decode("truncated JPEG segment".into()));
        }
        let body = &data[pos + 4..end];
        if marker == 0xE1 && body.starts_with(XMP_SIG) && xmp_at.is_none() {
            xmp_at = Some((pos, end));
        }
        // New XMP goes after APP0 (JFIF) and the EXIF block.
        if marker == 0xE0 || (marker == 0xE1 && body.starts_with(b"Exif\0\0")) {
            insert_at = end;
        }
        pos = end;
    }
    let old = xmp_at.map(|(s, e)| &data[s + 4 + XMP_SIG.len()..e]);
    let xmp = rewrite_xmp(old, regions, size)?;
    let seg_len = 2 + XMP_SIG.len() + xmp.len();
    if seg_len > 0xFFFF {
        return Err(Error::Invalid("the photo's XMP is too large to add names to".into()));
    }
    let mut segment = vec![0xFF, 0xE1];
    segment.extend((seg_len as u16).to_be_bytes());
    segment.extend(XMP_SIG);
    segment.extend(&xmp);
    let (cut_from, cut_to) = xmp_at.unwrap_or((insert_at, insert_at));
    let mut out = Vec::with_capacity(data.len() + segment.len());
    out.extend(&data[..cut_from]);
    out.extend(&segment);
    out.extend(&data[cut_to..]);
    Ok(out)
}

/// XMP with its people regions replaced by `regions`, keeping everything
/// else in `old`, or a new packet when there is none.
pub fn rewrite_xmp(old: Option<&[u8]>, regions: &[Region], size: (Option<u32>, Option<u32>)) -> Result<Vec<u8>> {
    let block = regions_xml(regions, size);
    let Some(old) = old.filter(|o| !o.iter().all(u8::is_ascii_whitespace)) else {
        return Ok(format!(
            "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\">{block}</rdf:RDF></x:xmpmeta>\n\
             <?xpacket end=\"w\"?>"
        )
        .into_bytes());
    };
    let mut reader = quick_xml::NsReader::from_reader(old);
    let mut writer = quick_xml::Writer::new(Vec::with_capacity(old.len() + block.len()));
    let mut buf = Vec::new();
    let mut skip = 0usize;
    let mut inserted = false;
    // An rdf:Description is held back until something is written inside
    // it, so one left empty by dropping its regions disappears.
    let mut pending: Option<(Vec<u8>, Vec<u8>)> = None;
    let bad = |e: &dyn std::fmt::Display| Error::Decode(format!("XMP: {e}"));
    loop {
        let (ns, event) = reader.read_resolved_event_into(&mut buf).map_err(|e| bad(&e))?;
        let is = |ns: &ResolveResult, uri: &str| matches!(ns, ResolveResult::Bound(n) if n.as_ref() == uri.as_bytes());
        let local = |e: &BytesStart| e.local_name().as_ref().to_vec();
        match &event {
            Event::Start(_) if skip > 0 => {
                skip += 1;
                buf.clear();
                continue;
            }
            Event::End(_) if skip > 0 => {
                skip -= 1;
                buf.clear();
                continue;
            }
            _ if skip > 0 => {
                buf.clear();
                continue;
            }
            // Old regions are dropped; ours replace them.
            Event::Start(e)
                if (is(&ns, MP) && local(e) == b"RegionInfo") || (is(&ns, MWG_RS) && local(e) == b"Regions") =>
            {
                skip = 1;
                buf.clear();
                continue;
            }
            Event::Empty(e)
                if (is(&ns, MP) && local(e) == b"RegionInfo") || (is(&ns, MWG_RS) && local(e) == b"Regions") =>
            {
                buf.clear();
                continue;
            }
            Event::Start(e) if is(&ns, RDF) && local(e) == b"Description" => {
                if let Some((start, ws)) = pending.take() {
                    writer.get_mut().extend(start);
                    writer.get_mut().extend(ws);
                }
                let mut start = Vec::new();
                quick_xml::Writer::new(&mut start).write_event(event.borrow()).map_err(|e| bad(&e))?;
                pending = Some((start, Vec::new()));
                buf.clear();
                continue;
            }
            Event::Text(t) if pending.is_some() && t.iter().all(u8::is_ascii_whitespace) => {
                pending.as_mut().expect("checked").1.extend(t.iter());
                buf.clear();
                continue;
            }
            Event::End(e) if pending.is_some() && is(&ns, RDF) && e.local_name().as_ref() == b"Description" => {
                pending = None;
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {}
        }
        if let Some((start, ws)) = pending.take() {
            writer.get_mut().extend(start);
            writer.get_mut().extend(ws);
        }
        if let Event::End(e) = &event {
            if is(&ns, RDF) && e.local_name().as_ref() == b"RDF" && !inserted {
                writer.get_mut().extend(block.as_bytes());
                inserted = true;
            }
        }
        writer.write_event(event.borrow()).map_err(|e| bad(&e))?;
        buf.clear();
    }
    if !inserted {
        return Err(Error::Decode("XMP has no rdf:RDF element".into()));
    }
    Ok(writer.into_inner())
}

fn regions_xml(regions: &[Region], size: (Option<u32>, Option<u32>)) -> String {
    use quick_xml::escape::escape;
    let num = |v: f32| format!("{:.6}", v.clamp(0.0, 1.0));
    let mut mp = String::new();
    let mut mwg = String::new();
    for g in regions {
        let name = escape(g.name.as_str());
        match &g.area {
            Some(a) => {
                mp.push_str(&format!(
                    "<rdf:li><rdf:Description MPReg:PersonDisplayName=\"{name}\" MPReg:Rectangle=\"{}, {}, {}, {}\"/></rdf:li>",
                    num(a.x),
                    num(a.y),
                    num(a.w),
                    num(a.h)
                ));
                mwg.push_str(&format!(
                    "<rdf:li><rdf:Description mwg-rs:Name=\"{name}\" mwg-rs:Type=\"Face\"><mwg-rs:Area stArea:x=\"{}\" \
                     stArea:y=\"{}\" stArea:w=\"{}\" stArea:h=\"{}\" stArea:unit=\"normalized\"/></rdf:Description></rdf:li>",
                    num(a.x + a.w / 2.0),
                    num(a.y + a.h / 2.0),
                    num(a.w),
                    num(a.h)
                ));
            }
            None => mp.push_str(&format!("<rdf:li><rdf:Description MPReg:PersonDisplayName=\"{name}\"/></rdf:li>")),
        }
    }
    let dims = match size {
        (Some(w), Some(h)) => {
            format!("<mwg-rs:AppliedToDimensions stDim:w=\"{w}\" stDim:h=\"{h}\" stDim:unit=\"pixel\"/>")
        }
        _ => String::new(),
    };
    let mwg = if mwg.is_empty() {
        String::new()
    } else {
        format!("<mwg-rs:Regions rdf:parseType=\"Resource\">{dims}<mwg-rs:RegionList><rdf:Bag>{mwg}</rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions>")
    };
    format!(
        "<rdf:Description rdf:about=\"\" xmlns:MP=\"{MP}\" xmlns:MPRI=\"http://ns.microsoft.com/photo/1.2/t/RegionInfo#\" \
         xmlns:MPReg=\"http://ns.microsoft.com/photo/1.2/t/Region#\" xmlns:mwg-rs=\"{MWG_RS}\" \
         xmlns:stArea=\"http://ns.adobe.com/xmp/sType/Area#\" xmlns:stDim=\"http://ns.adobe.com/xap/1.0/sType/Dimensions#\">\
         <MP:RegionInfo rdf:parseType=\"Resource\"><MPRI:Regions><rdf:Bag>{mp}</rdf:Bag></MPRI:Regions></MP:RegionInfo>\
         {mwg}</rdf:Description>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::{Area, Kind};

    fn tags_of(xmp: &[u8]) -> Vec<tags::Tag> {
        let mut out = Vec::new();
        tags::parse_xmp_for_test(xmp, &mut out);
        out
    }

    #[test]
    fn replaces_regions_and_keeps_the_rest() {
        let old = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:MP="http://ns.microsoft.com/photo/1.2/"
             xmlns:MPRI="http://ns.microsoft.com/photo/1.2/t/RegionInfo#" xmlns:MPReg="http://ns.microsoft.com/photo/1.2/t/Region#">
            <dc:subject><rdf:Bag><rdf:li>Urlaub &amp; Meer</rdf:li></rdf:Bag></dc:subject>
            <MP:RegionInfo><rdf:Description><MPRI:Regions><rdf:Bag>
              <rdf:li><rdf:Description MPReg:PersonDisplayName="Old Name" MPReg:Rectangle="0.1, 0.1, 0.1, 0.1"/></rdf:li>
            </rdf:Bag></MPRI:Regions></rdf:Description></MP:RegionInfo>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let regions = vec![
            Region { name: "Anna <A>".into(), area: Some(Rect { x: 0.2, y: 0.3, w: 0.1, h: 0.2 }) },
            Region { name: "Ben".into(), area: None },
        ];
        let new = rewrite_xmp(Some(old), &regions, (Some(4000), Some(3000))).unwrap();
        let tags = tags_of(&new);
        assert!(tags.iter().any(|t| t.kind == Kind::Keyword && t.value == "Urlaub & Meer"));
        assert!(!tags.iter().any(|t| t.value == "Old Name"));
        let anna: Vec<_> = tags.iter().filter(|t| t.value == "Anna <A>").collect();
        // Both the Microsoft and the MWG region, at the same place.
        assert_eq!(anna.len(), 2);
        for t in anna {
            let a = t.area.unwrap();
            assert!((a.x - 0.2).abs() < 1e-5 && (a.y - 0.3).abs() < 1e-5 && (a.h - 0.2).abs() < 1e-5);
        }
        assert!(tags.iter().any(|t| t.value == "Ben" && t.area.is_none()));
        let _ = Area { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    }

    #[test]
    fn jpeg_keeps_its_image_data() {
        let img = image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8 * 6, y as u8 * 8, 77]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&img).unwrap();
        let regions = vec![Region { name: "Cara".into(), area: Some(Rect { x: 0.1, y: 0.1, w: 0.5, h: 0.5 }) }];
        let once = jpeg_with_xmp(&jpeg, &regions, (Some(40), Some(30))).unwrap();
        let twice = jpeg_with_xmp(&once, &regions, (Some(40), Some(30))).unwrap();
        assert_eq!(once, twice, "writing the same names again changes nothing");
        let sos = |d: &[u8]| d.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        assert_eq!(&once[sos(&once)..], &jpeg[sos(&jpeg)..]);
        assert_eq!(
            image::load_from_memory(&once).unwrap().to_rgb8(),
            image::load_from_memory(&jpeg).unwrap().to_rgb8()
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, &once).unwrap();
        let tags = tags::read(&path, false).unwrap();
        assert_eq!(tags.iter().filter(|t| t.value == "Cara").count(), 1);
    }
}
