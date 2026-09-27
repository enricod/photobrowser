use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

pub fn to_xmp_value(rating: u8, rejected: bool) -> i32 {
    if rejected { -1 } else { rating.min(5) as i32 }
}

pub fn from_xmp_value(v: i32) -> XmpRating {
    if v < 0 { XmpRating::Rejected } else { XmpRating::Stars(v.min(5) as u8) }
}

pub fn sidecar_mtime_ns(file: &Path) -> Option<i64> {
    fs::metadata(sidecar_path(file)).ok().map(|m| mtime_ns(&m))
}

/// Reads the rating from `file`'s sidecar. `Ok(None)`: no sidecar or no rating in it.
pub fn read_rating(file: &Path) -> Result<Option<XmpRating>, XmpError> {
    crate::exif::init();
    let sidecar = sidecar_path(file);
    if !sidecar.exists() {
        return Ok(None);
    }
    let meta = rexiv2::Metadata::new_from_path(&sidecar)?;
    if !meta.has_tag(RATING_TAG) {
        return Ok(None);
    }
    let raw = meta.get_tag_string(RATING_TAG)?;
    Ok(raw.trim().parse::<i32>().ok().map(from_xmp_value))
}

/// Writes the rating into `file`'s sidecar, preserving everything else in it.
/// Creates the sidecar if missing or empty; refuses to touch one it cannot parse.
/// Returns the sidecar's mtime (ns) after writing.
pub fn write_rating(file: &Path, rating: u8, rejected: bool) -> Result<i64, XmpError> {
    crate::exif::init();
    let sidecar = sidecar_path(file);
    let needs_packet = match fs::metadata(&sidecar) {
        Ok(m) => m.len() == 0,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    if needs_packet {
        fs::write(&sidecar, EMPTY_PACKET)?;
    }
    let meta = rexiv2::Metadata::new_from_path(&sidecar)?;
    meta.set_tag_string(RATING_TAG, &to_xmp_value(rating, rejected).to_string())?;
    meta.save_to_file(&sidecar)?;
    Ok(mtime_ns(&fs::metadata(&sidecar)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/darktable.xmp");

    #[test]
    fn sidecar_path_appends_xmp() {
        assert_eq!(sidecar_path(Path::new("/p/IMG_1.CR3")), PathBuf::from("/p/IMG_1.CR3.xmp"));
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
        let meta = rexiv2::Metadata::new_from_path(sidecar_path(&file)).unwrap();
        assert_eq!(meta.get_tag_string("Xmp.darktable.history_end").unwrap(), "1");
        assert_eq!(meta.get_tag_string("Xmp.xmpMM.DerivedFrom").unwrap(), "IMG_0001.CR3");
        let content = fs::read_to_string(sidecar_path(&file)).unwrap();
        assert!(content.contains("exposure"), "history lost:\n{content}");
        assert!(content.contains("gz02eJxjYIAAAAAIAAE="), "params lost:\n{content}");
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
        assert_eq!(fs::read(sidecar_path(&file)).unwrap(), b"definitely not xmp");
    }
}
