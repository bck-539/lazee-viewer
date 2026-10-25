//! EXIF metadata: capture date, camera, orientation, size and GPS.

use exif::{In, Reader, Tag, Value};
use std::io::{BufRead, Cursor, Seek};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// Local capture time as `YYYY-MM-DDTHH:MM:SS`, as the camera recorded it.
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    /// EXIF orientation, 1 to 8. 1 means no rotation is needed.
    pub orientation: u8,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub gps: Option<(f64, f64)>,
}

/// Reads EXIF from a JPEG or TIFF-based file. Files without EXIF, or with
/// EXIF we can't parse, give default metadata rather than an error: a photo
/// with a broken EXIF block is still a photo.
pub fn read<R: BufRead + Seek>(reader: &mut R) -> Meta {
    match Reader::new().read_from_container(reader) {
        Ok(exif) => from_exif(&exif),
        Err(_) => Meta { orientation: 1, ..Meta::default() },
    }
}

pub fn read_bytes(data: &[u8]) -> Meta {
    read(&mut Cursor::new(data))
}

/// Reads EXIF from a bare TIFF structure, such as the start of a TIFF-based
/// RAW file or one of the metadata boxes in a CR3.
pub fn read_tiff(data: Vec<u8>) -> Meta {
    match Reader::new().read_raw(data) {
        Ok(exif) => from_exif(&exif),
        Err(_) => Meta { orientation: 1, ..Meta::default() },
    }
}

impl Meta {
    /// Fills whatever is missing here from `other`.
    pub fn or(self, other: Meta) -> Meta {
        Meta {
            taken_at: self.taken_at.or(other.taken_at),
            camera_make: self.camera_make.or(other.camera_make),
            camera_model: self.camera_model.or(other.camera_model),
            orientation: if self.orientation != 1 { self.orientation } else { other.orientation },
            width: self.width.or(other.width),
            height: self.height.or(other.height),
            gps: self.gps.or(other.gps),
        }
    }
}

fn from_exif(exif: &exif::Exif) -> Meta {
    // Fields are matched by tag number, not by the IFD they were found in:
    // CR3 files keep the EXIF IFD as a TIFF of its own, so its date fields
    // turn up as if they sat in IFD0.
    let field = |tag: Tag| {
        exif.get_field(tag, In::PRIMARY)
            .or_else(|| exif.fields().find(|f| f.tag.number() == tag.number() && f.ifd_num == In::PRIMARY))
    };
    let text = |tag| {
        field(tag).and_then(|f| match &f.value {
            Value::Ascii(v) => v.first().map(|s| String::from_utf8_lossy(s).trim().to_string()),
            _ => None,
        })
    };
    let uint = |tag, _ifd: In| field(tag).and_then(|f| f.value.get_uint(0));

    let taken_at = [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime]
        .into_iter()
        .find_map(|t| text(t).and_then(|s| parse_exif_date(&s)));
    let orientation = uint(Tag::Orientation, In::PRIMARY).filter(|o| (1..=8).contains(o)).unwrap_or(1) as u8;
    let width = uint(Tag::PixelXDimension, In::PRIMARY).or_else(|| uint(Tag::ImageWidth, In::PRIMARY));
    let height = uint(Tag::PixelYDimension, In::PRIMARY).or_else(|| uint(Tag::ImageLength, In::PRIMARY));

    Meta {
        taken_at,
        camera_make: text(Tag::Make).filter(|s| !s.is_empty()),
        camera_model: text(Tag::Model).filter(|s| !s.is_empty()),
        orientation,
        width,
        height,
        gps: gps(exif),
    }
}

/// `2015:07:04 18:30:00` to `2015-07-04T18:30:00`. Cameras with an unset
/// clock write zeros or spaces, which we treat as no date.
pub fn parse_exif_date(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1800..=2200).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60
    {
        return None;
    }
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}"))
}

fn gps(exif: &exif::Exif) -> Option<(f64, f64)> {
    let coord = |tag, ref_tag, neg: &[u8]| -> Option<f64> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        let Value::Rational(r) = &f.value else { return None };
        if r.len() < 3 {
            return None;
        }
        let v = r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0;
        let negative = exif
            .get_field(ref_tag, In::PRIMARY)
            .and_then(|f| match &f.value {
                Value::Ascii(a) => a.first().map(|s| s.starts_with(neg)),
                _ => None,
            })
            .unwrap_or(false);
        v.is_finite().then_some(if negative { -v } else { v })
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, b"S")?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, b"W")?;
    // 0,0 is what many phones write when they had no fix.
    (lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0)).then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dates() {
        assert_eq!(parse_exif_date("2015:07:04 18:30:00").as_deref(), Some("2015-07-04T18:30:00"));
        assert_eq!(parse_exif_date("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_date("    :  :     :  :  "), None);
        assert_eq!(parse_exif_date("2015:07:04"), None);
    }

    #[test]
    fn missing_exif_gives_defaults() {
        let m = read_bytes(b"not an image");
        assert_eq!(m.orientation, 1);
        assert_eq!(m.taken_at, None);
    }
}
