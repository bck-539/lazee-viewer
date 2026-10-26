//! Tags already saved in photos by other programs: people's names with
//! their face areas, keywords and captions.
//!
//! Windows Photo Gallery saves people as Microsoft Photo regions in XMP and
//! keywords in XMP and in EXIF `XPKeywords`. IrfanView saves IPTC keywords
//! and captions, and JPEG comments. Lightroom, digiKam and others use the
//! Metadata Working Group's regions. All of these are read; nothing is
//! written back here.

use crate::Result;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Person,
    Keyword,
    Caption,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Keyword => "keyword",
            Kind::Caption => "caption",
        }
    }
}

/// An area of the photo as fractions of its width and height, from the
/// top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub kind: Kind,
    pub value: String,
    /// Where the person's face is, when the program saved that.
    pub area: Option<Area>,
    /// Which kind of metadata it came from, such as `xmp-mp` or `iptc`.
    pub source: &'static str,
}

/// Reads the tags of a JPEG, or of a RAW file's `.xmp` sidecar.
pub fn read(path: &Path, is_raw: bool) -> Result<Vec<Tag>> {
    let mut tags = Vec::new();
    if is_raw {
        if let Some(xmp) = sidecar(path) {
            parse_xmp(&std::fs::read(xmp)?, &mut tags);
        }
    } else {
        let mut f = BufReader::with_capacity(64 * 1024, File::open(path)?);
        for (marker, data) in jpeg_segments(&mut f)? {
            match marker {
                0xE1 if data.starts_with(XMP_SIG) => parse_xmp(&data[XMP_SIG.len()..], &mut tags),
                0xE1 if data.starts_with(b"Exif\0\0") => parse_exif(&data[6..], &mut tags),
                0xED if data.starts_with(b"Photoshop 3.0\0") => parse_photoshop(&data[14..], &mut tags),
                0xFE => add(&mut tags, Kind::Caption, &decode_text(&data), None, "comment"),
                _ => {}
            }
        }
    }
    Ok(dedup(tags))
}

const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// `IMG_1.xmp` or `IMG_1.CR2.xmp` next to a RAW file.
fn sidecar(path: &Path) -> Option<std::path::PathBuf> {
    let with_ext = {
        let mut s = path.as_os_str().to_os_string();
        s.push(".xmp");
        std::path::PathBuf::from(s)
    };
    [path.with_extension("xmp"), path.with_extension("XMP"), with_ext].into_iter().find(|p| p.is_file())
}

/// The JPEG's metadata segments, read up to the start of the image data
/// so the rest of the file is never touched.
fn jpeg_segments(r: &mut impl Read) -> Result<Vec<(u8, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut b2 = [0u8; 2];
    r.read_exact(&mut b2)?;
    if b2 != [0xFF, 0xD8] {
        return Ok(out);
    }
    loop {
        let mut m = [0u8; 1];
        // Skip fill bytes up to the marker.
        loop {
            if r.read(&mut m)? == 0 {
                return Ok(out);
            }
            if m[0] == 0xFF {
                break;
            }
        }
        let mut marker = 0xFF;
        while marker == 0xFF {
            if r.read(&mut m)? == 0 {
                return Ok(out);
            }
            marker = m[0];
        }
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 | 0xDA => return Ok(out),
            _ => {}
        }
        r.read_exact(&mut b2)?;
        let len = u16::from_be_bytes(b2) as usize;
        if len < 2 {
            return Ok(out);
        }
        let mut data = vec![0u8; len - 2];
        r.read_exact(&mut data)?;
        if matches!(marker, 0xE1 | 0xED | 0xFE) {
            out.push((marker, data));
        }
    }
}

fn add(tags: &mut Vec<Tag>, kind: Kind, value: &str, area: Option<Area>, source: &'static str) {
    let value = value.trim().trim_matches('\0').trim();
    if value.is_empty() {
        return;
    }
    // Some programs keep people as keywords under a "People" branch.
    if kind == Kind::Keyword {
        for prefix in ["People/", "People|", "Personen/", "Personen|"] {
            if value.len() > prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
                let name = value[prefix.len()..].rsplit(['/', '|']).next().unwrap_or_default();
                return add(tags, Kind::Person, name, None, source);
            }
        }
    }
    tags.push(Tag { kind, value: value.to_string(), area, source });
}

/// Drops repeats, preferring a person tag that has a face area.
fn dedup(tags: Vec<Tag>) -> Vec<Tag> {
    let mut out: Vec<Tag> = Vec::new();
    for t in tags {
        let same = |o: &Tag| o.kind == t.kind && o.value.eq_ignore_ascii_case(&t.value);
        match out.iter_mut().find(|o| same(o) && (o.area.is_none() || t.area.is_none() || o.area == t.area)) {
            Some(o) if o.area.is_none() && t.area.is_some() => *o = t,
            Some(_) => {}
            None => out.push(t),
        }
    }
    // A person named both with and without an area is the same tag once.
    out
}

/// Text that is UTF-8 if it can be, otherwise Windows-1252, IrfanView's
/// default.
fn decode_text(data: &[u8]) -> String {
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => data.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘',
        '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

// ---------- EXIF (Windows keywords and comments) ----------

fn parse_exif(tiff: &[u8], tags: &mut Vec<Tag>) {
    let Ok(exif) = exif::Reader::new().read_raw(tiff.to_vec()) else { return };
    let utf16 = |number: u16| -> Option<String> {
        let f = exif.get_field(exif::Tag(exif::Context::Tiff, number), exif::In::PRIMARY)?;
        let bytes: Vec<u8> = match &f.value {
            exif::Value::Byte(b) => b.clone(),
            exif::Value::Undefined(b, _) => b.clone(),
            _ => return None,
        };
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    };
    // XPKeywords, separated by semicolons.
    if let Some(k) = utf16(0x9C9E) {
        for word in k.split(';') {
            add(tags, Kind::Keyword, word, None, "exif");
        }
    }
    // XPComment and XPTitle.
    for number in [0x9C9C, 0x9C9B] {
        if let Some(c) = utf16(number) {
            add(tags, Kind::Caption, &c, None, "exif");
        }
    }
    if let Some(f) = exif.get_field(exif::Tag::ImageDescription, exif::In::PRIMARY) {
        if let exif::Value::Ascii(v) = &f.value {
            for s in v {
                add(tags, Kind::Caption, &decode_text(s), None, "exif");
            }
        }
    }
}

// ---------- IPTC (IrfanView keywords and captions) ----------

fn parse_photoshop(mut d: &[u8], tags: &mut Vec<Tag>) {
    // Image resource blocks: "8BIM", id, padded Pascal name, size, data.
    while d.len() >= 12 && &d[..4] == b"8BIM" {
        let id = u16::from_be_bytes([d[4], d[5]]);
        let name_len = d[6] as usize;
        let mut pos = 7 + name_len;
        if pos % 2 == 1 {
            pos += 1;
        }
        if d.len() < pos + 4 {
            return;
        }
        let size = u32::from_be_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        pos += 4;
        let Some(data) = d.get(pos..pos + size) else { return };
        if id == 0x0404 {
            parse_iptc(data, tags);
        }
        pos += size + size % 2;
        d = d.get(pos..).unwrap_or_default();
    }
}

fn parse_iptc(mut d: &[u8], tags: &mut Vec<Tag>) {
    let mut utf8 = false;
    let mut found = Vec::new();
    while d.len() >= 5 && d[0] == 0x1C {
        let (record, dataset) = (d[1], d[2]);
        let len = u16::from_be_bytes([d[3], d[4]]) as usize;
        // Extended lengths are only used for huge values; stop there.
        if len & 0x8000 != 0 {
            break;
        }
        let Some(value) = d.get(5..5 + len) else { break };
        match (record, dataset) {
            // Coded character set: ESC % G is UTF-8.
            (1, 90) => utf8 = value == b"\x1B%G",
            (2, 25) => found.push((Kind::Keyword, value)),
            (2, 120) => found.push((Kind::Caption, value)),
            _ => {}
        }
        d = &d[5 + len..];
    }
    for (kind, value) in found {
        let text = if utf8 { String::from_utf8_lossy(value).into_owned() } else { decode_text(value) };
        add(tags, kind, &text, None, "iptc");
    }
}

// ---------- XMP ----------

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const MS_PHOTO: &str = "http://ns.microsoft.com/photo/1.0/";
const MP_REGION: &str = "http://ns.microsoft.com/photo/1.2/t/Region#";
const MPRI: &str = "http://ns.microsoft.com/photo/1.2/t/RegionInfo#";
const MWG_RS: &str = "http://www.metadataworkinggroup.com/schemas/regions/";
const ST_AREA: &str = "http://ns.adobe.com/xmp/sType/Area#";

/// A parsed XML element, with namespaces resolved.
#[derive(Debug, Default)]
struct Node {
    ns: String,
    name: String,
    attrs: Vec<(String, String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    fn descendants<'a>(&'a self, ns: &str, name: &str, out: &mut Vec<&'a Node>) {
        for c in &self.children {
            if c.is(ns, name) {
                out.push(c);
            }
            c.descendants(ns, name, out);
        }
    }

    fn find(&self, ns: &str, name: &str) -> Vec<&Node> {
        let mut out = Vec::new();
        self.descendants(ns, name, &mut out);
        out
    }

    /// A property written either as an attribute or as a child element,
    /// possibly wrapped in an `rdf:Description`.
    fn prop(&self, ns: &str, name: &str) -> Option<String> {
        if let Some((_, _, v)) = self.attrs.iter().find(|(a, n, _)| a == ns && n == name) {
            return Some(v.clone());
        }
        for c in &self.children {
            if c.is(ns, name) {
                return Some(c.text.clone());
            }
            if c.is(RDF, "Description") {
                if let Some(v) = c.prop(ns, name) {
                    return Some(v);
                }
            }
        }
        None
    }

    /// The node holding a structure's fields: the element itself, or its
    /// `rdf:Description` child.
    fn fields(&self, ns: &str, name: &str) -> Option<&Node> {
        let n = self.children.iter().find(|c| c.is(ns, name))?;
        Some(n.children.iter().find(|c| c.is(RDF, "Description")).unwrap_or(n))
    }

    /// Texts of the `rdf:li` items of a bag, sequence or alternative.
    fn items(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        self.descendants(RDF, "li", &mut out);
        out
    }
}

fn parse_tree(xml: &[u8]) -> Option<Node> {
    let mut reader = quick_xml::NsReader::from_reader(xml);
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut buf = Vec::new();
    let resolve = |r: ResolveResult| match r {
        ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
        _ => String::new(),
    };
    loop {
        let (ns, event) = reader.read_resolved_event_into(&mut buf).ok()?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let mut node = Node {
                    ns: resolve(ns),
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Node::default()
                };
                for a in e.attributes().flatten() {
                    let (ans, local) = reader.resolve_attribute(a.key);
                    let value = a.decode_and_unescape_value(reader.decoder()).ok()?.into_owned();
                    node.attrs.push((resolve(ans), String::from_utf8_lossy(local.as_ref()).into_owned(), value));
                }
                if matches!(event, Event::Empty(_)) {
                    stack.last_mut()?.children.push(node);
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                let node = stack.pop()?;
                stack.last_mut()?.children.push(node);
            }
            Event::Text(t) => stack.last_mut()?.text.push_str(&t.decode().ok()?),
            Event::CData(t) => stack.last_mut()?.text.push_str(&String::from_utf8_lossy(&t)),
            Event::GeneralRef(r) => {
                let c = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => {
                        let name = r.decode().ok()?;
                        quick_xml::escape::resolve_predefined_entity(&name).unwrap_or_default().to_string()
                    }
                };
                stack.last_mut()?.text.push_str(&c);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    // Unclosed elements: keep what was read.
    while stack.len() > 1 {
        let node = stack.pop()?;
        stack.last_mut()?.children.push(node);
    }
    stack.pop()
}

fn parse_xmp(xml: &[u8], tags: &mut Vec<Tag>) {
    let Some(root) = parse_tree(xml) else { return };

    for subject in root.find(DC, "subject") {
        for li in subject.items() {
            add(tags, Kind::Keyword, &li.text, None, "xmp");
        }
    }
    for kw in root.find(MS_PHOTO, "LastKeywordXMP") {
        for li in kw.items() {
            add(tags, Kind::Keyword, &li.text, None, "xmp");
        }
    }
    for d in root.find(DC, "description").into_iter().chain(root.find(DC, "title")) {
        if let Some(li) = d.items().first() {
            add(tags, Kind::Caption, &li.text, None, "xmp");
        }
    }

    // Windows Photo Gallery: MP:RegionInfo/MPRI:Regions, each with a
    // name and "left, top, width, height" as fractions.
    for regions in root.find(MPRI, "Regions") {
        for li in regions.items() {
            let Some(name) = li.prop(MP_REGION, "PersonDisplayName") else { continue };
            let area = li.prop(MP_REGION, "Rectangle").and_then(|r| {
                let v: Vec<f32> = r.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                (v.len() == 4).then(|| Area { x: v[0], y: v[1], w: v[2], h: v[3] })
            });
            add(tags, Kind::Person, &name, area.filter(valid), "xmp-mp");
        }
    }

    // Metadata Working Group regions: centre, width and height.
    for list in root.find(MWG_RS, "RegionList") {
        for li in list.items() {
            let kind = li.prop(MWG_RS, "Type").unwrap_or_default();
            if !kind.is_empty() && kind != "Face" {
                continue;
            }
            let Some(name) = li.prop(MWG_RS, "Name") else { continue };
            let area = li.fields(MWG_RS, "Area").and_then(|a| {
                let num = |n| a.prop(ST_AREA, n).and_then(|v| v.trim().parse::<f32>().ok());
                let unit = a.prop(ST_AREA, "unit").unwrap_or_else(|| "normalized".into());
                if unit != "normalized" {
                    return None;
                }
                let (cx, cy, w, h) = (num("x")?, num("y")?, num("w")?, num("h")?);
                Some(Area { x: cx - w / 2.0, y: cy - h / 2.0, w, h })
            });
            add(tags, Kind::Person, &name, area.filter(valid), "xmp-mwg");
        }
    }
}

fn valid(a: &Area) -> bool {
    a.w > 0.0 && a.h > 0.0 && a.x > -0.5 && a.y > -0.5 && a.x + a.w < 1.5 && a.y + a.h < 1.5
}

#[cfg(test)]
mod tests {
    use super::*;

    const WPG_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:MP="http://ns.microsoft.com/photo/1.2/"
          xmlns:MPRI="http://ns.microsoft.com/photo/1.2/t/RegionInfo#"
          xmlns:MPReg="http://ns.microsoft.com/photo/1.2/t/Region#">
        <MP:RegionInfo><rdf:Description>
          <MPRI:Regions><rdf:Bag>
            <rdf:li><rdf:Description MPReg:PersonDisplayName="Anna M&#252;ller" MPReg:Rectangle="0.25, 0.1, 0.2, 0.3"/></rdf:li>
            <rdf:li><rdf:Description><MPReg:PersonDisplayName>Ben &amp; Co</MPReg:PersonDisplayName></rdf:Description></rdf:li>
          </rdf:Bag></MPRI:Regions>
        </rdf:Description></MP:RegionInfo>
      </rdf:Description>
      <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"
          xmlns:MicrosoftPhoto="http://ns.microsoft.com/photo/1.0/">
        <dc:subject><rdf:Bag><rdf:li>Urlaub</rdf:li><rdf:li>People/Carla</rdf:li></rdf:Bag></dc:subject>
        <MicrosoftPhoto:LastKeywordXMP><rdf:Bag><rdf:li>Urlaub</rdf:li></rdf:Bag></MicrosoftPhoto:LastKeywordXMP>
      </rdf:Description>
    </rdf:RDF></x:xmpmeta>"#;

    const MWG_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
          xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
        <mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag>
          <rdf:li rdf:parseType="Resource"><mwg-rs:Name>Dora</mwg-rs:Name><mwg-rs:Type>Face</mwg-rs:Type>
            <mwg-rs:Area stArea:x="0.5" stArea:y="0.5" stArea:w="0.2" stArea:h="0.4" stArea:unit="normalized"/></rdf:li>
          <rdf:li><rdf:Description mwg-rs:Name="Pet" mwg-rs:Type="Pet"/></rdf:li>
        </rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions>
      </rdf:Description>
    </rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn windows_photo_gallery_regions_and_keywords() {
        let mut tags = Vec::new();
        parse_xmp(WPG_XMP.as_bytes(), &mut tags);
        let tags = dedup(tags);
        let anna = tags.iter().find(|t| t.value == "Anna Müller").unwrap();
        assert_eq!(anna.kind, Kind::Person);
        assert_eq!(anna.area, Some(Area { x: 0.25, y: 0.1, w: 0.2, h: 0.3 }));
        assert!(tags.iter().any(|t| t.kind == Kind::Person && t.value == "Ben & Co" && t.area.is_none()));
        assert!(tags.iter().any(|t| t.kind == Kind::Person && t.value == "Carla"));
        assert_eq!(tags.iter().filter(|t| t.value == "Urlaub").count(), 1);
    }

    #[test]
    fn mwg_regions_are_centred() {
        let mut tags = Vec::new();
        parse_xmp(MWG_XMP.as_bytes(), &mut tags);
        assert_eq!(tags.len(), 1);
        let a = tags[0].area.unwrap();
        assert!((a.x - 0.4).abs() < 1e-6 && (a.y - 0.3).abs() < 1e-6);
    }

    fn segment(marker: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend(((data.len() + 2) as u16).to_be_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn reads_iptc_comment_and_xmp_from_a_jpeg() {
        // IrfanView-style IPTC: Latin-1 keyword and caption.
        let mut iptc = Vec::new();
        for (ds, v) in [(25u8, &b"K\xF6ln"[..]), (25, b"Dom"), (120, b"Ein Tag")] {
            iptc.extend([0x1C, 2, ds]);
            iptc.extend((v.len() as u16).to_be_bytes());
            iptc.extend(v);
        }
        let mut ps = b"Photoshop 3.0\08BIM\x04\x04\0\0".to_vec();
        ps.extend((iptc.len() as u32).to_be_bytes());
        ps.extend(&iptc);
        let mut xmp = XMP_SIG.to_vec();
        xmp.extend(MWG_XMP.as_bytes());

        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend(segment(0xE1, &xmp));
        jpeg.extend(segment(0xED, &ps));
        jpeg.extend(segment(0xFE, b"Geburtstag"));
        jpeg.extend([0xFF, 0xDA, 0, 2, 1, 2, 3]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, &jpeg).unwrap();

        let tags = read(&path, false).unwrap();
        let values: Vec<_> = tags.iter().map(|t| (t.kind, t.value.as_str())).collect();
        assert!(values.contains(&(Kind::Keyword, "Köln")));
        assert!(values.contains(&(Kind::Keyword, "Dom")));
        assert!(values.contains(&(Kind::Caption, "Ein Tag")));
        assert!(values.contains(&(Kind::Caption, "Geburtstag")));
        assert!(values.contains(&(Kind::Person, "Dora")));
    }

    #[test]
    fn raw_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("IMG_1.CR2");
        std::fs::write(&raw, b"raw").unwrap();
        assert!(read(&raw, true).unwrap().is_empty());
        std::fs::write(dir.path().join("IMG_1.xmp"), WPG_XMP).unwrap();
        assert!(read(&raw, true).unwrap().iter().any(|t| t.value == "Anna Müller"));
    }
}
