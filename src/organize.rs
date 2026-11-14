//! Sorting photos into dated folders: which folder each item goes to.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate};

/// The folder layout under the chosen root. One for now; meant to become a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrganizePattern {
    /// `2026/2026-11-14`
    #[default]
    YearThenDay,
}

impl OrganizePattern {
    /// How the layout is shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            OrganizePattern::YearThenDay => "YYYY / YYYY-MM-DD",
        }
    }

    /// The folder, relative to the root, for photos shot on `date`.
    pub fn subdir(self, date: NaiveDate) -> PathBuf {
        match self {
            OrganizePattern::YearThenDay => PathBuf::from(date.format("%Y").to_string())
                .join(date.format("%Y-%m-%d").to_string()),
        }
    }
}

/// An item's shot date: the earliest capture time of its files (a RAW+JPEG pair stays
/// together). Capture times are EXIF wall-clock times stored as if UTC, so the date is read
/// back in UTC to get the camera's own calendar day. `None` if no file has one.
pub fn shot_date(capture_times: impl IntoIterator<Item = Option<i64>>) -> Option<NaiveDate> {
    let earliest = capture_times.into_iter().flatten().min()?;
    DateTime::from_timestamp(earliest, 0).map(|t| t.date_naive())
}

/// Where each item goes, in input order; `None` for items without a shot date.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub destinations: Vec<Option<PathBuf>>,
    /// Distinct destination folders.
    pub folders: usize,
    pub undated: usize,
}

pub fn plan(dates: &[Option<NaiveDate>], root: &Path, pattern: OrganizePattern) -> Plan {
    let destinations: Vec<Option<PathBuf>> = dates
        .iter()
        .map(|d| d.map(|d| root.join(pattern.subdir(d))))
        .collect();
    let folders = destinations.iter().flatten().collect::<HashSet<_>>().len();
    let undated = dates.iter().filter(|d| d.is_none()).count();
    Plan {
        destinations,
        folders,
        undated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        day(y, m, d)
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn year_then_day_layout() {
        let p = OrganizePattern::YearThenDay;
        assert_eq!(
            p.subdir(day(2026, 11, 14)),
            PathBuf::from("2026/2026-11-14")
        );
        assert_eq!(p.subdir(day(2024, 2, 29)), PathBuf::from("2024/2024-02-29"));
        assert_eq!(p.subdir(day(2025, 1, 5)), PathBuf::from("2025/2025-01-05"));
    }

    #[test]
    fn shot_date_is_the_camera_day_of_the_earliest_file() {
        // Late evening and just past midnight: each stays on its own calendar day.
        assert_eq!(
            shot_date([Some(ts(2025, 12, 31, 23, 59))]),
            Some(day(2025, 12, 31))
        );
        assert_eq!(
            shot_date([Some(ts(2026, 1, 1, 0, 1))]),
            Some(day(2026, 1, 1))
        );
        // A pair uses its earlier time.
        let pair = [Some(ts(2026, 1, 1, 0, 1)), Some(ts(2025, 12, 31, 23, 59))];
        assert_eq!(shot_date(pair), Some(day(2025, 12, 31)));
        assert_eq!(
            shot_date([None, Some(ts(2026, 3, 2, 10, 0))]),
            Some(day(2026, 3, 2))
        );
        assert_eq!(shot_date([None, None]), None);
        assert_eq!(shot_date([]), None);
    }

    #[test]
    fn plan_assigns_folders_and_counts_undated() {
        let root = Path::new("/photos");
        let dates = [
            Some(day(2026, 11, 14)),
            None,
            Some(day(2026, 11, 14)),
            Some(day(2025, 6, 1)),
        ];
        let p = plan(&dates, root, OrganizePattern::YearThenDay);
        assert_eq!(
            p.destinations,
            [
                Some(PathBuf::from("/photos/2026/2026-11-14")),
                None,
                Some(PathBuf::from("/photos/2026/2026-11-14")),
                Some(PathBuf::from("/photos/2025/2025-06-01")),
            ]
        );
        assert_eq!((p.folders, p.undated), (2, 1));
    }
}
