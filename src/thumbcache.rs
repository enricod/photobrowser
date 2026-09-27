use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decode::jpeg::decode_jpeg;
use crate::decode::Rgba;
use crate::media::MediaFile;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct DiskCache {
    dir: PathBuf,
    cap_bytes: u64,
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
        Ok(DiskCache { dir, cap_bytes })
    }

    /// blake3(path ‖ mtime_ns ‖ size): any edit to the file yields a new key.
    pub fn key(file: &MediaFile) -> String {
        let mut h = blake3::Hasher::new();
        h.update(file.path.as_os_str().as_bytes());
        h.update(&file.mtime_ns.to_le_bytes());
        h.update(&file.size.to_le_bytes());
        h.finalize().to_hex().to_string()
    }

    fn path_for(&self, file: &MediaFile) -> PathBuf {
        self.dir.join(format!("{}.jpg", Self::key(file)))
    }

    pub fn contains(&self, file: &MediaFile) -> bool {
        self.path_for(file).exists()
    }

    pub fn load(&self, file: &MediaFile) -> Option<Rgba> {
        let data = fs::read(self.path_for(file)).ok()?;
        decode_jpeg(&data, 0).ok()
    }

    /// Writes atomically (temp file + rename) so readers never see a partial file.
    pub fn store(&self, file: &MediaFile, img: &Rgba) -> io::Result<()> {
        let jpeg = encode_jpeg(img).map_err(io::Error::other)?;
        let target = self.path_for(file);
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(".{}.{}.{n}.tmp", Self::key(file), std::process::id()));
        fs::write(&tmp, &jpeg)?;
        fs::rename(&tmp, &target).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    /// Deletes the oldest entries until the cache is within its cap.
    pub fn cleanup(&self) -> io::Result<u64> {
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file()
                    .then(|| (e.path(), m.len(), m.modified().unwrap_or(UNIX_EPOCH)))
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
        MediaFile { path: PathBuf::from(path), kind: FileKind::Jpeg, size, mtime_ns }
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
}
