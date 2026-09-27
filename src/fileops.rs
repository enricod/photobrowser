use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::xmp::sidecar_path;

const MAX_RENAME_ATTEMPTS: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Move,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Skip,
    /// Append `_N` to every file of the item.
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    Done(Vec<(PathBuf, PathBuf)>),
    Skipped,
    Failed(String),
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok() // also catches dangling symlinks
}

fn numbered_name(src: &Path, n: u32) -> OsString {
    let mut s = src.file_stem().unwrap_or_default().to_os_string();
    s.push(format!("_{n}"));
    if let Some(ext) = src.extension() {
        s.push(".");
        s.push(ext);
    }
    s
}

fn build_plan(sources: &[PathBuf], dest_dir: &Path, n: u32) -> Vec<(PathBuf, PathBuf)> {
    let mut plan = Vec::new();
    for src in sources {
        let name = if n == 0 {
            src.file_name().unwrap_or_default().to_os_string()
        } else {
            numbered_name(src, n)
        };
        let dst = dest_dir.join(name);
        let sidecar = sidecar_path(src);
        let has_sidecar = exists(&sidecar);
        plan.push((src.clone(), dst.clone()));
        if has_sidecar {
            plan.push((sidecar, sidecar_path(&dst)));
        }
    }
    plan
}

fn is_free(plan: &[(PathBuf, PathBuf)]) -> bool {
    plan.iter().all(|(_, dst)| !exists(dst))
}

pub fn has_conflict(sources: &[PathBuf], dest_dir: &Path) -> bool {
    !is_free(&build_plan(sources, dest_dir, 0))
}

pub fn plan_targets(
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: ConflictPolicy,
) -> Option<Vec<(PathBuf, PathBuf)>> {
    let plain = build_plan(sources, dest_dir, 0);
    if is_free(&plain) {
        return Some(plain);
    }
    match policy {
        ConflictPolicy::Skip => None,
        ConflictPolicy::Rename => (1..=MAX_RENAME_ATTEMPTS)
            .map(|n| build_plan(sources, dest_dir, n))
            .find(|p| is_free(p)),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn transfer_item(
    sources: &[PathBuf],
    dest_dir: &Path,
    kind: OpKind,
    policy: ConflictPolicy,
) -> TransferOutcome {
    let in_dest = sources
        .iter()
        .all(|s| s.parent().is_some_and(|p| same_dir(p, dest_dir)));
    if in_dest {
        return TransferOutcome::Skipped;
    }
    let Some(plan) = plan_targets(sources, dest_dir, policy) else {
        return TransferOutcome::Skipped;
    };
    let mut done = Vec::with_capacity(plan.len());
    for (src, dst) in plan {
        if let Err(e) = transfer_one(&src, &dst, kind) {
            rollback(&done, kind);
            return TransferOutcome::Failed(format!("{}: {e}", src.display()));
        }
        done.push((src, dst));
    }
    TransferOutcome::Done(done)
}

fn transfer_one(src: &Path, dst: &Path, kind: OpKind) -> io::Result<()> {
    if exists(dst) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
    }
    match kind {
        OpKind::Copy => copy_verified(src, dst),
        OpKind::Move => match fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_verified(src, dst)?;
                fs::remove_file(src).inspect_err(|_| {
                    let _ = fs::remove_file(dst);
                })
            }
            Err(e) => Err(e),
        },
    }
}

/// Copies via a hidden temp file, fsyncs, checks the size, keeps the mtime, then renames.
fn copy_verified(src: &Path, dst: &Path) -> io::Result<()> {
    let mut tmp_name = OsString::from(".");
    tmp_name.push(dst.file_name().unwrap_or_default());
    tmp_name.push(".pbpart");
    let tmp = dst.with_file_name(tmp_name);

    let result = (|| {
        let mut input = File::open(src)?;
        let src_meta = input.metadata()?;
        let mut out = File::create_new(&tmp)?;
        let copied = io::copy(&mut input, &mut out)?;
        out.set_modified(src_meta.modified()?)?;
        out.sync_all()?;
        if copied != src_meta.len() {
            return Err(io::Error::other("size mismatch after copy"));
        }
        if exists(dst) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
        }
        fs::rename(&tmp, dst)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn rollback(done: &[(PathBuf, PathBuf)], kind: OpKind) {
    for (src, dst) in done.iter().rev() {
        match kind {
            OpKind::Copy => {
                let _ = fs::remove_file(dst);
            }
            OpKind::Move => {
                if fs::rename(dst, src).is_err() && copy_verified(dst, src).is_ok() {
                    let _ = fs::remove_file(dst);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp::sidecar_path;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    struct Fixture {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        dst: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        Fixture { _tmp: tmp, src, dst }
    }

    fn write(p: &Path, content: &str) {
        fs::write(p, content).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// IMG_1.CR3 (+ sidecar) and IMG_1.JPG in `src`.
    fn pair(f: &Fixture) -> Vec<PathBuf> {
        let raw = f.src.join("IMG_1.CR3");
        let jpg = f.src.join("IMG_1.JPG");
        write(&raw, "raw");
        write(&sidecar_path(&raw), "xmp");
        write(&jpg, "jpg");
        vec![raw, jpg]
    }

    #[test]
    fn move_pair_with_sidecar() {
        let f = fixture();
        let sources = pair(&f);
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        let TransferOutcome::Done(done) = out else { panic!("{out:?}") };
        assert_eq!(done.len(), 3);
        assert_eq!(names(&f.dst), ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG"]);
        assert!(names(&f.src).is_empty());
    }

    #[test]
    fn copy_keeps_sources_and_mtime() {
        let f = fixture();
        let sources = pair(&f);
        let before = fs::metadata(&sources[0]).unwrap().modified().unwrap();
        let out = transfer_item(&sources, &f.dst, OpKind::Copy, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert_eq!(names(&f.src).len(), 3);
        assert_eq!(names(&f.dst).len(), 3);
        let after = fs::metadata(f.dst.join("IMG_1.CR3")).unwrap().modified().unwrap();
        assert_eq!(before, after);
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "jpg");
    }

    #[test]
    fn skip_on_conflict_changes_nothing() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.JPG"), "existing");
        assert!(has_conflict(&sources, &f.dst));
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(names(&f.src).len(), 3);
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "existing");
    }

    #[test]
    fn rename_on_conflict_renames_whole_item() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.JPG"), "existing");
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Rename);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(
            names(&f.dst),
            ["IMG_1.JPG", "IMG_1_1.CR3", "IMG_1_1.CR3.xmp", "IMG_1_1.JPG"]
        );
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "existing");
    }

    #[test]
    fn rename_skips_taken_numbers() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.CR3"), "x");
        write(&f.dst.join("IMG_1_1.JPG"), "x");
        let plan = plan_targets(&sources, &f.dst, ConflictPolicy::Rename).unwrap();
        assert_eq!(plan[0].1, f.dst.join("IMG_1_2.CR3"));
    }

    #[test]
    fn move_rolls_back_on_failure() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        write(&sidecar_path(&raw), "xmp");
        let missing = f.src.join("IMG_1.JPG"); // never created → second transfer fails
        let out = transfer_item(&[raw.clone(), missing], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Failed(_)), "{out:?}");
        assert_eq!(names(&f.src), ["IMG_1.CR3", "IMG_1.CR3.xmp"]);
        assert!(names(&f.dst).is_empty());
    }

    #[test]
    fn copy_rolls_back_on_failure() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        let out = transfer_item(
            &[raw, f.src.join("IMG_1.JPG")],
            &f.dst,
            OpKind::Copy,
            ConflictPolicy::Skip,
        );
        assert!(matches!(out, TransferOutcome::Failed(_)));
        assert!(names(&f.dst).is_empty(), "{:?}", names(&f.dst));
    }

    #[test]
    fn move_into_same_folder_is_noop() {
        let f = fixture();
        let sources = pair(&f);
        let out = transfer_item(&sources, &f.src, OpKind::Move, ConflictPolicy::Rename);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(names(&f.src), ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG"]);
    }

    #[test]
    fn moves_non_utf8_names() {
        let f = fixture();
        let p = f.src.join(OsStr::from_bytes(b"\xffphoto.jpg"));
        write(&p, "x");
        let out = transfer_item(&[p], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert!(f.dst.join(OsStr::from_bytes(b"\xffphoto.jpg")).exists());
    }

    #[test]
    fn cross_filesystem_move_when_available() {
        // /dev/shm is tmpfs on Fedora; the project dir is usually on a disk.
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        let Ok(src_dir) = tempfile::tempdir_in(shm) else { return };
        let Ok(dst_dir) = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")) else { return };
        let dev = |p: &Path| fs::metadata(p).unwrap().dev();
        if dev(src_dir.path()) == dev(dst_dir.path()) {
            eprintln!("same filesystem: skipping cross-fs test");
            return;
        }
        let p = src_dir.path().join("a.jpg");
        write(&p, "content");
        let out = transfer_item(&[p.clone()], dst_dir.path(), OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert!(!p.exists());
        assert_eq!(fs::read_to_string(dst_dir.path().join("a.jpg")).unwrap(), "content");
    }
}
