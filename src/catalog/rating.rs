use crate::media::Item;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingChange {
    /// 0 clears the rating. Setting stars always un-rejects (XMP has one field for both).
    Set(u8),
    ToggleReject,
}

pub fn apply(item: &mut Item, change: RatingChange) {
    match change {
        RatingChange::Set(n) => {
            item.rating = n.min(5);
            item.rejected = false;
        }
        RatingChange::ToggleReject => item.rejected = !item.rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{FileKind, MediaFile};
    use std::path::PathBuf;

    fn item() -> Item {
        Item::new(vec![MediaFile {
            path: PathBuf::from("/p/a.jpg"),
            kind: FileKind::Jpeg,
            size: 1,
            mtime_ns: 0,
        }])
    }

    #[test]
    fn set_rating_clamps_and_unrejects() {
        let mut i = item();
        i.rejected = true;
        apply(&mut i, RatingChange::Set(9));
        assert_eq!((i.rating, i.rejected), (5, false));
    }

    #[test]
    fn toggle_reject_keeps_stars() {
        let mut i = item();
        apply(&mut i, RatingChange::Set(3));
        apply(&mut i, RatingChange::ToggleReject);
        assert_eq!((i.rating, i.rejected), (3, true));
        apply(&mut i, RatingChange::ToggleReject);
        assert_eq!((i.rating, i.rejected), (3, false));
    }
}
