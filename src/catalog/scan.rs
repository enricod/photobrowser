use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::media::{FileKind, MediaFile, mtime_ns};

/// What a folder listing found. Only a file that is really gone may be treated as removed:
/// a transient error (network share, USB hiccup) must not look like a deletion.
#[derive(Debug, Default)]
pub struct Listing {
    pub files: Vec<MediaFile>,
    /// Media files whose metadata could not be read for a reason other than NotFound.
    pub unreadable: Vec<PathBuf>,
    /// Some directory entry could not be read, so any known file may be missing from `files`.
    pub incomplete: bool,
}

/// Lists supported, non-hidden regular files directly inside `folder` (symlinks followed).
pub fn list_media_files(folder: &Path) -> io::Result<Listing> {
    let mut out = Listing::default();
    for entry in fs::read_dir(folder)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("scan of {}: unreadable entry: {e}", folder.display());
                out.incomplete = true;
                continue;
            }
        };
        if entry.file_name().as_bytes().first() == Some(&b'.') {
            continue;
        }
        let path = entry.path();
        let Some(kind) = FileKind::from_path(&path) else {
            continue;
        };
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            // Gone since the listing, or a dangling symlink: really not there.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!("scan: can't read {}: {e}", path.display());
                out.unreadable.push(path);
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        out.files.push(MediaFile {
            path,
            kind,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        });
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
            .files
            .into_iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "b.CR3"]);
    }

    #[test]
    fn unreadable_file_is_reported_and_dangling_link_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let looped = dir.path().join("loop.jpg");
        std::os::unix::fs::symlink(&looped, &looped).unwrap(); // metadata: ELOOP
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("gone.jpg"))
            .unwrap();
        let listing = list_media_files(dir.path()).unwrap();
        assert!(listing.files.is_empty());
        assert_eq!(listing.unreadable, [looped]);
        assert!(!listing.incomplete);
    }

    #[test]
    fn missing_folder_is_error() {
        assert!(list_media_files(Path::new("/definitely/not/here")).is_err());
    }
}
