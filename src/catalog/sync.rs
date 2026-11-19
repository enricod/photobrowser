use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::catalog::pairing::group_into_items;
use crate::catalog::scan::list_media_files;
use crate::exif;
use crate::index::FileRecord;
use crate::media::{FileKind, Item, MediaFile};
use crate::xmp::{self, XmpRating};

#[derive(Debug, Default)]
pub struct ScanResult {
    /// Items with `id == 0`; ids are assigned on the main thread.
    pub items: Vec<Item>,
    /// Records that are new or changed and must be written to the index.
    pub upserts: Vec<FileRecord>,
    /// Paths known to the index that no longer exist.
    pub removed: Vec<PathBuf>,
    /// The folder's non-hidden subfolders, sorted by name.
    pub folders: Vec<PathBuf>,
}

/// What a folder's listing shows before any file is opened: its items (ratings from the
/// index, new files dated by mtime) and subfolders. Sent ahead of the full scan so a slow
/// folder (a network share) can be shown while capture dates and sidecars are read.
#[derive(Debug, Default)]
pub struct Listed {
    /// Items with `id == 0`.
    pub items: Vec<Item>,
    pub folders: Vec<PathBuf>,
}

/// How many files are read at once for capture dates and sidecars. Parsing is the cost on a
/// local disk (≈4× faster with 8 threads); a GVFS mount serves one request at a time, so
/// there it neither helps nor hurts.
const READ_THREADS: usize = 8;

pub fn scan_folder(folder: &Path, known: &HashMap<PathBuf, FileRecord>) -> io::Result<ScanResult> {
    scan_folder_with(folder, known, |_| {})
}

/// Like `scan_folder`, but calls `on_listed` once the folder is listed and before any file
/// is opened, if the listing found a file the index doesn't know as it is now.
pub fn scan_folder_with(
    folder: &Path,
    known: &HashMap<PathBuf, FileRecord>,
    on_listed: impl FnOnce(Listed),
) -> io::Result<ScanResult> {
    let listing = list_media_files(folder)?;
    let folders = listing.folders;
    let mut files = listing.files;
    // Known files the listing could not check are kept exactly as recorded (shown, rating
    // intact, not removed) until a later scan can read them.
    let seen: HashSet<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let unreadable: HashSet<&PathBuf> = listing.unreadable.iter().collect();
    files.extend(
        known
            .values()
            .filter(|r| !seen.contains(&r.path))
            .filter(|r| listing.incomplete || unreadable.contains(&r.path))
            .filter_map(as_recorded),
    );

    if files.iter().any(|f| !is_unchanged(f, known)) {
        on_listed(Listed {
            items: preliminary_items(&files, known),
            folders: folders.clone(),
        });
    }

    let built = par_map(&files, READ_THREADS, |f| build_record(f, known));
    let mut records: HashMap<PathBuf, FileRecord> = HashMap::with_capacity(files.len());
    let mut changed: HashSet<PathBuf> = HashSet::new();
    for (rec, was_changed) in built {
        if was_changed {
            changed.insert(rec.path.clone());
        }
        records.insert(rec.path.clone(), rec);
    }

    let mut items = group_into_items(files);
    for item in &mut items {
        changed.extend(reconcile_item(item, &mut records));
    }
    let removed = known
        .keys()
        .filter(|p| !records.contains_key(*p))
        .cloned()
        .collect();
    let upserts = changed
        .into_iter()
        .filter_map(|p| records.remove(&p))
        .collect();
    Ok(ScanResult {
        items,
        upserts,
        removed,
        folders,
    })
}

fn is_unchanged(f: &MediaFile, known: &HashMap<PathBuf, FileRecord>) -> bool {
    known
        .get(&f.path)
        .is_some_and(|r| r.size == f.size && r.mtime_ns == f.mtime_ns)
}

/// Items from the listing and the index alone: no file is opened.
fn preliminary_items(files: &[MediaFile], known: &HashMap<PathBuf, FileRecord>) -> Vec<Item> {
    let mut records: HashMap<PathBuf, FileRecord> = files
        .iter()
        .map(|f| {
            let rec = match known.get(&f.path) {
                Some(old) if old.size == f.size && old.mtime_ns == f.mtime_ns => old.clone(),
                // Its old capture time may be wrong now; its rating still holds.
                Some(old) => FileRecord {
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                    capture_time: None,
                    ..old.clone()
                },
                None => FileRecord::new(f.path.clone(), f.size, f.mtime_ns),
            };
            (f.path.clone(), rec)
        })
        .collect();
    let mut items = group_into_items(files.to_vec());
    for item in &mut items {
        reconcile_item(item, &mut records);
    }
    items
}

/// The file's record after this scan, and whether it differs from the index's.
fn build_record(f: &MediaFile, known: &HashMap<PathBuf, FileRecord>) -> (FileRecord, bool) {
    let (mut rec, mut changed) = match known.get(&f.path) {
        Some(old) if old.size == f.size && old.mtime_ns == f.mtime_ns => (old.clone(), false),
        Some(old) => (
            FileRecord {
                size: f.size,
                mtime_ns: f.mtime_ns,
                capture_time: exif::capture_time(&f.path),
                ..old.clone()
            },
            true,
        ),
        None => (
            FileRecord {
                capture_time: exif::capture_time(&f.path),
                ..FileRecord::new(f.path.clone(), f.size, f.mtime_ns)
            },
            true,
        ),
    };
    if import_sidecar(&mut rec) {
        changed = true;
    }
    (rec, changed)
}

/// `f` applied to every input on up to `threads` threads; results in input order.
fn par_map<T: Sync, R: Send>(input: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = threads.min(input.len());
    if threads <= 1 {
        return input.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(x) = input.get(i) else { break };
                        done.push((i, f(x)));
                    }
                    done
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    out.sort_unstable_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// The file as last recorded in the index, for keeping one the scan could not read.
fn as_recorded(rec: &FileRecord) -> Option<MediaFile> {
    Some(MediaFile {
        path: rec.path.clone(),
        kind: FileKind::from_path(&rec.path)?,
        size: rec.size,
        mtime_ns: rec.mtime_ns,
    })
}

/// Imports the sidecar's rating when it changed since the last sync.
/// Returns true if `rec` was modified.
///
/// A sidecar that cannot be read leaves `rec` untouched, including `xmp_synced_at`: it is
/// retried on the next scan, and its stale rating cannot win `reconcile_item` and be pushed
/// onto the other file of a pair.
fn import_sidecar(rec: &mut FileRecord) -> bool {
    if rec.xmp_dirty {
        return false; // a local change is waiting to be written: local wins
    }
    let Some(mtime) = xmp::sidecar_mtime_ns(&rec.path) else {
        return false;
    };
    if rec.xmp_synced_at.is_some_and(|t| t >= mtime) {
        return false;
    }
    match xmp::read_rating(&rec.path) {
        Ok(Some(XmpRating::Stars(n))) => {
            rec.rating = n;
            rec.rejected = false;
        }
        Ok(Some(XmpRating::Rejected)) => rec.rejected = true,
        // No rating in it (e.g. written by a geotagger): it says nothing about the rating, so
        // it must not mark the record as freshly synced and win `reconcile_item` for a pair.
        Ok(None) => return false,
        Err(e) => {
            eprintln!(
                "couldn't read sidecar {}: {e}",
                xmp::sidecar_path(&rec.path).display()
            );
            return false;
        }
    }
    rec.xmp_synced_at = Some(mtime);
    true
}

/// Fills the item's rating, reject flag and sort time from its records and makes
/// the records of a pair agree. Returns paths whose records were changed.
pub fn reconcile_item(item: &mut Item, records: &mut HashMap<PathBuf, FileRecord>) -> Vec<PathBuf> {
    let mut best: Option<(bool, i64, u8, bool)> = None;
    for f in &item.files {
        if let Some(r) = records.get(&f.path) {
            let key = (r.xmp_dirty, r.xmp_synced_at.unwrap_or(i64::MIN));
            if best.is_none_or(|(d, t, _, _)| key > (d, t)) {
                best = Some((key.0, key.1, r.rating, r.rejected));
            }
        }
    }
    let (rating, rejected) = best.map(|(_, _, r, j)| (r, j)).unwrap_or((0, false));
    item.rating = rating;
    item.rejected = rejected;

    let mut changed = Vec::new();
    for f in &item.files {
        if let Some(rec) = records.get_mut(&f.path)
            && (rec.rating != rating || rec.rejected != rejected)
        {
            rec.rating = rating;
            rec.rejected = rejected;
            rec.xmp_dirty = true;
            changed.push(f.path.clone());
        }
    }

    item.sort_time = item
        .files
        .iter()
        .filter_map(|f| records.get(&f.path).and_then(|r| r.capture_time))
        .min()
        .unwrap_or(item.display_file().mtime_ns / 1_000_000_000);
    changed
}

/// Builds items from index records alone (no disk access) for an instant first paint.
pub fn items_from_records(records: &HashMap<PathBuf, FileRecord>) -> Vec<Item> {
    let files: Vec<MediaFile> = records
        .values()
        .filter_map(|r| {
            Some(MediaFile {
                path: r.path.clone(),
                kind: FileKind::from_path(&r.path)?,
                size: r.size,
                mtime_ns: r.mtime_ns,
            })
        })
        .collect();
    let mut items = group_into_items(files);
    let mut scratch = records.clone();
    for item in &mut items {
        reconcile_item(item, &mut scratch);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp;
    use std::fs;

    fn touch(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn known_from(result: &ScanResult) -> HashMap<PathBuf, FileRecord> {
        result
            .upserts
            .iter()
            .map(|r| (r.path.clone(), r.clone()))
            .collect()
    }

    #[test]
    fn new_folder_scan_creates_records() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        touch(dir.path(), "b.CR3", b"x");
        touch(dir.path(), "b.JPG", b"x");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.upserts.len(), 3);
        assert!(r.removed.is_empty());
    }

    #[test]
    fn unchanged_files_are_not_upserted() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert!(second.upserts.is_empty());
        assert_eq!(second.items.len(), 1);
    }

    #[test]
    fn modified_file_is_upserted_keeping_rating() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 4;
        fs::write(&a, b"longer content").unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.upserts.len(), 1);
        assert_eq!(second.upserts[0].size, 14);
        assert_eq!(second.upserts[0].rating, 4);
        assert_eq!(second.items[0].rating, 4);
    }

    #[test]
    fn unreadable_file_is_kept_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 4;
        // Replace the file with a self-referencing symlink: metadata fails with ELOOP, a
        // stand-in for a transient I/O error.
        fs::remove_file(&a).unwrap();
        std::os::unix::fs::symlink(&a, &a).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert!(second.removed.is_empty(), "{:?}", second.removed);
        assert!(second.upserts.is_empty());
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].rating, 4);
    }

    #[test]
    fn scan_reports_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        fs::create_dir(dir.path().join("trip")).unwrap();
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!(r.folders, [dir.path().join("trip")]);
        assert_eq!(r.items.len(), 1);
    }

    #[test]
    fn removed_files_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        fs::remove_file(&a).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert_eq!(second.removed, vec![a]);
        assert!(second.items.is_empty());
    }

    #[test]
    fn ratingless_sidecar_is_not_marked_synced() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 3;
        fs::write(xmp::sidecar_path(&a), xmp::EMPTY_PACKET).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert!(second.upserts.is_empty());
        assert_eq!(second.items[0].rating, 3);
    }

    #[test]
    fn ratingless_sidecar_does_not_override_pair_partner() {
        let dir = tempfile::tempdir().unwrap();
        let raw = touch(dir.path(), "b.CR3", b"x");
        let jpg = touch(dir.path(), "b.JPG", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        xmp::write_rating(&raw, 4, false).unwrap();
        let raw_synced = xmp::sidecar_mtime_ns(&raw).unwrap();
        let mut known = known_from(&first);
        {
            let r = known.get_mut(&raw).unwrap();
            r.rating = 4;
            r.xmp_synced_at = Some(raw_synced);
        }
        // A geotagger later writes a sidecar for the JPEG without any rating.
        let jpg_sidecar = xmp::sidecar_path(&jpg);
        fs::write(&jpg_sidecar, xmp::EMPTY_PACKET).unwrap();
        let later = std::time::UNIX_EPOCH
            + std::time::Duration::from_nanos(raw_synced as u64)
            + std::time::Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&jpg_sidecar)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 4);
        assert!(
            second.upserts.iter().all(|r| r.path != raw),
            "the RAW's real rating must not be rewritten"
        );
    }

    #[test]
    fn sidecar_newer_than_sync_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        xmp::write_rating(&a, 5, false).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert_eq!(second.items[0].rating, 5);
        assert_eq!(second.upserts[0].xmp_synced_at, xmp::sidecar_mtime_ns(&a));
    }

    #[test]
    fn dirty_record_ignores_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        let rec = known.get_mut(&a).unwrap();
        rec.rating = 2;
        rec.xmp_dirty = true;
        xmp::write_rating(&a, 5, false).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 2);
    }

    #[test]
    fn rejected_sidecar_keeps_stars() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 3;
        xmp::write_rating(&a, 0, true).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(
            (second.items[0].rating, second.items[0].rejected),
            (3, true)
        );
    }

    #[test]
    fn unreadable_sidecar_is_not_marked_synced() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        {
            let rec = known.get_mut(&a).unwrap();
            rec.rating = 3;
            rec.xmp_synced_at = None;
        }
        fs::write(xmp::sidecar_path(&a), b"definitely not xmp").unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert!(second.upserts.is_empty(), "record must not change");
        assert_eq!(second.items[0].rating, 3);
    }

    #[test]
    fn unreadable_sidecar_does_not_override_pair_partner() {
        let dir = tempfile::tempdir().unwrap();
        let raw = touch(dir.path(), "b.CR3", b"x");
        let jpg = touch(dir.path(), "b.JPG", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        xmp::write_rating(&jpg, 4, false).unwrap();
        let mut known = known_from(&first);
        let jpg_synced = xmp::sidecar_mtime_ns(&jpg);
        {
            let j = known.get_mut(&jpg).unwrap();
            j.rating = 4;
            j.xmp_synced_at = jpg_synced;
        }
        {
            // Stale catalog rating; its sidecar is newer than the JPEG's but unreadable.
            let r = known.get_mut(&raw).unwrap();
            r.rating = 1;
            r.xmp_synced_at = None;
        }
        fs::write(xmp::sidecar_path(&raw), b"definitely not xmp").unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 4);
        assert!(
            second.upserts.iter().all(|r| r.path != jpg),
            "the readable sidecar's record must not be rewritten"
        );
    }

    #[test]
    fn pair_disagreement_resolved_by_latest_sync() {
        let dir = tempfile::tempdir().unwrap();
        let raw = touch(dir.path(), "b.CR3", b"x");
        let jpg = touch(dir.path(), "b.JPG", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        {
            let r = known.get_mut(&raw).unwrap();
            r.rating = 2;
            r.xmp_synced_at = Some(10);
        }
        {
            let j = known.get_mut(&jpg).unwrap();
            j.rating = 4;
            j.xmp_synced_at = Some(20);
        }
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 4);
        let raw_up = second.upserts.iter().find(|r| r.path == raw).unwrap();
        assert_eq!((raw_up.rating, raw_up.xmp_dirty), (4, true));
    }

    #[test]
    fn sort_time_falls_back_to_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mtime_s = crate::media::mtime_ns(&fs::metadata(&a).unwrap()) / 1_000_000_000;
        assert_eq!(r.items[0].sort_time, mtime_s);
    }

    #[test]
    fn capture_time_is_used_for_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        crate::testutil::write_jpeg(&a, 8, 8, [1, 2, 3]);
        crate::testutil::set_exif(&a, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!(r.items[0].sort_time, 1_788_257_730);
    }

    #[test]
    fn malformed_sidecar_does_not_break_scan() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        fs::write(xmp::sidecar_path(&a), b"garbage").unwrap();
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!((r.items[0].rating, r.items[0].rejected), (0, false));
    }

    fn scan_listed(
        dir: &Path,
        known: &HashMap<PathBuf, FileRecord>,
    ) -> (Option<Listed>, ScanResult) {
        let mut listed = None;
        let r = scan_folder_with(dir, known, |l| listed = Some(l)).unwrap();
        (listed, r)
    }

    #[test]
    fn listing_is_reported_before_dates_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        crate::testutil::write_jpeg(&a, 8, 8, [1, 2, 3]);
        crate::testutil::set_exif(&a, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        touch(dir.path(), "b.CR3", b"x");
        touch(dir.path(), "b.JPG", b"x");
        fs::create_dir(dir.path().join("trip")).unwrap();
        let (listed, r) = scan_listed(dir.path(), &HashMap::new());
        let listed = listed.expect("new files: the listing is reported");
        assert_eq!(listed.items.len(), 2);
        assert_eq!(listed.folders, [dir.path().join("trip")]);
        // Not opened yet: dated by mtime.
        let mtime_s = crate::media::mtime_ns(&fs::metadata(&a).unwrap()) / 1_000_000_000;
        let la = listed
            .items
            .iter()
            .find(|i| i.display_file().path == a)
            .unwrap();
        assert_eq!(la.sort_time, mtime_s);
        let ra = r.items.iter().find(|i| i.display_file().path == a).unwrap();
        assert_eq!(ra.sort_time, 1_788_257_730);
    }

    #[test]
    fn listing_keeps_known_ratings() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 4;
        touch(dir.path(), "new.jpg", b"x");
        let (listed, _) = scan_listed(dir.path(), &known);
        let listed = listed.unwrap();
        let la = listed
            .items
            .iter()
            .find(|i| i.display_file().path == a)
            .unwrap();
        assert_eq!(la.rating, 4);
        assert_eq!(listed.items.len(), 2);
    }

    #[test]
    fn unchanged_folder_reports_no_listing() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let (listed, r) = scan_listed(dir.path(), &known_from(&first));
        assert!(listed.is_none());
        assert_eq!(r.items.len(), 1);
    }

    #[test]
    fn many_files_scan_like_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            let p = dir.path().join(format!("img{i:02}.jpg"));
            crate::testutil::write_jpeg(&p, 4, 4, [i as u8, 0, 0]);
            let date = format!("2026:01:{:02} 10:00:00", i % 28 + 1);
            crate::testutil::set_exif(&p, "Exif.Photo.DateTimeOriginal", &date);
            if i % 5 == 0 {
                xmp::write_rating(&p, (i % 6) as u8, false).unwrap();
            }
        }
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let files: Vec<MediaFile> = list_media_files(dir.path()).unwrap().files;
        let serial: HashMap<PathBuf, FileRecord> = files
            .iter()
            .map(|f| build_record(f, &HashMap::new()).0)
            .map(|rec| (rec.path.clone(), rec))
            .collect();
        assert_eq!(r.upserts.len(), 40);
        for rec in &r.upserts {
            assert_eq!(rec, &serial[&rec.path]);
        }
    }

    #[test]
    fn par_map_keeps_order() {
        let input: Vec<u32> = (0..1000).collect();
        let out = par_map(&input, 8, |x| x * 2);
        assert_eq!(out, input.iter().map(|x| x * 2).collect::<Vec<_>>());
        assert!(par_map(&[] as &[u32], 8, |x| *x).is_empty());
    }

    #[test]
    fn items_from_records_builds_pairs() {
        let mut records = HashMap::new();
        for p in ["/p/b.CR3", "/p/b.JPG", "/p/c.png"] {
            let mut r = FileRecord::new(PathBuf::from(p), 1, 5_000_000_000);
            r.rating = 3;
            records.insert(r.path.clone(), r);
        }
        let items = items_from_records(&records);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.is_pair() && i.rating == 3));
        assert!(items.iter().all(|i| i.sort_time == 5));
    }
}
