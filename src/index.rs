use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::media::MediaFile;

pub const SCHEMA_VERSION: &str = "1";
const MAX_RECENT: i64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("index schema version mismatch")]
    SchemaMismatch,
}

pub type Result<T> = std::result::Result<T, IndexError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub capture_time: Option<i64>,
    pub rating: u8,
    pub rejected: bool,
    /// Sidecar mtime (ns) at the last successful read or write.
    pub xmp_synced_at: Option<i64>,
    /// A local rating change has not been written to the sidecar yet.
    pub xmp_dirty: bool,
}

impl FileRecord {
    pub fn new(path: PathBuf, size: u64, mtime_ns: i64) -> FileRecord {
        FileRecord {
            path,
            size,
            mtime_ns,
            capture_time: None,
            rating: 0,
            rejected: false,
            xmp_synced_at: None,
            xmp_dirty: false,
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (
  path BLOB PRIMARY KEY,
  folder BLOB NOT NULL,
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  capture_time INTEGER,
  rating INTEGER NOT NULL DEFAULT 0,
  rejected INTEGER NOT NULL DEFAULT 0,
  xmp_synced_at INTEGER,
  xmp_dirty INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS files_folder ON files(folder);
CREATE TABLE IF NOT EXISTS recent_destinations (path BLOB PRIMARY KEY, used_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

const COLUMNS: &str =
    "path, size, mtime_ns, capture_time, rating, rejected, xmp_synced_at, xmp_dirty";

const UPSERT: &str = "
INSERT INTO files (path, folder, size, mtime_ns, capture_time, rating, rejected, xmp_synced_at, xmp_dirty)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(path) DO UPDATE SET
  folder = excluded.folder,
  size = excluded.size,
  mtime_ns = excluded.mtime_ns,
  capture_time = excluded.capture_time,
  rating = CASE WHEN files.xmp_dirty = 1 THEN files.rating ELSE excluded.rating END,
  rejected = CASE WHEN files.xmp_dirty = 1 THEN files.rejected ELSE excluded.rejected END,
  xmp_synced_at = CASE WHEN files.xmp_dirty = 1 THEN files.xmp_synced_at ELSE excluded.xmp_synced_at END,
  xmp_dirty = MAX(files.xmp_dirty, excluded.xmp_dirty)
";

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn folder_bytes(p: &Path) -> &[u8] {
    bytes(p.parent().unwrap_or(Path::new("")))
}

fn path_from(v: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(v))
}

fn row_to_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        path: path_from(r.get(0)?),
        size: r.get::<_, i64>(1)? as u64,
        mtime_ns: r.get(2)?,
        capture_time: r.get(3)?,
        rating: r.get(4)?,
        rejected: r.get(5)?,
        xmp_synced_at: r.get(6)?,
        xmp_dirty: r.get(7)?,
    })
}

/// Classify whether an error indicates actual corruption (not transient failures).
/// Only NotADatabase, DatabaseCorrupt, and schema mismatch are considered corruption.
fn is_corruption(e: &IndexError) -> bool {
    use rusqlite::ErrorCode;
    match e {
        IndexError::SchemaMismatch => true,
        IndexError::Db(rusqlite::Error::SqliteFailure(sqlite_err, _)) => {
            matches!(
                sqlite_err.code,
                ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
            )
        }
        _ => false,
    }
}

pub struct Index {
    conn: Connection,
}

impl Index {
    pub fn default_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("photobrowser")
            .join("index.db")
    }

    /// Opens (or creates) the index; a corrupt or outdated file is deleted and rebuilt.
    /// Transient errors (locked, permission denied, I/O) are returned unchanged.
    pub fn open(path: &Path) -> Result<Index> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match Self::try_open(path) {
            Ok(index) => Ok(index),
            Err(e) => {
                if is_corruption(&e) {
                    // Delete corrupted database and all WAL/SHM files
                    for suffix in ["", "-wal", "-shm"] {
                        let mut p = path.as_os_str().to_owned();
                        p.push(suffix);
                        let _ = std::fs::remove_file(p);
                    }
                    Self::try_open(path)
                } else {
                    // Transient error: return it unchanged
                    Err(e)
                }
            }
        }
    }

    fn try_open(path: &Path) -> Result<Index> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Index> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Index> {
        conn.execute_batch(SCHEMA)?;
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match version.as_deref() {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION],
                )?;
            }
            Some(v) if v == SCHEMA_VERSION => {}
            Some(_) => return Err(IndexError::SchemaMismatch),
        }
        Ok(Index { conn })
    }

    pub fn load_folder(&self, folder: &Path) -> Result<HashMap<PathBuf, FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM files WHERE folder = ?1"))?;
        let rows = stmt.query_map([bytes(folder)], row_to_record)?;
        let mut map = HashMap::new();
        for row in rows {
            let rec = row?;
            map.insert(rec.path.clone(), rec);
        }
        Ok(map)
    }

    pub fn get(&self, path: &Path) -> Result<Option<FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM files WHERE path = ?1"))?;
        Ok(stmt.query_row([bytes(path)], row_to_record).optional()?)
    }

    pub fn upsert_many(&mut self, recs: &[FileRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(UPSERT)?;
            for r in recs {
                stmt.execute(params![
                    bytes(&r.path),
                    folder_bytes(&r.path),
                    r.size as i64,
                    r.mtime_ns,
                    r.capture_time,
                    r.rating,
                    r.rejected,
                    r.xmp_synced_at,
                    r.xmp_dirty,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_paths(&mut self, paths: &[PathBuf]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM files WHERE path = ?1")?;
            for p in paths {
                stmt.execute([bytes(p)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records a local rating change (inserting the row if the scan has not yet).
    pub fn set_rating(&self, file: &MediaFile, rating: u8, rejected: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files (path, folder, size, mtime_ns, rating, rejected, xmp_dirty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT(path) DO UPDATE SET
               rating = excluded.rating, rejected = excluded.rejected, xmp_dirty = 1",
            params![
                bytes(&file.path),
                folder_bytes(&file.path),
                file.size as i64,
                file.mtime_ns,
                rating,
                rejected
            ],
        )?;
        Ok(())
    }

    pub fn mark_xmp_synced(&self, path: &Path, sidecar_mtime_ns: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET xmp_dirty = 0, xmp_synced_at = ?2 WHERE path = ?1",
            params![bytes(path), sidecar_mtime_ns],
        )?;
        Ok(())
    }

    pub fn mark_xmp_dirty(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET xmp_dirty = 1 WHERE path = ?1",
            [bytes(path)],
        )?;
        Ok(())
    }

    pub fn dirty_records(&self, folder: &Path) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM files WHERE folder = ?1 AND xmp_dirty = 1"
        ))?;
        let rows = stmt.query_map([bytes(folder)], row_to_record)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename_path(&mut self, from: &Path, to: &Path) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM files WHERE path = ?1", [bytes(to)])?;
        tx.execute(
            "UPDATE files SET path = ?2, folder = ?3 WHERE path = ?1",
            params![bytes(from), bytes(to), folder_bytes(to)],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_recent_destination(&self, dir: &Path, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO recent_destinations (path, used_at) VALUES (?1, ?2)",
            params![bytes(dir), now],
        )?;
        self.conn.execute(
            "DELETE FROM recent_destinations WHERE path NOT IN
               (SELECT path FROM recent_destinations ORDER BY used_at DESC LIMIT ?1)",
            [MAX_RECENT],
        )?;
        Ok(())
    }

    pub fn recent_destinations(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path FROM recent_destinations ORDER BY used_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(path_from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::ffi::OsStr;

    fn rec(p: &str) -> FileRecord {
        FileRecord::new(PathBuf::from(p), 10, 100)
    }

    fn media(p: &str) -> MediaFile {
        MediaFile {
            path: PathBuf::from(p),
            kind: FileKind::Jpeg,
            size: 10,
            mtime_ns: 100,
        }
    }

    #[test]
    fn upsert_and_load_folder_roundtrip() {
        let mut idx = Index::open_in_memory().unwrap();
        let mut a = rec("/p/a.jpg");
        a.capture_time = Some(1234);
        a.rating = 3;
        idx.upsert_many(&[a.clone(), rec("/p/sub/b.jpg"), rec("/q/c.jpg")])
            .unwrap();
        let loaded = idx.load_folder(Path::new("/p")).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[Path::new("/p/a.jpg")], a);
    }

    #[test]
    fn set_rating_marks_dirty_and_mark_synced_clears() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
        idx.set_rating(&media("/p/a.jpg"), 4, false).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.rating, r.rejected, r.xmp_dirty), (4, false, true));
        idx.mark_xmp_synced(Path::new("/p/a.jpg"), 555).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.xmp_dirty, r.xmp_synced_at), (false, Some(555)));
        idx.mark_xmp_dirty(Path::new("/p/a.jpg")).unwrap();
        assert_eq!(idx.dirty_records(Path::new("/p")).unwrap().len(), 1);
    }

    #[test]
    fn set_rating_inserts_missing_row() {
        let idx = Index::open_in_memory().unwrap();
        idx.set_rating(&media("/p/new.jpg"), 2, true).unwrap();
        let r = idx.get(Path::new("/p/new.jpg")).unwrap().unwrap();
        assert_eq!(
            (r.rating, r.rejected, r.xmp_dirty, r.size),
            (2, true, true, 10)
        );
    }

    #[test]
    fn upsert_does_not_clobber_dirty_rating() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
        idx.set_rating(&media("/p/a.jpg"), 5, false).unwrap();
        let mut scanned = rec("/p/a.jpg");
        scanned.size = 99;
        scanned.rating = 1;
        idx.upsert_many(&[scanned]).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.size, r.rating, r.xmp_dirty), (99, 5, true));
    }

    #[test]
    fn rename_path_moves_record_to_new_folder() {
        let mut idx = Index::open_in_memory().unwrap();
        let mut a = rec("/p/a.jpg");
        a.rating = 2;
        idx.upsert_many(&[a]).unwrap();
        idx.rename_path(Path::new("/p/a.jpg"), Path::new("/q/a.jpg"))
            .unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
        assert_eq!(
            idx.load_folder(Path::new("/q")).unwrap()[Path::new("/q/a.jpg")].rating,
            2
        );
    }

    #[test]
    fn remove_paths_deletes_rows() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg"), rec("/p/b.jpg")])
            .unwrap();
        idx.remove_paths(&[PathBuf::from("/p/a.jpg")]).unwrap();
        assert_eq!(idx.load_folder(Path::new("/p")).unwrap().len(), 1);
    }

    #[test]
    fn recent_destinations_order_and_cap() {
        let idx = Index::open_in_memory().unwrap();
        for i in 0..12 {
            idx.add_recent_destination(Path::new(&format!("/d/{i}")), i)
                .unwrap();
        }
        idx.add_recent_destination(Path::new("/d/3"), 100).unwrap();
        let recent = idx.recent_destinations().unwrap();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0], PathBuf::from("/d/3"));
        assert_eq!(recent[1], PathBuf::from("/d/11"));
    }

    #[test]
    fn garbage_file_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        std::fs::write(&path, b"this is not a database at all, just junk bytes").unwrap();
        let idx = Index::open(&path).unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
    }

    #[test]
    fn schema_mismatch_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        {
            let mut idx = Index::open(&path).unwrap();
            idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
            idx.conn
                .execute("UPDATE meta SET value='0' WHERE key='schema_version'", [])
                .unwrap();
        }
        let idx = Index::open(&path).unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
    }

    #[test]
    fn non_utf8_path_roundtrip() {
        let mut idx = Index::open_in_memory().unwrap();
        let path = Path::new("/p").join(OsStr::from_bytes(b"\xff\xfe.jpg"));
        idx.upsert_many(&[FileRecord::new(path.clone(), 1, 1)])
            .unwrap();
        let loaded = idx.load_folder(Path::new("/p")).unwrap();
        assert!(loaded.contains_key(&path));
    }

    #[test]
    fn is_corruption_classifier() {
        use rusqlite::ErrorCode;

        // SchemaMismatch is corruption
        assert!(is_corruption(&IndexError::SchemaMismatch));

        // Io error is NOT corruption (transient)
        let io_err = IndexError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test",
        ));
        assert!(!is_corruption(&io_err));

        // NotADatabase is corruption
        let not_db_err = IndexError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::NotADatabase,
                extended_code: 0,
            },
            None,
        ));
        assert!(is_corruption(&not_db_err));

        // DatabaseCorrupt is corruption
        let corrupt_err = IndexError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseCorrupt,
                extended_code: 0,
            },
            None,
        ));
        assert!(is_corruption(&corrupt_err));

        // DatabaseBusy is NOT corruption (transient)
        let busy_err = IndexError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy,
                extended_code: 0,
            },
            None,
        ));
        assert!(!is_corruption(&busy_err));
    }
}
