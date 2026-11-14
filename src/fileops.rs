use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::xmp::{is_sidecar, sidecar_path};

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

/// A sidecar another tool keeps next to a photo, besides our `<file>.<ext>.xmp`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Extra {
    /// darktable duplicate `<stem>_<digits>.<ext>.xmp`; holds the digits.
    Duplicate(OsString),
    /// `<stem>.xmp` (Lightroom, Capture One, digiKam).
    Stem,
}

/// Extra sidecars of each source, read once per item (the Keep Both loop reuses them). A
/// `<stem>_<digits>.<ext>.xmp` is a duplicate only if no photo `<stem>_<digits>.<ext>` exists:
/// otherwise it is that photo's own sidecar. `<stem>.xmp` is shared by a RAW+JPEG pair, so it
/// goes with the first source only.
fn find_extras(sources: &[PathBuf]) -> Vec<Vec<(PathBuf, Extra)>> {
    let mut taken: HashSet<PathBuf> = HashSet::new();
    sources
        .iter()
        .map(|src| {
            let (Some(dir), Some(stem), Some(ext)) =
                (src.parent(), src.file_stem(), src.extension())
            else {
                return Vec::new();
            };
            let Ok(entries) = fs::read_dir(dir) else {
                return Vec::new();
            };
            let names: HashSet<OsString> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            let (stem_b, ext_b) = (stem.as_bytes(), ext.as_bytes());
            let mut extras = Vec::new();
            for name in &names {
                let n = name.as_bytes();
                let extra = if n == [stem_b, b".xmp"].concat().as_slice() {
                    Some(Extra::Stem)
                } else {
                    let prefix = [stem_b, b"_"].concat();
                    let suffix = [b".", ext_b, b".xmp"].concat();
                    n.strip_prefix(prefix.as_slice())
                        .and_then(|rest| rest.strip_suffix(suffix.as_slice()))
                        .filter(|d| !d.is_empty() && d.iter().all(u8::is_ascii_digit))
                        .filter(|d| {
                            let photo = [stem_b, b"_", d, b".", ext_b].concat();
                            !names.contains(OsStr::from_bytes(&photo))
                        })
                        .map(|d| Extra::Duplicate(OsStr::from_bytes(d).to_os_string()))
                };
                if let Some(extra) = extra {
                    let path = dir.join(name);
                    if taken.insert(path.clone()) {
                        extras.push((path, extra));
                    }
                }
            }
            extras.sort();
            extras
        })
        .collect()
}

/// Where an extra sidecar goes when its photo lands at `dst`.
fn extra_destination(dst: &Path, extra: &Extra) -> PathBuf {
    let stem = dst.file_stem().unwrap_or_default();
    let mut name = stem.to_os_string();
    match extra {
        Extra::Duplicate(digits) => {
            name.push("_");
            name.push(digits);
            if let Some(ext) = dst.extension() {
                name.push(".");
                name.push(ext);
            }
            name.push(".xmp");
        }
        Extra::Stem => name.push(".xmp"),
    }
    dst.with_file_name(name)
}

fn build_plan(
    sources: &[PathBuf],
    extras: &[Vec<(PathBuf, Extra)>],
    dest_dir: &Path,
    n: u32,
) -> Vec<(PathBuf, PathBuf)> {
    let mut plan = Vec::new();
    for (i, src) in sources.iter().enumerate() {
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
        for (path, extra) in extras.get(i).into_iter().flatten() {
            plan.push((path.clone(), extra_destination(&dst, extra)));
        }
    }
    plan
}

/// True if nothing in the plan would land on an existing path. A photo's sidecar name at the
/// destination must be free too, even when the photo has no sidecar to bring along: otherwise
/// it would silently adopt a leftover sidecar of another photo (its rating, reject flag and
/// darktable history).
fn is_free(plan: &[(PathBuf, PathBuf)]) -> bool {
    plan.iter()
        .all(|(_, dst)| !exists(dst) && (is_sidecar(dst) || !exists(&sidecar_path(dst))))
}

pub fn has_conflict(sources: &[PathBuf], dest_dir: &Path) -> bool {
    !is_free(&build_plan(sources, &find_extras(sources), dest_dir, 0))
}

pub fn plan_targets(
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: ConflictPolicy,
) -> Option<Vec<(PathBuf, PathBuf)>> {
    let extras = find_extras(sources);
    let plain = build_plan(sources, &extras, dest_dir, 0);
    if is_free(&plain) {
        return Some(plain);
    }
    match policy {
        ConflictPolicy::Skip => None,
        ConflictPolicy::Rename => (1..=MAX_RENAME_ATTEMPTS)
            .map(|n| build_plan(sources, &extras, dest_dir, n))
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
            let incomplete = rollback(&done, kind);
            let mut msg = format!("{}: {e}", src.display());
            if !incomplete.is_empty() {
                msg.push_str("; rollback incomplete:");
                for (src, dst) in incomplete {
                    msg.push_str(&format!(
                        " {} could not be restored to {}",
                        dst.display(),
                        src.display()
                    ));
                }
            }
            return TransferOutcome::Failed(msg);
        }
        done.push((src, dst));
    }
    TransferOutcome::Done(done)
}

/// Like `transfer_item`, but first creates `dest_dir` and any missing parents (for organizing
/// into dated folders). Folders it created are removed again if the item did not land in
/// them (skipped or failed), so a run leaves no empty folders behind.
pub fn transfer_item_creating_dest(
    sources: &[PathBuf],
    dest_dir: &Path,
    kind: OpKind,
    policy: ConflictPolicy,
) -> TransferOutcome {
    let created = missing_dirs(dest_dir);
    if let Err(e) = fs::create_dir_all(dest_dir) {
        return TransferOutcome::Failed(format!("{}: {e}", dest_dir.display()));
    }
    let outcome = transfer_item(sources, dest_dir, kind, policy);
    if !matches!(outcome, TransferOutcome::Done(_)) {
        // Deepest first; `remove_dir` refuses a folder something else has put files in.
        for dir in &created {
            if fs::remove_dir(dir).is_err() {
                break;
            }
        }
    }
    outcome
}

/// `dir` and those of its ancestors that do not exist yet, deepest first.
fn missing_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.as_os_str().is_empty() || exists(d) {
            break;
        }
        missing.push(d.to_path_buf());
        current = d.parent();
    }
    missing
}

fn transfer_one(src: &Path, dst: &Path, kind: OpKind) -> io::Result<()> {
    if exists(dst) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination exists",
        ));
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

/// Flushes `dir`'s entries (e.g. a rename into it) to disk. Filesystems that cannot sync a
/// directory (some network and FUSE mounts) report InvalidInput or Unsupported: there is
/// nothing more to do there, so those count as success.
fn sync_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir).and_then(|d| d.sync_all()) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// Copies via a hidden temp file, fsyncs, checks the size, keeps the mtime, renames, then
/// syncs the destination folder, so a move's source is only removed once the new name is on
/// disk. On failure only a temp file this call created is removed: an existing `.pbpart`
/// (left by an earlier crash) may hold the last copy of a photo.
fn copy_verified(src: &Path, dst: &Path) -> io::Result<()> {
    let mut tmp_name = OsString::from(".");
    tmp_name.push(dst.file_name().unwrap_or_default());
    tmp_name.push(".pbpart");
    let tmp = dst.with_file_name(tmp_name);

    let mut created = false;
    let mut renamed = false;
    let result = (|| {
        let mut input = File::open(src)?;
        let src_meta = input.metadata()?;
        let mut out = File::create_new(&tmp)?;
        created = true;
        let copied = io::copy(&mut input, &mut out)?;
        out.set_modified(src_meta.modified()?)?;
        out.sync_all()?;
        if copied != src_meta.len() {
            return Err(io::Error::other("size mismatch after copy"));
        }
        if exists(dst) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination exists",
            ));
        }
        fs::rename(&tmp, dst)?;
        renamed = true;
        sync_dir(dst.parent().unwrap_or(Path::new(".")))
    })();
    if result.is_err() && created {
        // After the rename the copy lives at `dst`; the caller keeps the source.
        let _ = fs::remove_file(if renamed { dst } else { &tmp });
    }
    result
}

fn rollback(done: &[(PathBuf, PathBuf)], kind: OpKind) -> Vec<(PathBuf, PathBuf)> {
    let mut incomplete = Vec::new();
    for (src, dst) in done.iter().rev() {
        let failed = match kind {
            OpKind::Copy => {
                // For copy, just need to delete the destination
                fs::remove_file(dst).is_err()
            }
            OpKind::Move => {
                // For move, try rename first, then copy+remove
                let rename_ok = fs::rename(dst, src).is_ok();
                if rename_ok {
                    false // success
                } else {
                    // rename failed, try copy+remove
                    match copy_verified(dst, src) {
                        Ok(()) => {
                            // copy succeeded, try to remove dst
                            fs::remove_file(dst).is_err()
                        }
                        Err(_) => true, // copy failed
                    }
                }
            }
        };
        if failed {
            incomplete.push((src.clone(), dst.clone()));
        }
    }
    incomplete
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
        Fixture {
            _tmp: tmp,
            src,
            dst,
        }
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
        let TransferOutcome::Done(done) = out else {
            panic!("{out:?}")
        };
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
        let after = fs::metadata(f.dst.join("IMG_1.CR3"))
            .unwrap()
            .modified()
            .unwrap();
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
        assert_eq!(
            fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(),
            "existing"
        );
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
        assert_eq!(
            fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(),
            "existing"
        );
    }

    #[test]
    fn leftover_sidecar_at_destination_is_a_conflict() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        let leftover = f.dst.join("IMG_1.CR3.xmp");
        write(&leftover, "another photo's edits");
        let sources = vec![raw];
        assert!(has_conflict(&sources, &f.dst));
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(names(&f.src), ["IMG_1.CR3"]);
        assert_eq!(
            fs::read_to_string(&leftover).unwrap(),
            "another photo's edits"
        );
    }

    #[test]
    fn keep_both_avoids_a_leftover_sidecar() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        write(&f.dst.join("IMG_1.CR3.xmp"), "leftover");
        let out = transfer_item(&[raw], &f.dst, OpKind::Move, ConflictPolicy::Rename);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(names(&f.dst), ["IMG_1.CR3.xmp", "IMG_1_1.CR3"]);
        assert_eq!(
            fs::read_to_string(f.dst.join("IMG_1.CR3.xmp")).unwrap(),
            "leftover"
        );
    }

    #[test]
    fn keep_both_skips_numbers_with_a_leftover_sidecar() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        write(&f.dst.join("IMG_1.CR3"), "x");
        write(&f.dst.join("IMG_1_1.CR3.xmp"), "leftover");
        let plan = plan_targets(&[raw], &f.dst, ConflictPolicy::Rename).unwrap();
        assert_eq!(plan, [(f.src.join("IMG_1.CR3"), f.dst.join("IMG_1_2.CR3"))]);
    }

    #[test]
    fn copy_leaves_an_existing_part_file_alone() {
        let f = fixture();
        let src = f.src.join("IMG_1.CR3");
        write(&src, "new photo");
        let part = f.dst.join(".IMG_1.CR3.pbpart");
        write(&part, "last copy of another photo");
        let out = transfer_item(
            std::slice::from_ref(&src),
            &f.dst,
            OpKind::Copy,
            ConflictPolicy::Skip,
        );
        assert!(matches!(out, TransferOutcome::Failed(_)), "{out:?}");
        assert_eq!(
            fs::read_to_string(&part).unwrap(),
            "last copy of another photo"
        );
        assert!(!f.dst.join("IMG_1.CR3").exists());
        assert_eq!(fs::read_to_string(&src).unwrap(), "new photo");
    }

    #[test]
    fn sync_dir_succeeds_on_a_regular_folder() {
        let f = fixture();
        sync_dir(&f.dst).unwrap();
    }

    /// IMG_1.CR3 with its own sidecar and two darktable duplicates, in `src`.
    fn raw_with_duplicates(f: &Fixture) -> PathBuf {
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        write(&sidecar_path(&raw), "v0");
        write(&f.src.join("IMG_1_01.CR3.xmp"), "v1");
        write(&f.src.join("IMG_1_02.CR3.xmp"), "v2");
        raw
    }

    #[test]
    fn darktable_duplicates_move_with_their_photo() {
        let f = fixture();
        let raw = raw_with_duplicates(&f);
        let out = transfer_item(&[raw], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(
            names(&f.dst),
            [
                "IMG_1.CR3",
                "IMG_1.CR3.xmp",
                "IMG_1_01.CR3.xmp",
                "IMG_1_02.CR3.xmp"
            ]
        );
        assert!(names(&f.src).is_empty());
    }

    #[test]
    fn stem_sidecar_moves_once_with_a_pair() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.src.join("IMG_1.xmp"), "lightroom");
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        let TransferOutcome::Done(done) = out else {
            panic!("{out:?}")
        };
        assert_eq!(done.len(), 4);
        assert_eq!(
            names(&f.dst),
            ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG", "IMG_1.xmp"]
        );
        assert!(names(&f.src).is_empty());
    }

    #[test]
    fn another_photos_sidecar_is_not_a_duplicate() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        // A real photo IMG_1_01.CR3 owns IMG_1_01.CR3.xmp.
        write(&f.src.join("IMG_1_01.CR3"), "other photo");
        write(&f.src.join("IMG_1_01.CR3.xmp"), "other photo's sidecar");
        let out = transfer_item(&[raw], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(names(&f.dst), ["IMG_1.CR3"]);
        assert_eq!(names(&f.src), ["IMG_1_01.CR3", "IMG_1_01.CR3.xmp"]);
    }

    #[test]
    fn keep_both_renames_extra_sidecars_to_match() {
        let f = fixture();
        let raw = raw_with_duplicates(&f);
        write(&f.src.join("IMG_1.xmp"), "lightroom");
        write(&f.dst.join("IMG_1.CR3"), "existing");
        let out = transfer_item(&[raw], &f.dst, OpKind::Move, ConflictPolicy::Rename);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(
            names(&f.dst),
            [
                "IMG_1.CR3",
                "IMG_1_1.CR3",
                "IMG_1_1.CR3.xmp",
                "IMG_1_1.xmp",
                "IMG_1_1_01.CR3.xmp",
                "IMG_1_1_02.CR3.xmp"
            ]
        );
        assert_eq!(
            fs::read_to_string(f.dst.join("IMG_1_1_02.CR3.xmp")).unwrap(),
            "v2"
        );
    }

    #[test]
    fn copy_brings_extra_sidecars_and_keeps_originals() {
        let f = fixture();
        let raw = raw_with_duplicates(&f);
        let out = transfer_item(&[raw], &f.dst, OpKind::Copy, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(names(&f.dst), names(&f.src));
        assert_eq!(names(&f.src).len(), 4);
    }

    #[test]
    fn taken_extra_sidecar_name_is_a_conflict() {
        let f = fixture();
        let raw = raw_with_duplicates(&f);
        write(&f.dst.join("IMG_1_02.CR3.xmp"), "someone else's");
        let sources = vec![raw];
        assert!(has_conflict(&sources, &f.dst));
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(
            fs::read_to_string(f.dst.join("IMG_1_02.CR3.xmp")).unwrap(),
            "someone else's"
        );
    }

    #[test]
    fn organizing_creates_dated_folders_and_moves_items_into_them() {
        use crate::organize::{OrganizePattern, plan};
        use chrono::NaiveDate;
        let f = fixture();
        let a = pair(&f); // IMG_1.CR3 (+ sidecar) and IMG_1.JPG
        let b = f.src.join("IMG_2.JPG");
        write(&b, "jpg");
        let c = f.src.join("IMG_3.JPG");
        write(&c, "jpg");
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d);
        let p = plan(
            &[day(14), day(14), day(15)],
            &f.dst,
            OrganizePattern::YearThenDay,
        );
        for (sources, dest) in [vec![a[0].clone(), a[1].clone()], vec![b], vec![c]]
            .iter()
            .zip(&p.destinations)
        {
            let out = transfer_item_creating_dest(
                sources,
                dest.as_ref().unwrap(),
                OpKind::Move,
                ConflictPolicy::Skip,
            );
            assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        }
        assert_eq!(names(&f.dst), ["2026"]);
        assert_eq!(
            names(&f.dst.join("2026/2026-11-14")),
            ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG", "IMG_2.JPG"]
        );
        assert_eq!(names(&f.dst.join("2026/2026-11-15")), ["IMG_3.JPG"]);
        assert!(names(&f.src).is_empty());
    }

    #[test]
    fn skipped_item_keeps_existing_dated_folders() {
        let f = fixture();
        let src = f.src.join("IMG_1.JPG");
        write(&src, "new");
        // The day folder already exists and the name is taken: Skip leaves the photo.
        let existing = f.dst.join("2026/2026-11-14");
        fs::create_dir_all(&existing).unwrap();
        write(&existing.join("IMG_1.JPG"), "existing");
        let out = transfer_item_creating_dest(
            std::slice::from_ref(&src),
            &existing,
            OpKind::Move,
            ConflictPolicy::Skip,
        );
        assert_eq!(out, TransferOutcome::Skipped);
        assert!(existing.exists(), "a folder that existed before is kept");
        assert_eq!(fs::read_to_string(&src).unwrap(), "new");
    }

    #[test]
    fn failed_item_removes_only_the_folders_it_created() {
        let f = fixture();
        let missing = f.src.join("gone.JPG"); // never written: the move fails
        let dest = f.dst.join("2026/2026-11-14");
        let out = transfer_item_creating_dest(
            std::slice::from_ref(&missing),
            &dest,
            OpKind::Move,
            ConflictPolicy::Skip,
        );
        assert!(matches!(out, TransferOutcome::Failed(_)), "{out:?}");
        assert!(
            !f.dst.join("2026").exists(),
            "created folders removed again"
        );
        assert!(f.dst.exists(), "the root existed before and is kept");
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
        let out = transfer_item(
            &[raw.clone(), missing],
            &f.dst,
            OpKind::Move,
            ConflictPolicy::Skip,
        );
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
        let Ok(src_dir) = tempfile::tempdir_in(shm) else {
            return;
        };
        let Ok(dst_dir) = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")) else {
            return;
        };
        let dev = |p: &Path| fs::metadata(p).unwrap().dev();
        if dev(src_dir.path()) == dev(dst_dir.path()) {
            eprintln!("same filesystem: skipping cross-fs test");
            return;
        }
        let p = src_dir.path().join("a.jpg");
        write(&p, "content");
        let out = transfer_item(
            std::slice::from_ref(&p),
            dst_dir.path(),
            OpKind::Move,
            ConflictPolicy::Skip,
        );
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert!(!p.exists());
        assert_eq!(
            fs::read_to_string(dst_dir.path().join("a.jpg")).unwrap(),
            "content"
        );
    }

    #[test]
    fn rollback_reports_incomplete_undos() {
        // Create a temp directory structure
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        let dst_dir = tmp.path().join("dst");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dst_dir).unwrap();

        let src = src_dir.join("IMG_1.CR3");
        let dst = dst_dir.join("IMG_1.CR3");

        // Create destination file (simulating a successfully transferred file)
        write(&dst, "content");

        // Remove the source directory to make rollback fail
        fs::remove_dir(&src_dir).unwrap();

        // Try to rollback
        let incomplete = rollback(&[(src.clone(), dst.clone())], OpKind::Move);

        // Should report the pair as incomplete
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0], (src, dst));
    }
}
