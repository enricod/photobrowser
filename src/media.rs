use std::path::{Path, PathBuf};

pub type ItemId = u64;

const RAW_EXTS: &[&str] = &["cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "dng", "pef", "srw"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Jpeg,
    Png,
    Tiff,
    Raw,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Option<FileKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(FileKind::Jpeg),
            "png" => Some(FileKind::Png),
            "tif" | "tiff" => Some(FileKind::Tiff),
            e if RAW_EXTS.contains(&e) => Some(FileKind::Raw),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ns: i64,
}

/// One browsable unit: a single file, or a RAW+JPEG pair (RAW first).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub base_name: String,
    pub files: Vec<MediaFile>,
    /// Capture time (unix seconds), falling back to the display file's mtime.
    pub sort_time: i64,
    /// Lower-cased display file name, precomputed for sorting.
    pub sort_name: String,
    pub rating: u8,
    pub rejected: bool,
}

impl Item {
    pub fn new(files: Vec<MediaFile>) -> Item {
        assert!(!files.is_empty(), "an item needs at least one file");
        let base_name = files[0]
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut item = Item {
            id: 0,
            base_name,
            files,
            sort_time: 0,
            sort_name: String::new(),
            rating: 0,
            rejected: false,
        };
        item.sort_name = item.display_name().to_lowercase();
        item
    }

    /// The file shown on screen: the JPEG of a pair, otherwise the only file.
    pub fn display_file(&self) -> &MediaFile {
        self.files
            .iter()
            .find(|f| f.kind == FileKind::Jpeg)
            .unwrap_or(&self.files[0])
    }

    pub fn raw_file(&self) -> Option<&MediaFile> {
        self.files.iter().find(|f| f.kind == FileKind::Raw)
    }

    pub fn is_pair(&self) -> bool {
        self.files.len() > 1
    }

    pub fn display_name(&self) -> String {
        file_name(&self.display_file().path)
    }

    /// All file names, e.g. "IMG_1.CR3 + IMG_1.JPG".
    pub fn file_names(&self) -> String {
        self.files
            .iter()
            .map(|f| file_name(&f.path))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mf(p: &str) -> MediaFile {
        MediaFile {
            path: PathBuf::from(p),
            kind: FileKind::from_path(Path::new(p)).unwrap(),
            size: 1,
            mtime_ns: 0,
        }
    }

    #[test]
    fn classifies_extensions() {
        assert_eq!(FileKind::from_path(Path::new("a.JPG")), Some(FileKind::Jpeg));
        assert_eq!(FileKind::from_path(Path::new("a.jpeg")), Some(FileKind::Jpeg));
        assert_eq!(FileKind::from_path(Path::new("a.png")), Some(FileKind::Png));
        assert_eq!(FileKind::from_path(Path::new("a.TIFF")), Some(FileKind::Tiff));
        assert_eq!(FileKind::from_path(Path::new("a.NEF")), Some(FileKind::Raw));
        assert_eq!(FileKind::from_path(Path::new("a.cr3")), Some(FileKind::Raw));
        assert_eq!(FileKind::from_path(Path::new("a.txt")), None);
        assert_eq!(FileKind::from_path(Path::new("noext")), None);
    }

    #[test]
    fn display_file_prefers_jpeg() {
        let item = Item::new(vec![mf("/p/IMG_1.CR3"), mf("/p/IMG_1.JPG")]);
        assert_eq!(item.display_file().path, PathBuf::from("/p/IMG_1.JPG"));
        assert_eq!(item.raw_file().unwrap().path, PathBuf::from("/p/IMG_1.CR3"));
        assert!(item.is_pair());
        assert_eq!(item.sort_name, "img_1.jpg");
        assert_eq!(item.file_names(), "IMG_1.CR3 + IMG_1.JPG");
        assert_eq!(item.base_name, "IMG_1");
    }
}
