use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::decode::Rgba;
use crate::decode::jpeg::decode_jpeg;
use crate::media::MediaFile;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Version of the cached thumbnails. Bump it whenever their content changes (decoder fixes,
/// orientation handling, `THUMB_EDGE`, encoder settings) so stale entries are not reused.
/// Entries of an older version are never read again and are the first to be evicted.
pub const CACHE_VERSION: u32 = 1;

/// A loaded entry whose mtime is older than this gets it refreshed, so eviction follows last
/// use; refreshing only stale entries keeps writes rare while scrolling.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// A temp file older than this cannot belong to a write in progress (those take
/// milliseconds), even one from another running instance: it was left by a crash.
const ORPHAN_TMP_AGE: Duration = Duration::from_secs(10 * 60);

pub struct DiskCache {
    dir: PathBuf,
    cap_bytes: u64,
    /// Bytes stored since the last cleanup; past `cap_bytes / 16` a cleanup runs, so the cap
    /// holds during a long session and not only at startup.
    written_since_cleanup: AtomicU64,
    cleaning: AtomicBool,
}

impl DiskCache {
    pub fn default_dir() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("photobrowser")
            .join("thumbs")
    }

    pub fn new(dir: PathBuf, cap_bytes: u64) -> io::Result<DiskCache> {
        fs::create_dir_all(&dir)?;
        Ok(DiskCache {
            dir,
            cap_bytes,
            written_since_cleanup: AtomicU64::new(0),
            cleaning: AtomicBool::new(false),
        })
    }

    /// blake3(path ‖ mtime_ns ‖ size [‖ version]): any edit to the file, or a new
    /// `CACHE_VERSION`, yields a new key.
    pub fn key(file: &MediaFile) -> String {
        Self::key_for_version(file, CACHE_VERSION)
    }

    /// Version 1 is the original key (no version mixed in), so existing caches stay valid.
    fn key_for_version(file: &MediaFile, version: u32) -> String {
        let mut h = blake3::Hasher::new();
        h.update(file.path.as_os_str().as_bytes());
        h.update(&file.mtime_ns.to_le_bytes());
        h.update(&file.size.to_le_bytes());
        if version >= 2 {
            h.update(b"photobrowser-thumb-v");
            h.update(&version.to_le_bytes());
        }
        h.finalize().to_hex().to_string()
    }

    fn path_for(&self, file: &MediaFile) -> PathBuf {
        self.dir.join(format!("{}.jpg", Self::key(file)))
    }

    pub fn contains(&self, file: &MediaFile) -> bool {
        self.path_for(file).exists()
    }

    pub fn load(&self, file: &MediaFile) -> Option<Rgba> {
        let path = self.path_for(file);
        let data = fs::read(&path).ok()?;
        let img = decode_jpeg(&data, 0).ok()?;
        touch_if_stale(&path);
        Some(img)
    }

    /// Writes atomically (temp file + rename) so readers never see a partial file.
    pub fn store(&self, file: &MediaFile, img: &Rgba) -> io::Result<()> {
        let jpeg = encode_jpeg(img).map_err(io::Error::other)?;
        let target = self.path_for(file);
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(
            ".{}.{}.{n}.tmp",
            Self::key(file),
            std::process::id()
        ));
        fs::write(&tmp, &jpeg)?;
        fs::rename(&tmp, &target).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        self.note_written(jpeg.len() as u64);
        Ok(())
    }

    /// Runs `cleanup` on this (worker) thread once enough has been written since the last
    /// one. Only one cleanup runs at a time; others just keep counting.
    fn note_written(&self, bytes: u64) {
        let total = self
            .written_since_cleanup
            .fetch_add(bytes, Ordering::Relaxed)
            + bytes;
        if total < (self.cap_bytes / 16).max(1) || self.cleaning.swap(true, Ordering::AcqRel) {
            return;
        }
        self.written_since_cleanup.store(0, Ordering::Relaxed);
        let _ = self.cleanup();
        self.cleaning.store(false, Ordering::Release);
    }

    /// Deletes the least recently used entries (oldest mtime; `load` refreshes it) until the
    /// cache is within its cap.
    pub fn cleanup(&self) -> io::Result<u64> {
        self.remove_orphaned_temp_files();
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                let path = e.path();
                let filename = path.file_name()?.to_str()?;
                // Only process completed cache entries: must be regular files,
                // not start with '.', and have '.jpg' extension.
                (m.is_file() && !filename.starts_with('.') && filename.ends_with(".jpg"))
                    .then(|| (path, m.len(), m.modified().unwrap_or(UNIX_EPOCH)))
            })
            .collect();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        if total <= self.cap_bytes {
            return Ok(0);
        }
        entries.sort_by_key(|e| e.2);
        let mut removed = 0;
        for (path, len, _) in entries {
            if total <= self.cap_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
                removed += len;
            }
        }
        Ok(removed)
    }
}

impl DiskCache {
    /// Deletes `.…tmp` files left by a `store()` that never reached its rename (the process
    /// was killed or crashed). They are skipped by eviction and outside the cap, so they
    /// would otherwise pile up forever.
    fn remove_orphaned_temp_files(&self) {
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        for entry in dir.filter_map(Result::ok) {
            let name = entry.file_name();
            let name = name.as_bytes();
            if !(name.starts_with(b".") && name.ends_with(b".tmp")) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > ORPHAN_TMP_AGE));
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Best effort: sets `path`'s mtime to now if it is older than `TOUCH_AFTER`.
fn touch_if_stale(path: &std::path::Path) {
    let now = SystemTime::now();
    let stale = fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > TOUCH_AFTER));
    if stale && let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(now);
    }
}

pub fn encode_jpeg(img: &Rgba) -> Result<Vec<u8>, turbojpeg::Error> {
    let mut c = turbojpeg::Compressor::new()?;
    c.set_quality(85)?;
    c.set_subsamp(turbojpeg::Subsamp::Sub2x2)?;
    c.compress_to_vec(turbojpeg::Image {
        pixels: img.data.as_slice(),
        width: img.width as usize,
        pitch: img.width as usize * 4,
        height: img.height as usize,
        format: turbojpeg::PixelFormat::RGBA,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::fs::File;
    use std::time::{Duration, UNIX_EPOCH};

    fn media(path: &str, size: u64, mtime_ns: i64) -> MediaFile {
        MediaFile {
            path: PathBuf::from(path),
            kind: FileKind::Jpeg,
            size,
            mtime_ns,
        }
    }

    fn thumb() -> Rgba {
        Rgba::new(64, 48, vec![120; 64 * 48 * 4])
    }

    #[test]
    fn store_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let f = media("/p/a.jpg", 10, 20);
        assert!(!cache.contains(&f));
        assert!(cache.load(&f).is_none());
        cache.store(&f, &thumb()).unwrap();
        assert!(cache.contains(&f));
        let img = cache.load(&f).unwrap();
        assert_eq!((img.width, img.height), (64, 48));
    }

    #[test]
    fn key_changes_with_mtime_and_size() {
        let a = DiskCache::key(&media("/p/a.jpg", 10, 20));
        assert_ne!(a, DiskCache::key(&media("/p/a.jpg", 10, 21)));
        assert_ne!(a, DiskCache::key(&media("/p/a.jpg", 11, 20)));
        assert_ne!(a, DiskCache::key(&media("/p/b.jpg", 10, 20)));
        assert_eq!(a, DiskCache::key(&media("/p/a.jpg", 10, 20)));
    }

    #[test]
    fn version_1_keeps_the_original_key_and_later_versions_change_it() {
        let f = media("/p/a.jpg", 10, 20);
        assert_eq!(
            CACHE_VERSION, 1,
            "update this test when bumping the version"
        );
        let mut h = blake3::Hasher::new();
        h.update(b"/p/a.jpg");
        h.update(&20i64.to_le_bytes());
        h.update(&10u64.to_le_bytes());
        assert_eq!(DiskCache::key(&f), h.finalize().to_hex().to_string());
        let v2 = DiskCache::key_for_version(&f, 2);
        assert_ne!(DiskCache::key(&f), v2);
        assert_ne!(v2, DiskCache::key_for_version(&f, 3));
    }

    fn set_mtime(path: &std::path::Path, t: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn mtime(path: &std::path::Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn load_refreshes_only_stale_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let (old, fresh) = (media("/p/old.jpg", 1, 1), media("/p/fresh.jpg", 1, 1));
        cache.store(&old, &thumb()).unwrap();
        cache.store(&fresh, &thumb()).unwrap();
        let long_ago = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let recent = SystemTime::now() - Duration::from_secs(60 * 60);
        set_mtime(&cache.path_for(&old), long_ago);
        set_mtime(&cache.path_for(&fresh), recent);
        cache.load(&old).unwrap();
        cache.load(&fresh).unwrap();
        assert!(
            mtime(&cache.path_for(&old)) > recent,
            "stale entry refreshed"
        );
        assert_eq!(
            mtime(&cache.path_for(&fresh)),
            recent,
            "fresh entry left alone"
        );
    }

    #[test]
    fn cleanup_keeps_recently_loaded_entries() {
        let dir = tempfile::tempdir().unwrap();
        let unlimited = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let (used, unused) = (media("/p/used.jpg", 1, 1), media("/p/unused.jpg", 1, 1));
        unlimited.store(&used, &thumb()).unwrap();
        unlimited.store(&unused, &thumb()).unwrap();
        // `used` was written first, but is the one being looked at.
        set_mtime(
            &unlimited.path_for(&used),
            UNIX_EPOCH + Duration::from_secs(1_000),
        );
        set_mtime(
            &unlimited.path_for(&unused),
            UNIX_EPOCH + Duration::from_secs(2_000),
        );
        unlimited.load(&used).unwrap();
        let one_entry = fs::metadata(unlimited.path_for(&used)).unwrap().len();
        let capped = DiskCache::new(dir.path().to_path_buf(), one_entry).unwrap();
        capped.cleanup().unwrap();
        assert!(capped.contains(&used));
        assert!(!capped.contains(&unused));
    }

    #[test]
    fn storing_past_the_threshold_enforces_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let probe = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        probe.store(&media("/p/probe.jpg", 1, 1), &thumb()).unwrap();
        let entry = fs::metadata(probe.path_for(&media("/p/probe.jpg", 1, 1)))
            .unwrap()
            .len();
        let cap = entry * 3;
        let cache = DiskCache::new(dir.path().to_path_buf(), cap).unwrap();
        for i in 0..20 {
            cache
                .store(&media(&format!("/p/{i}.jpg"), 1, 1), &thumb())
                .unwrap();
        }
        let total: u64 = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= cap, "{total} > {cap}");
    }

    #[test]
    fn cleanup_removes_orphaned_temp_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let entry = media("/p/a.jpg", 1, 1);
        cache.store(&entry, &thumb()).unwrap();
        let orphan = dir.path().join(".abc.123.4.tmp");
        let in_flight = dir.path().join(".def.456.7.tmp");
        let other_dotfile = dir.path().join(".keep-me");
        for p in [&orphan, &in_flight, &other_dotfile] {
            fs::write(p, b"x").unwrap();
        }
        let long_ago = SystemTime::now() - Duration::from_secs(60 * 60);
        set_mtime(&orphan, long_ago);
        set_mtime(&other_dotfile, long_ago);
        set_mtime(&cache.path_for(&entry), long_ago);
        cache.cleanup().unwrap();
        assert!(!orphan.exists(), "crash leftover removed");
        assert!(in_flight.exists(), "a fresh temp file may still be renamed");
        assert!(other_dotfile.exists(), "only temp files are touched");
        assert!(cache.contains(&entry), "entries within the cap are kept");
    }

    #[test]
    fn corrupt_entry_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let f = media("/p/a.jpg", 1, 1);
        fs::write(cache.path_for(&f), b"junk").unwrap();
        assert!(cache.load(&f).is_none());
    }

    #[test]
    fn cleanup_removes_oldest_until_under_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 250).unwrap();
        for (i, name) in ["old.jpg", "mid.jpg", "new.jpg"].iter().enumerate() {
            let p = dir.path().join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64))
                .unwrap();
        }
        assert_eq!(cache.cleanup().unwrap(), 100);
        assert!(!dir.path().join("old.jpg").exists());
        assert!(dir.path().join("mid.jpg").exists() && dir.path().join("new.jpg").exists());
    }

    #[test]
    fn cleanup_skips_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 150).unwrap();
        // Create completed cache entries
        for (i, name) in ["old.jpg", "new.jpg"].iter().enumerate() {
            let p = dir.path().join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64))
                .unwrap();
        }
        // Add a temp file (in-flight write from concurrent store())
        let tmp_path = dir.path().join(".abc123.456.789.tmp");
        fs::write(&tmp_path, vec![0u8; 100]).unwrap();
        // Total is 300 bytes, cap is 150. cleanup() should remove only old.jpg (100 bytes),
        // leaving new.jpg and the temp file untouched.
        assert_eq!(cache.cleanup().unwrap(), 100);
        assert!(!dir.path().join("old.jpg").exists());
        assert!(dir.path().join("new.jpg").exists());
        assert!(
            tmp_path.exists(),
            "temp file must not be deleted by cleanup"
        );
    }
}
