use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::media::{mtime_ns, FileKind, MediaFile};

/// Lists supported, non-hidden regular files directly inside `folder` (symlinks followed).
pub fn list_media_files(folder: &Path) -> io::Result<Vec<MediaFile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(folder)? {
        let Ok(entry) = entry else { continue };
        if entry.file_name().as_bytes().first() == Some(&b'.') {
            continue;
        }
        let path = entry.path();
        let Some(kind) = FileKind::from_path(&path) else { continue };
        let Ok(meta) = fs::metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(MediaFile { path, kind, size: meta.len(), mtime_ns: mtime_ns(&meta) });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_supported_visible_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.jpg", "b.CR3", ".hidden.jpg", "notes.txt", "c.png.xmp"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("sub.jpg")).unwrap();
        let mut names: Vec<_> = list_media_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "b.CR3"]);
    }

    #[test]
    fn missing_folder_is_error() {
        assert!(list_media_files(Path::new("/definitely/not/here")).is_err());
    }
}
