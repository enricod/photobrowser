use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::media::{FileKind, Item, MediaFile};

/// Groups files into items. Exactly one RAW + one JPEG with the same folder and
/// case-insensitive stem become a pair (RAW first); everything else is single.
/// Returned items have `id == 0`.
pub fn group_into_items(files: Vec<MediaFile>) -> Vec<Item> {
    let mut groups: BTreeMap<(PathBuf, Vec<u8>), Vec<MediaFile>> = BTreeMap::new();
    for f in files {
        let dir = f.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = f
            .path
            .file_stem()
            .map(|s| s.as_bytes().to_ascii_lowercase())
            .unwrap_or_default();
        groups.entry((dir, stem)).or_default().push(f);
    }

    let mut items = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| a.path.cmp(&b.path));
        let raws = group.iter().filter(|f| f.kind == FileKind::Raw).count();
        let jpegs = group.iter().filter(|f| f.kind == FileKind::Jpeg).count();
        if group.len() == 2 && raws == 1 && jpegs == 1 {
            group.sort_by_key(|f| f.kind != FileKind::Raw);
            items.push(Item::new(group));
        } else {
            items.extend(group.into_iter().map(|f| Item::new(vec![f])));
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn mf(p: &str) -> MediaFile {
        mf_path(PathBuf::from(p))
    }

    fn mf_path(path: PathBuf) -> MediaFile {
        let kind = FileKind::from_path(&path).unwrap();
        MediaFile {
            path,
            kind,
            size: 1,
            mtime_ns: 0,
        }
    }

    fn names(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.file_names()).collect()
    }

    #[test]
    fn pairs_raw_and_jpeg_with_same_stem() {
        let items = group_into_items(vec![mf("/p/IMG_1.JPG"), mf("/p/IMG_1.CR3")]);
        assert_eq!(names(&items), ["IMG_1.CR3 + IMG_1.JPG"]);
        assert_eq!(items[0].files[0].kind, FileKind::Raw);
    }

    #[test]
    fn pairs_case_insensitively() {
        let items = group_into_items(vec![mf("/p/img_001.CR3"), mf("/p/IMG_001.jpg")]);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_pair());
    }

    #[test]
    fn orphans_are_single_items() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/B.JPG")]);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.is_pair()));
    }

    #[test]
    fn duplicate_jpeg_variants_are_not_paired() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.jpg"), mf("/p/A.JPG")]);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn png_does_not_pair_with_raw() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.png")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn two_raws_do_not_pair() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.DNG")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn same_stem_in_different_dirs_not_paired() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/q/A.JPG")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn non_utf8_names_pair() {
        let dir = PathBuf::from("/p");
        let raw = dir.join(OsStr::from_bytes(b"\xffphoto.cr3"));
        let jpg = dir.join(OsStr::from_bytes(b"\xffPHOTO.JPG"));
        let items = group_into_items(vec![mf_path(raw), mf_path(jpg)]);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_pair());
    }
}
