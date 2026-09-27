use std::path::Path;
use std::sync::Once;

static INIT: Once = Once::new();

/// Initialises exiv2 once per process. Call before any rexiv2 use.
pub fn init() {
    INIT.call_once(|| {
        rexiv2::initialize().expect("failed to initialise exiv2");
        rexiv2::set_log_level(rexiv2::LogLevel::MUTE);
        let _ = rexiv2::register_xmp_namespace("http://darktable.sf.net/", "darktable");
    });
}

/// Parses EXIF "YYYY:MM:DD HH:MM:SS" as a naive timestamp (treated as UTC; used only for sorting).
pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('\0');
    chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

/// EXIF orientation 1–8 (1 when missing or invalid).
pub fn orientation(meta: &rexiv2::Metadata) -> u8 {
    let v = meta.get_tag_numeric("Exif.Image.Orientation");
    if (1..=8).contains(&v) { v as u8 } else { 1 }
}

pub fn capture_time(path: &Path) -> Option<i64> {
    init();
    let meta = rexiv2::Metadata::new_from_path(path).ok()?;
    ["Exif.Photo.DateTimeOriginal", "Exif.Image.DateTime"]
        .iter()
        .find_map(|tag| meta.get_tag_string(tag).ok().and_then(|s| parse_exif_datetime(&s)))
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
    init();
    let Ok(meta) = rexiv2::Metadata::new_from_path(path) else {
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
            .map(|r| format_exposure(*r.numer(), *r.denom())),
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
    fn parses_exif_datetime() {
        assert_eq!(parse_exif_datetime("2026:09:01 10:15:30"), Some(1_788_257_730));
        assert_eq!(parse_exif_datetime("2026:09:01 10:15:30\0"), Some(1_788_257_730));
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
        let meta = rexiv2::Metadata::new_from_path(&path).unwrap();
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
}
