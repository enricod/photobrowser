use std::cmp::Ordering;
use std::collections::HashSet;

use crate::media::{FileKind, Item, ItemId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RejectFilter {
    ShowAll,
    #[default]
    HideRejects,
    OnlyRejects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeFilter {
    #[default]
    All,
    /// Items containing a RAW file.
    Raw,
    /// Items containing a JPEG file.
    Jpeg,
    /// PNG and TIFF.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    CaptureTime,
    FileName,
}

#[derive(Debug, Clone, Default)]
pub struct FilterState {
    /// 0 = show all ratings. Ignored for `OnlyRejects` and for rejected items.
    pub min_rating: u8,
    pub reject: RejectFilter,
    pub kind: TypeFilter,
    /// Items re-rated since the user last navigated: always shown.
    pub pinned: HashSet<ItemId>,
}

impl FilterState {
    pub fn matches(&self, item: &Item) -> bool {
        if self.pinned.contains(&item.id) {
            return true;
        }
        let has = |k: FileKind| item.files.iter().any(|f| f.kind == k);
        let kind_ok = match self.kind {
            TypeFilter::All => true,
            TypeFilter::Raw => has(FileKind::Raw),
            TypeFilter::Jpeg => has(FileKind::Jpeg),
            TypeFilter::Other => has(FileKind::Png) || has(FileKind::Tiff),
        };
        if !kind_ok {
            return false;
        }
        match self.reject {
            RejectFilter::OnlyRejects => item.rejected,
            RejectFilter::HideRejects => !item.rejected && item.rating >= self.min_rating,
            RejectFilter::ShowAll => item.rejected || item.rating >= self.min_rating,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortState {
    pub key: SortKey,
    pub descending: bool,
}

/// Total order: key, then file name, then id. `descending` reverses the whole order.
pub fn compare(a: &Item, b: &Item, sort: SortState) -> Ordering {
    let by_name = || a.sort_name.cmp(&b.sort_name);
    let ord = match sort.key {
        SortKey::CaptureTime => a.sort_time.cmp(&b.sort_time).then_with(by_name),
        SortKey::FileName => by_name(),
    }
    .then(a.id.cmp(&b.id));
    if sort.descending { ord.reverse() } else { ord }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::pairing::group_into_items;
    use crate::media::MediaFile;
    use std::path::{Path, PathBuf};

    fn item(paths: &[&str], rating: u8, rejected: bool) -> Item {
        let files = paths
            .iter()
            .map(|p| MediaFile {
                path: PathBuf::from(p),
                kind: FileKind::from_path(Path::new(p)).unwrap(),
                size: 1,
                mtime_ns: 0,
            })
            .collect();
        let mut i = group_into_items(files).remove(0);
        i.rating = rating;
        i.rejected = rejected;
        i
    }

    #[test]
    fn default_hides_rejects() {
        let f = FilterState::default();
        assert!(f.matches(&item(&["/p/a.jpg"], 0, false)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 5, true)));
    }

    #[test]
    fn min_rating_filters() {
        let f = FilterState { min_rating: 3, ..Default::default() };
        assert!(!f.matches(&item(&["/p/a.jpg"], 2, false)));
        assert!(f.matches(&item(&["/p/a.jpg"], 3, false)));
    }

    #[test]
    fn only_rejects_ignores_min_rating() {
        let f = FilterState { min_rating: 5, reject: RejectFilter::OnlyRejects, ..Default::default() };
        assert!(f.matches(&item(&["/p/a.jpg"], 0, true)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 5, false)));
    }

    #[test]
    fn show_all_includes_rejects() {
        let f = FilterState { min_rating: 4, reject: RejectFilter::ShowAll, ..Default::default() };
        assert!(f.matches(&item(&["/p/a.jpg"], 0, true)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 1, false)));
    }

    #[test]
    fn type_filters_match_pairs_by_member() {
        let pair = item(&["/p/a.CR3", "/p/a.JPG"], 0, false);
        let png = item(&["/p/b.png"], 0, false);
        let raw = FilterState { kind: TypeFilter::Raw, ..Default::default() };
        let jpeg = FilterState { kind: TypeFilter::Jpeg, ..Default::default() };
        let other = FilterState { kind: TypeFilter::Other, ..Default::default() };
        assert!(raw.matches(&pair) && jpeg.matches(&pair) && !other.matches(&pair));
        assert!(!raw.matches(&png) && !jpeg.matches(&png) && other.matches(&png));
    }

    #[test]
    fn pinned_items_bypass_filters() {
        let mut i = item(&["/p/a.jpg"], 1, true);
        i.id = 7;
        let mut f = FilterState { min_rating: 5, ..Default::default() };
        assert!(!f.matches(&i));
        f.pinned.insert(7);
        assert!(f.matches(&i));
    }

    #[test]
    fn sorts_by_capture_time_then_name() {
        let mut a = item(&["/p/b.jpg"], 0, false);
        let mut b = item(&["/p/a.jpg"], 0, false);
        a.sort_time = 10;
        b.sort_time = 10;
        let s = SortState::default();
        assert_eq!(compare(&a, &b, s), Ordering::Greater); // same time → a.jpg first
        b.sort_time = 20;
        assert_eq!(compare(&a, &b, s), Ordering::Less);
    }

    #[test]
    fn filename_sort_is_case_insensitive_and_reversible() {
        let a = item(&["/p/B.jpg"], 0, false);
        let b = item(&["/p/a.jpg"], 0, false);
        let asc = SortState { key: SortKey::FileName, descending: false };
        let desc = SortState { key: SortKey::FileName, descending: true };
        assert_eq!(compare(&a, &b, asc), Ordering::Greater);
        assert_eq!(compare(&a, &b, desc), Ordering::Less);
    }
}
