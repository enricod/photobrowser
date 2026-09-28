use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::exif;
use crate::media::mtime_ns;

pub const RATING_TAG: &str = "Xmp.xmp.Rating";

const EMPTY_PACKET: &str = concat!(
    "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
    " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
    "  <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"/>\n",
    " </rdf:RDF>\n",
    "</x:xmpmeta>\n",
    "<?xpacket end=\"w\"?>\n",
);

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("exiv2: {0}")]
    Exiv2(#[from] rexiv2::Rexiv2Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmpRating {
    Stars(u8),
    Rejected,
}

/// `IMG_001.CR3` → `IMG_001.CR3.xmp` (darktable naming).
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".xmp");
    PathBuf::from(s)
}

pub fn is_sidecar(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

pub fn to_xmp_value(rating: u8, rejected: bool) -> i32 {
    if rejected { -1 } else { rating.min(5) as i32 }
}

pub fn from_xmp_value(v: i32) -> XmpRating {
    if v < 0 {
        XmpRating::Rejected
    } else {
        XmpRating::Stars(v.min(5) as u8)
    }
}

pub fn sidecar_mtime_ns(file: &Path) -> Option<i64> {
    fs::metadata(sidecar_path(file)).ok().map(|m| mtime_ns(&m))
}

/// Reads the rating from `file`'s sidecar. `Ok(None)`: no sidecar or no rating in it.
pub fn read_rating(file: &Path) -> Result<Option<XmpRating>, XmpError> {
    let sidecar = sidecar_path(file);
    if !sidecar.exists() {
        return Ok(None);
    }
    let meta = exif::open_path(&sidecar)?;
    if !meta.has_tag(RATING_TAG) {
        return Ok(None);
    }
    let raw = meta.get_tag_string(RATING_TAG)?;
    Ok(raw.trim().parse::<i32>().ok().map(from_xmp_value))
}

/// Temporary file a sidecar write goes through: hidden (so scans ignore it), same folder
/// (so the final `rename` is atomic).
fn temp_path(sidecar: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(sidecar.file_name().unwrap_or_default());
    name.push(".pbtmp");
    sidecar.with_file_name(name)
}

/// Writes the rating into `file`'s sidecar, preserving everything else in it.
/// Creates the sidecar if missing or empty; refuses to touch one it cannot parse.
/// Atomic: the edit is made on a temporary copy, synced, then renamed over the sidecar,
/// so a crash or a failed save never leaves a truncated or half-written sidecar.
/// Returns the sidecar's mtime (ns) after writing.
pub fn write_rating(file: &Path, rating: u8, rejected: bool) -> Result<i64, XmpError> {
    let sidecar = sidecar_path(file);
    let needs_packet = match fs::metadata(&sidecar) {
        Ok(m) => m.len() == 0,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    let temp = temp_path(&sidecar);
    let result = write_via_temp(&sidecar, &temp, needs_packet, rating, rejected);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_via_temp(
    sidecar: &Path,
    temp: &Path,
    needs_packet: bool,
    rating: u8,
    rejected: bool,
) -> Result<i64, XmpError> {
    if needs_packet {
        fs::write(temp, EMPTY_PACKET)?;
    } else {
        fs::copy(sidecar, temp)?;
    }
    let meta = exif::open_path(temp)?;
    meta.set_tag_string(RATING_TAG, &to_xmp_value(rating, rejected).to_string())?;
    exif::save(&meta, temp)?;
    fs::File::open(temp)?.sync_all()?;
    fs::rename(temp, sidecar)?;
    Ok(mtime_ns(&fs::metadata(sidecar)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/darktable.xmp");

    #[test]
    fn sidecar_path_appends_xmp() {
        assert_eq!(
            sidecar_path(Path::new("/p/IMG_1.CR3")),
            PathBuf::from("/p/IMG_1.CR3.xmp")
        );
        assert!(is_sidecar(Path::new("/p/IMG_1.CR3.xmp")));
        assert!(!is_sidecar(Path::new("/p/IMG_1.CR3")));
    }

    #[test]
    fn maps_values() {
        assert_eq!(to_xmp_value(3, false), 3);
        assert_eq!(to_xmp_value(3, true), -1);
        assert_eq!(to_xmp_value(9, false), 5);
        assert_eq!(from_xmp_value(-1), XmpRating::Rejected);
        assert_eq!(from_xmp_value(4), XmpRating::Stars(4));
        assert_eq!(from_xmp_value(7), XmpRating::Stars(5));
    }

    #[test]
    fn missing_sidecar_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_rating(&dir.path().join("a.jpg")).unwrap(), None);
    }

    #[test]
    fn write_creates_sidecar_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_1.CR3");
        let mtime = write_rating(&file, 4, false).unwrap();
        assert_eq!(sidecar_mtime_ns(&file), Some(mtime));
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(4)));
    }

    #[test]
    fn reject_roundtrips_as_minus_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_1.CR3");
        write_rating(&file, 3, true).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Rejected));
        let content = fs::read_to_string(sidecar_path(&file)).unwrap();
        assert!(content.contains("-1"), "{content}");
    }

    #[test]
    fn preserves_darktable_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_0001.CR3");
        fs::copy(FIXTURE, sidecar_path(&file)).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(2)));

        write_rating(&file, 5, false).unwrap();

        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(5)));
        let meta = exif::open_path(&sidecar_path(&file)).unwrap();
        assert_eq!(
            meta.get_tag_string("Xmp.darktable.history_end").unwrap(),
            "1"
        );
        assert_eq!(
            meta.get_tag_string("Xmp.xmpMM.DerivedFrom").unwrap(),
            "IMG_0001.CR3"
        );
        let content = fs::read_to_string(sidecar_path(&file)).unwrap();
        assert!(content.contains("exposure"), "history lost:\n{content}");
        assert!(
            content.contains("gz02eJxjYIAAAAAIAAE="),
            "params lost:\n{content}"
        );
    }

    #[test]
    fn sidecar_without_rating_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), EMPTY_PACKET).unwrap();
        assert_eq!(read_rating(&file).unwrap(), None);
    }

    #[test]
    fn empty_sidecar_is_replaced_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), b"").unwrap();
        assert!(read_rating(&file).is_err() || read_rating(&file).unwrap().is_none());
        write_rating(&file, 2, false).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(2)));
    }

    #[test]
    fn garbage_sidecar_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), b"definitely not xmp").unwrap();
        assert!(read_rating(&file).is_err());
        assert!(write_rating(&file, 2, false).is_err());
        assert_eq!(
            fs::read(sidecar_path(&file)).unwrap(),
            b"definitely not xmp"
        );
        assert!(
            !temp_path(&sidecar_path(&file)).exists(),
            "temp left behind"
        );
    }

    #[test]
    fn temp_path_is_hidden_next_to_sidecar() {
        assert_eq!(
            temp_path(Path::new("/p/IMG_1.CR3.xmp")),
            PathBuf::from("/p/.IMG_1.CR3.xmp.pbtmp")
        );
    }

    #[test]
    fn failed_write_leaves_sidecar_identical_and_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_0001.CR3");
        fs::copy(FIXTURE, sidecar_path(&file)).unwrap();
        let before = fs::read(sidecar_path(&file)).unwrap();
        // A read-only folder: the temporary copy cannot be created.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dir.path().join(".probe");
        if fs::write(&probe, b"").is_ok() {
            // Running with CAP_DAC_OVERRIDE (root): permissions can't provoke a failure.
            let _ = fs::remove_file(&probe);
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("permissions not enforced: skipping");
            return;
        }
        let result = write_rating(&file, 5, false);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(sidecar_path(&file)).unwrap(), before);
        assert!(
            !temp_path(&sidecar_path(&file)).exists(),
            "temp left behind"
        );
    }

    #[test]
    fn write_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_0001.CR3");
        fs::copy(FIXTURE, sidecar_path(&file)).unwrap();
        write_rating(&file, 1, false).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("IMG_0001.CR3.xmp")]);
    }

    /// Regression: exiv2's XMP encode (save) deletes and re-registers every custom
    /// namespace (e.g. `darktable`) in the XMP SDK's global table, so a concurrent
    /// decode used to silently drop the whole packet (rating read as `None`, and a
    /// write then saved a sidecar without the darktable history).
    #[test]
    fn concurrent_reads_and_writes_keep_darktable_sidecars_intact() {
        let dir = tempfile::tempdir().unwrap();
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let d = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    for i in 0..60 {
                        let file = d.join(format!("IMG_{t}_{i}.CR3"));
                        fs::copy(FIXTURE, sidecar_path(&file)).unwrap();
                        if t % 2 == 0 {
                            assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(2)));
                        } else {
                            write_rating(&file, 4, false).unwrap();
                            let content = fs::read_to_string(sidecar_path(&file)).unwrap();
                            assert!(
                                content.contains("gz02eJxjYIAAAAAIAAE="),
                                "history lost:\n{content}"
                            );
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
