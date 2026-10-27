//! Which files the library picks up, by extension.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        || name.eq_ignore_ascii_case("#snapshot") // Synology snapshots repeat every photo
        || name.eq_ignore_ascii_case("AppData") // browser caches and app icons
}

/// Folders skipped only at the top of a drive that was added whole: Windows
/// itself and installed programs hold thousands of JPEGs that are not
/// anyone's photos.
pub fn skip_root_dir(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "Windows",
        "Program Files",
        "Program Files (x86)",
        "ProgramData",
        "Recovery",
        "PerfLogs",
        "MSOCache",
        "Config.Msi",
        "Intel",
        "AMD",
        "NVIDIA",
        "OneDriveTemp",
    ];
    skip_dir(name) || NAMES.iter().any(|n| name.eq_ignore_ascii_case(n))
}

/// Whether a path below a source folder is, or lies in, a folder the scan
/// skips. `at_root` says the source is a whole drive or share.
pub fn skipped(rel: &Path, at_root: bool) -> bool {
    let mut dirs = rel.components().map(|c| c.as_os_str().to_string_lossy());
    let Some(first) = dirs.next() else { return false };
    (if at_root { skip_root_dir(&first) } else { skip_dir(&first) }) || dirs.any(|d| skip_dir(&d))
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
        assert!(skip_root_dir("program files"));
        assert!(skip_root_dir("$RECYCLE.BIN"));
        assert!(!skip_dir("Windows"), "only skipped at the top of a drive");
    }

    #[test]
    fn skipped_paths() {
        assert!(skipped(Path::new("Windows/Web/img0.jpg"), true));
        assert!(!skipped(Path::new("Windows/Web/img0.jpg"), false));
        assert!(skipped(Path::new("Users/me/AppData/Local/x.jpg"), true));
        assert!(skipped(Path::new("2015/@eaDir/a.jpg"), false));
        assert!(!skipped(Path::new("Photos/2015/a.jpg"), true));
        assert!(!skipped(Path::new("a.jpg"), true));
        assert!(skipped(Path::new("Windows"), true));
    }
}
