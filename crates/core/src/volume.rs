//! Finding which drive a folder lives on, independent of its drive letter.
//!
//! Every photo is stored as a volume key plus a path relative to that
//! volume's root. A local or USB drive is keyed by its volume serial number,
//! so a USB disk that comes back as `E:` instead of `F:` is still the same
//! volume. A network share is keyed by its UNC name (`\\nas\photos`), whether
//! it was added through that name or a mapped drive letter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeKind {
    Fixed,
    Removable,
    Network,
    Other,
}

impl VolumeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeKind::Fixed => "fixed",
            VolumeKind::Removable => "removable",
            VolumeKind::Network => "network",
            VolumeKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "fixed" => VolumeKind::Fixed,
            "removable" => VolumeKind::Removable,
            "network" => VolumeKind::Network,
            _ => VolumeKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Stable identity, such as `vol:1A2B3C4D` or `unc:\\nas\photos`.
    pub key: String,
    /// Human-readable name for the UI, such as the drive label.
    pub label: String,
    pub kind: VolumeKind,
}

/// A path split into the volume it lives on and its place on that volume.
#[derive(Debug, Clone)]
pub struct Located {
    pub volume: Volume,
    /// Where the volume is mounted right now.
    pub root: PathBuf,
    /// Path below the root with `/` separators and no leading separator.
    pub rel: String,
}

pub fn locate(path: &Path) -> std::io::Result<Located> {
    let path = std::path::absolute(path)?;
    let (volume, root) = sys::volume_of(&path)?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| std::io::Error::other(format!("{} is not under {}", path.display(), root.display())))?;
    Ok(Located { volume, root, rel: rel_to_string(rel) })
}

/// Current mount roots for the given volume keys. Keys that are not
/// reachable right now (an unplugged USB disk, an offline NAS) are missing
/// from the result.
pub fn resolve(keys: &[String]) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let local = sys::mounted_local();
    let mut pending = Vec::new();
    for key in keys {
        if let Some(root) = local.get(key) {
            out.insert(key.clone(), root.clone());
        } else if let Some(root) = sys::root_from_key(key) {
            pending.push((key.clone(), root));
        }
    }
    // Network shares can take tens of seconds to time out when the NAS is
    // off, so probe them in parallel and give up quickly.
    let (tx, rx) = mpsc::channel();
    let n = pending.len();
    for (key, root) in pending {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let ok = root.is_dir();
            let _ = tx.send((key, root, ok));
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    for _ in 0..n {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok((key, root, true)) => {
                out.insert(key, root);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

pub fn rel_to_string(rel: &Path) -> String {
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    parts.join("/")
}

pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    p
}

#[cfg(windows)]
mod sys {
    use super::{Volume, VolumeKind};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, GetVolumePathNameW,
    };

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
    }

    fn volume_root(path: &Path) -> std::io::Result<PathBuf> {
        let w = wide(path.as_os_str());
        let mut buf = vec![0u16; 1024];
        // SAFETY: both buffers are valid and NUL-terminated or sized as passed.
        let ok = unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(PathBuf::from(from_wide(&buf)))
    }

    fn drive_type(root: &Path) -> u32 {
        let w = wide(root.as_os_str());
        // SAFETY: `w` is NUL-terminated.
        unsafe { GetDriveTypeW(w.as_ptr()) }
    }

    /// `\\server\share` behind a mapped drive letter such as `Z:`.
    fn mapped_unc(root: &Path) -> Option<String> {
        let letter: String = root.to_string_lossy().chars().take(2).collect();
        let w = wide(std::ffi::OsStr::new(&letter));
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buffers are valid; `len` holds the buffer size in characters.
        let rc = unsafe { WNetGetConnectionW(w.as_ptr(), buf.as_mut_ptr(), &mut len) };
        (rc == 0).then(|| from_wide(&buf))
    }

    fn local_volume(root: &Path, dtype: u32) -> std::io::Result<Volume> {
        let w = wide(root.as_os_str());
        let mut name = vec![0u16; 261];
        let mut serial = 0u32;
        // SAFETY: buffers are valid; unused out-parameters are null.
        let ok = unsafe {
            GetVolumeInformationW(
                w.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let label = from_wide(&name);
        let letter = root.to_string_lossy().trim_end_matches('\\').to_string();
        Ok(Volume {
            key: format!("vol:{serial:08X}"),
            label: if label.is_empty() { letter } else { format!("{label} ({letter})") },
            kind: match dtype {
                DRIVE_REMOVABLE => VolumeKind::Removable,
                DRIVE_FIXED => VolumeKind::Fixed,
                _ => VolumeKind::Other,
            },
        })
    }

    fn network_volume(unc: &str) -> (Volume, PathBuf) {
        let unc = unc.trim_end_matches('\\');
        let vol =
            Volume { key: format!("unc:{}", unc.to_lowercase()), label: unc.to_string(), kind: VolumeKind::Network };
        (vol, PathBuf::from(format!("{unc}\\")))
    }

    pub fn volume_of(path: &Path) -> std::io::Result<(Volume, PathBuf)> {
        let root = volume_root(path)?;
        let dtype = drive_type(&root);
        if dtype == DRIVE_REMOTE {
            let s = root.to_string_lossy();
            if s.starts_with(r"\\") {
                return Ok(network_volume(&s));
            }
            // A mapped drive: key it by the share so it survives remapping,
            // but keep the drive-letter root for splitting the path.
            if let Some(unc) = mapped_unc(&root) {
                let (vol, _) = network_volume(&unc);
                return Ok((vol, root));
            }
        }
        Ok((local_volume(&root, dtype)?, root))
    }

    pub fn mounted_local() -> HashMap<String, PathBuf> {
        let mut out = HashMap::new();
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = PathBuf::from(format!("{}:\\", (b'A' + i as u8) as char));
            let dtype = drive_type(&root);
            if dtype == DRIVE_REMOTE {
                continue;
            }
            // An empty card reader slot fails here, which is what we want.
            if let Ok(vol) = local_volume(&root, dtype) {
                out.insert(vol.key, root);
            }
        }
        out
    }

    pub fn root_from_key(key: &str) -> Option<PathBuf> {
        key.strip_prefix("unc:").map(|unc| PathBuf::from(format!("{unc}\\")))
    }
}

#[cfg(not(windows))]
mod sys {
    //! Development fallback: the whole filesystem is one volume. The app only
    //! ships for Windows, but this keeps the core testable everywhere.
    use super::{Volume, VolumeKind};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    const KEY: &str = "path:/";

    fn root_volume() -> Volume {
        Volume { key: KEY.into(), label: "/".into(), kind: VolumeKind::Fixed }
    }

    pub fn volume_of(_path: &Path) -> std::io::Result<(Volume, PathBuf)> {
        Ok((root_volume(), PathBuf::from("/")))
    }

    pub fn mounted_local() -> HashMap<String, PathBuf> {
        HashMap::from([(KEY.to_string(), PathBuf::from("/"))])
    }

    pub fn root_from_key(_key: &str) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_use_forward_slashes() {
        let rel: PathBuf = ["Photos", "2015", "Summer"].iter().collect();
        assert_eq!(rel_to_string(&rel), "Photos/2015/Summer");
        assert_eq!(join(Path::new("base"), "a/b"), Path::new("base").join("a").join("b"));
        assert_eq!(join(Path::new("base"), ""), Path::new("base"));
    }

    #[test]
    fn locates_a_temp_folder() {
        let dir = tempfile::tempdir().unwrap();
        let loc = locate(dir.path()).unwrap();
        assert_eq!(join(&loc.root, &loc.rel), std::path::absolute(dir.path()).unwrap());
        let roots = resolve(std::slice::from_ref(&loc.volume.key));
        assert_eq!(roots.get(&loc.volume.key), Some(&loc.root));
    }
}
