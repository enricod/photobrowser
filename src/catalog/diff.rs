use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::media::{Item, ItemId};

#[derive(Debug)]
pub struct ItemUpdate {
    /// The rescanned item carrying its existing id.
    pub item: Item,
    /// The display file's size or mtime changed: the thumbnail must be reloaded.
    pub display_changed: bool,
}

#[derive(Debug, Default)]
pub struct ItemDiff {
    pub removed: Vec<ItemId>,
    pub added: Vec<Item>,
    pub updated: Vec<ItemUpdate>,
}

fn key(item: &Item) -> Vec<PathBuf> {
    item.files.iter().map(|f| f.path.clone()).collect()
}

pub fn diff_items(old: &[Item], new: Vec<Item>, mut alloc_id: impl FnMut() -> ItemId) -> ItemDiff {
    let old_by_key: HashMap<Vec<PathBuf>, &Item> = old.iter().map(|i| (key(i), i)).collect();
    let mut seen = HashSet::new();
    let mut diff = ItemDiff::default();
    for mut item in new {
        match old_by_key.get(&key(&item)) {
            Some(old_item) => {
                seen.insert(old_item.id);
                item.id = old_item.id;
                if item != **old_item {
                    let display_changed = item.display_file() != old_item.display_file();
                    diff.updated.push(ItemUpdate { item, display_changed });
                }
            }
            None => {
                item.id = alloc_id();
                diff.added.push(item);
            }
        }
    }
    diff.removed = old.iter().filter(|i| !seen.contains(&i.id)).map(|i| i.id).collect();
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::pairing::group_into_items;
    use crate::media::{FileKind, MediaFile};
    use std::path::Path;

    fn mf(p: &str, mtime_ns: i64) -> MediaFile {
        MediaFile {
            path: PathBuf::from(p),
            kind: FileKind::from_path(Path::new(p)).unwrap(),
            size: 1,
            mtime_ns,
        }
    }

    fn with_ids(mut items: Vec<Item>) -> Vec<Item> {
        for (n, i) in items.iter_mut().enumerate() {
            i.id = n as ItemId + 1;
        }
        items
    }

    fn counter() -> impl FnMut() -> ItemId {
        let mut next = 100;
        move || {
            next += 1;
            next
        }
    }

    #[test]
    fn unchanged_items_produce_empty_diff() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 0)]), counter());
        assert!(d.added.is_empty() && d.removed.is_empty() && d.updated.is_empty());
    }

    #[test]
    fn new_file_is_added_with_fresh_id() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let new = group_into_items(vec![mf("/p/a.jpg", 0), mf("/p/b.jpg", 0)]);
        let d = diff_items(&old, new, counter());
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].id, 101);
    }

    #[test]
    fn missing_file_is_removed() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0), mf("/p/b.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 0)]), counter());
        assert_eq!(d.removed, vec![2]);
    }

    #[test]
    fn rating_change_is_update_without_display_change() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let mut new = group_into_items(vec![mf("/p/a.jpg", 0)]);
        new[0].rating = 4;
        let d = diff_items(&old, new, counter());
        assert_eq!(d.updated.len(), 1);
        assert_eq!(d.updated[0].item.id, 1);
        assert!(!d.updated[0].display_changed);
    }

    #[test]
    fn mtime_change_marks_display_changed() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 7)]), counter());
        assert!(d.updated[0].display_changed);
    }

    #[test]
    fn single_becoming_pair_is_remove_plus_add() {
        let old = with_ids(group_into_items(vec![mf("/p/a.CR3", 0)]));
        let new = group_into_items(vec![mf("/p/a.CR3", 0), mf("/p/a.JPG", 0)]);
        let d = diff_items(&old, new, counter());
        assert_eq!(d.removed, vec![1]);
        assert_eq!(d.added.len(), 1);
        assert!(d.added[0].is_pair());
    }
}
