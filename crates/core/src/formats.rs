//! Which files the library picks up, by extension.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Jpeg,
    Raw,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Jpeg => "jpeg",
            FileKind::Raw => "raw",
        }
    }
}

const JPEG_EXTS: &[&str] = &["jpg", "jpeg", "jpe", "jfif"];
const RAW_EXTS: &[&str] = &["cr2", "cr3", "nef", "arw", "dng", "orf", "raf", "rw2"];

pub fn classify(path: &Path) -> Option<FileKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if JPEG_EXTS.contains(&ext.as_str()) {
        Some(FileKind::Jpeg)
    } else if RAW_EXTS.contains(&ext.as_str()) {
        Some(FileKind::Raw)
    } else {
        None
    }
}

/// Folders that are never worth walking: Windows system folders, recycle bins
/// and hidden dot-folders such as `.thumbnails`.
pub fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('$')
        || name.eq_ignore_ascii_case("System Volume Information")
        || name.eq_ignore_ascii_case("@eaDir") // Synology NAS thumbnail folders
        || name.eq_ignore_ascii_case("#recycle")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(classify(Path::new("a/B.JPG")), Some(FileKind::Jpeg));
        assert_eq!(classify(Path::new("x.Cr3")), Some(FileKind::Raw));
        assert_eq!(classify(Path::new("x.png")), None);
        assert_eq!(classify(Path::new("noext")), None);
    }

    #[test]
    fn skips_system_folders() {
        assert!(skip_dir("$RECYCLE.BIN"));
        assert!(skip_dir("@eaDir"));
        assert!(skip_dir(".git"));
        assert!(!skip_dir("Holidays 2015"));
    }
}
