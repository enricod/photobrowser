use std::io::Read;
use std::path::Path;
use std::sync::{Once, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::gexiv2::{self, Metadata};

static INIT: Once = Once::new();

/// Serialises exiv2's XMP encode against XMP decode.
///
/// `Exiv2::XmpParser::encode` (run by every save) deletes and re-registers each
/// custom namespace (e.g. `darktable`) in the XMP SDK's process-global table and
/// iterates exiv2's namespace registry without its mutex. A decode running in
/// between fails with "XMP Toolkit error 101: Unregistered schema namespace URI",
/// which exiv2 only logs: the sidecar opens with *no* XMP at all. Decodes are safe
/// among themselves (each SDK call takes the SDK's own lock), so opening takes the
/// shared side and saving the exclusive side. gexiv2 offers no way to pass a lock
/// to `XmpParser::initialize`, and that lock would only cover namespace
/// registration anyway, not the decode.
static EXIV2_LOCK: RwLock<()> = RwLock::new(());

/// Proof that the shared side of `EXIV2_LOCK` is held. The private field means only this
/// module can create one, so gexiv2 calls that require it cannot run without the lock.
pub struct Shared<'a> {
    _guard: RwLockReadGuard<'a, ()>,
}

/// Proof that the exclusive side of `EXIV2_LOCK` is held. See `Shared`.
pub struct Exclusive<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
}

fn lock_shared() -> Shared<'static> {
    Shared {
        _guard: EXIV2_LOCK.read().unwrap_or_else(PoisonError::into_inner),
    }
}

fn lock_exclusive() -> Exclusive<'static> {
    Exclusive {
        _guard: EXIV2_LOCK.write().unwrap_or_else(PoisonError::into_inner),
    }
}

/// Initialises exiv2 once per process. Call before any gexiv2 use.
pub fn init() {
    INIT.call_once(|| {
        gexiv2::initialize().expect("failed to initialise exiv2");
        gexiv2::mute_log();
        let _ = gexiv2::register_xmp_namespace("http://darktable.sf.net/", "darktable");
    });
}

/// Opens a file's metadata.
pub fn open_path(path: &Path) -> gexiv2::Result<Metadata> {
    init();
    Metadata::new_from_path(path.as_os_str(), &lock_shared())
}

/// Parses metadata from an in-memory image.
pub fn open_buffer(data: &[u8]) -> gexiv2::Result<Metadata> {
    init();
    Metadata::new_from_buffer(data, &lock_shared())
}

/// Writes metadata back to `path`.
pub fn save(meta: &Metadata, path: &Path) -> gexiv2::Result<()> {
    init();
    meta.save_to_file(path.as_os_str(), &lock_exclusive())
}

/// Data of the largest embedded JPEG preview whose long edge is at least `needed` px.
/// Preview enumeration and extraction read through exiv2, so they hold the shared side of
/// the lock like `open_path`.
pub fn largest_jpeg_preview(meta: &Metadata, needed: u32) -> Option<Vec<u8>> {
    init();
    let shared = lock_shared();
    let previews = meta.get_preview_images(&shared);
    let best = previews
        .iter()
        .filter(|p| {
            p.extension()
                .is_some_and(|e| is_jpeg_extension(e.to_bytes()))
        })
        .max_by_key(|p| p.width() as u64 * p.height() as u64)?;
    if best.width().max(best.height()) < needed {
        return None;
    }
    best.data().ok()
}

/// True for a preview extension of "jpg", ignoring case and leading dots (exiv2 gives ".jpg").
fn is_jpeg_extension(ext: &[u8]) -> bool {
    let start = ext.iter().take_while(|&&b| b == b'.').count();
    ext[start..].eq_ignore_ascii_case(b"jpg")
}

/// Parses EXIF "YYYY:MM:DD HH:MM:SS" as a naive timestamp (treated as UTC; used only for sorting).
pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('\0');
    chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

/// EXIF orientation 1–8 (1 when missing or invalid).
pub fn orientation(meta: &Metadata) -> u8 {
    match meta.get_tag_long("Exif.Image.Orientation") {
        Ok(v @ 1..=8) => v as u8,
        _ => 1,
    }
}

/// How much of a file `capture_time` reads first. The date sits near the start of JPEG,
/// CR2/CR3, ORF, NEF, ARW, HEIC and most other camera files, while opening the whole file
/// makes exiv2 read far more of it: on an SMB share via GVFS, ~0.08 s instead of ~0.5 s
/// per photo. Large enough for a JPEG's whole EXIF segment (up to 64 KB) after its JFIF
/// header; a smaller prefix would cut that segment off and pay for a second, full read.
const CAPTURE_TIME_PREFIX: u64 = 128 * 1024;

pub fn capture_time(path: &Path) -> Option<i64> {
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(CAPTURE_TIME_PREFIX)
        .read_to_end(&mut head)
        .ok()?;
    if let Some(t) = open_buffer(&head).ok().and_then(|m| date_of(&m)) {
        return Some(t);
    }
    if (head.len() as u64) < CAPTURE_TIME_PREFIX {
        return None; // that was the whole file
    }
    // Metadata further in (e.g. a PNG's eXIf chunk after the image data).
    date_of(&open_path(path).ok()?)
}

fn date_of(meta: &Metadata) -> Option<i64> {
    ["Exif.Photo.DateTimeOriginal", "Exif.Image.DateTime"]
        .iter()
        .find_map(|tag| {
            meta.get_tag_string(tag)
                .ok()
                .and_then(|s| parse_exif_datetime(&s))
        })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifInfo {
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<f64>,
    pub iso: Option<i32>,
    pub focal_length: Option<f64>,
}

impl ExifInfo {
    /// e.g. "Canon EOS R6 · RF24-105mm · 1/250 s · f/4 · ISO 400 · 50 mm"
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.camera.clone());
        parts.extend(self.lens.clone());
        parts.extend(self.exposure.clone());
        parts.extend(self.aperture.map(|a| format!("f/{}", trim_float(a))));
        parts.extend(self.iso.map(|i| format!("ISO {i}")));
        parts.extend(self.focal_length.map(|f| format!("{f:.0} mm")));
        parts.join(" · ")
    }
}

pub fn read_info(path: &Path) -> ExifInfo {
    let Ok(meta) = open_path(path) else {
        return ExifInfo::default();
    };
    let s = |tag: &str| {
        meta.get_tag_string(tag)
            .ok()
            .map(|v| v.trim().trim_end_matches('\0').to_string())
            .filter(|v| !v.is_empty())
    };
    let camera = match (s("Exif.Image.Make"), s("Exif.Image.Model")) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (None, model) => model,
        (make, None) => make,
    };
    ExifInfo {
        camera,
        lens: s("Exif.Photo.LensModel"),
        exposure: meta
            .get_exposure_time()
            .map(|(num, den)| format_exposure(num, den)),
        aperture: meta.get_fnumber(),
        iso: meta.get_iso_speed(),
        focal_length: meta.get_focal_length(),
    }
}

pub fn format_exposure(num: i32, den: i32) -> String {
    if num <= 0 || den <= 0 {
        return "?".into();
    }
    let secs = num as f64 / den as f64;
    if secs >= 1.0 {
        format!("{} s", trim_float(secs))
    } else {
        format!("1/{} s", (1.0 / secs).round() as i64)
    }
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{set_exif, write_jpeg};

    #[test]
    fn capture_time_of_file_larger_than_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.jpg");
        write_jpeg(&p, 8, 8, [1, 2, 3]);
        set_exif(&p, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        // Trailing data after the image, as in a file with a large embedded preview.
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.extend(std::iter::repeat_n(0u8, 2 * CAPTURE_TIME_PREFIX as usize));
        std::fs::write(&p, bytes).unwrap();
        assert_eq!(capture_time(&p), Some(1_788_257_730));
    }

    #[test]
    fn capture_time_beyond_prefix_falls_back_to_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("late.jpg");
        write_jpeg(&p, 8, 8, [1, 2, 3]);
        set_exif(&p, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        // Push the EXIF segment past the prefix with application segments right after SOI.
        let bytes = std::fs::read(&p).unwrap();
        let mut out = bytes[..2].to_vec();
        let payload = 65_000usize;
        let segments = CAPTURE_TIME_PREFIX as usize / payload + 1;
        for _ in 0..segments {
            out.extend([0xFF, 0xEF]);
            out.extend(((payload + 2) as u16).to_be_bytes());
            out.extend(std::iter::repeat_n(0u8, payload));
        }
        out.extend(&bytes[2..]);
        std::fs::write(&p, out).unwrap();
        assert_eq!(capture_time(&p), Some(1_788_257_730));
    }

    #[test]
    fn capture_time_without_exif_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plain.jpg");
        write_jpeg(&p, 8, 8, [1, 2, 3]);
        assert_eq!(capture_time(&p), None);
        assert_eq!(capture_time(&dir.path().join("missing.jpg")), None);
    }

    #[test]
    fn parses_exif_datetime() {
        assert_eq!(
            parse_exif_datetime("2026:09:01 10:15:30"),
            Some(1_788_257_730)
        );
        assert_eq!(
            parse_exif_datetime("2026:09:01 10:15:30\0"),
            Some(1_788_257_730)
        );
        assert_eq!(parse_exif_datetime("garbage"), None);
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
    }

    #[test]
    fn formats_exposure() {
        assert_eq!(format_exposure(1, 250), "1/250 s");
        assert_eq!(format_exposure(10, 2500), "1/250 s");
        assert_eq!(format_exposure(2, 1), "2 s");
        assert_eq!(format_exposure(13, 10), "1.3 s");
        assert_eq!(format_exposure(0, 1), "?");
    }

    #[test]
    fn reads_capture_time_and_orientation_from_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        write_jpeg(&path, 16, 16, [0, 0, 255]);
        set_exif(&path, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        set_exif(&path, "Exif.Image.Orientation", "6");
        assert_eq!(capture_time(&path), Some(1_788_257_730));
        let meta = open_path(&path).unwrap();
        assert_eq!(orientation(&meta), 6);
    }

    #[test]
    fn missing_metadata_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.cr3");
        std::fs::write(&path, b"not an image").unwrap();
        assert_eq!(capture_time(&path), None);
        assert_eq!(read_info(&path), ExifInfo::default());
    }

    #[test]
    fn summary_joins_known_fields() {
        let info = ExifInfo {
            camera: Some("Canon EOS R6".into()),
            exposure: Some("1/250 s".into()),
            aperture: Some(4.0),
            iso: Some(400),
            ..Default::default()
        };
        assert_eq!(info.summary(), "Canon EOS R6 · 1/250 s · f/4 · ISO 400");
    }

    /// The guard parameters make the compiler reject any locked call made without
    /// `EXIV2_LOCK`. The one way around them is calling the C function directly, so each
    /// locked entry point must be called exactly once in `gexiv2.rs`: inside its guarded
    /// method (`Preview::data` is guarded through the borrow `get_preview_images` returns).
    #[test]
    fn orientation_falls_back_to_normal_outside_1_to_8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o.jpg");
        write_jpeg(&path, 8, 8, [0, 0, 0]);
        let read = |p: &Path| orientation(&open_path(p).unwrap());
        assert_eq!(read(&path), 1, "missing tag");
        for (value, expected) in [("6", 6), ("8", 8), ("0", 1), ("9", 1), ("65535", 1)] {
            set_exif(&path, "Exif.Image.Orientation", value);
            assert_eq!(read(&path), expected, "Orientation={value}");
        }
    }

    #[test]
    fn get_tag_long_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jpg");
        write_jpeg(&path, 8, 8, [0, 0, 0]);
        set_exif(&path, "Exif.Image.ImageWidth", "4294967295");
        let meta = open_path(&path).unwrap();
        assert_eq!(
            meta.get_tag_long("Exif.Image.ImageWidth"),
            Ok(4_294_967_295)
        );
        assert!(matches!(
            meta.get_tag_long("Bogus.x.y"),
            Err(gexiv2::Error::Gexiv2(_))
        ));
        assert_eq!(
            meta.get_tag_long("Exif.Image.Orien\0tation"),
            Err(gexiv2::Error::Nul)
        );
    }

    #[test]
    fn jpeg_extension_matching() {
        for ext in [".jpg", ".JPG", "jpg", ".Jpg"] {
            assert!(is_jpeg_extension(ext.as_bytes()), "{ext}");
        }
        for ext in [".jpeg", ".tif", ".jpgx", "", "."] {
            assert!(!is_jpeg_extension(ext.as_bytes()), "{ext}");
        }
    }

    #[test]
    fn locked_ffi_calls_only_in_guarded_methods() {
        const LOCKED: [&str; 5] = [
            "gexiv2_metadata_open_path",
            "gexiv2_metadata_open_buf",
            "gexiv2_metadata_save_file",
            "gexiv2_metadata_get_preview_properties",
            "gexiv2_metadata_try_get_preview_image",
        ];
        let text = include_str!("gexiv2.rs");
        for name in LOCKED {
            let calls = text.matches(&format!("sys::{name}(")).count();
            assert_eq!(
                calls, 1,
                "{name} must be called exactly once, in its guarded method"
            );
        }
    }
}
