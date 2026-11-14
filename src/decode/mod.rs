pub mod generic;
pub mod jpeg;
pub mod pixels;
pub mod raw;

use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use crate::media::{FileKind, MediaFile};
pub use pixels::{Quality, Rgba};

/// Long edge of cached thumbnails, in pixels. Changing it changes cached output: bump
/// `thumbcache::CACHE_VERSION` too.
pub const THUMB_EDGE: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("JPEG: {0}")]
    Jpeg(String),
    #[error("image: {0}")]
    Image(String),
    #[error("RAW: {0}")]
    Raw(String),
    #[error("decoder crashed")]
    Panic,
}

/// Decodes `file` for display: long edge ≤ `max_edge` (0 = native), upright.
pub fn decode_for_display(
    file: &MediaFile,
    max_edge: u32,
    quality: Quality,
) -> Result<Rgba, DecodeError> {
    let (img, orientation) = match file.kind {
        FileKind::Jpeg => {
            let data = fs::read(&file.path)?;
            (
                jpeg::decode_jpeg(&data, max_edge)?,
                orientation_of_buffer(&data),
            )
        }
        FileKind::Png | FileKind::Tiff => (
            generic::decode_with_image_crate(&file.path)?,
            orientation_of_path(&file.path),
        ),
        FileKind::Raw => raw::decode_raw_preview(&file.path, max_edge)?,
    };
    let img = pixels::resize_to_fit(img, max_edge, quality);
    Ok(pixels::apply_orientation(img, orientation))
}

/// Native-resolution decode for the 100% view: the RAW of the item if it has one.
pub fn decode_actual_size(item: &crate::media::Item) -> Result<Rgba, DecodeError> {
    match item.raw_file() {
        Some(raw_file) => raw::decode_raw_full(&raw_file.path),
        None => decode_for_display(item.display_file(), 0, Quality::High),
    }
}

impl From<crate::libraw::LibRawError> for DecodeError {
    fn from(e: crate::libraw::LibRawError) -> Self {
        DecodeError::Raw(e.to_string())
    }
}

fn orientation_of_buffer(data: &[u8]) -> u8 {
    crate::exif::open_buffer(data)
        .map(|m| crate::exif::orientation(&m))
        .unwrap_or(1)
}

fn orientation_of_path(path: &Path) -> u8 {
    crate::exif::open_path(path)
        .map(|m| crate::exif::orientation(&m))
        .unwrap_or(1)
}

/// Runs a decoder, turning a panic inside it into `DecodeError::Panic`.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(Err(DecodeError::Panic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{set_exif, write_png, write_tiff, write_two_tone_jpeg};
    use std::path::PathBuf;

    fn media(path: PathBuf) -> MediaFile {
        let kind = FileKind::from_path(&path).unwrap();
        MediaFile {
            path,
            kind,
            size: 0,
            mtime_ns: 0,
        }
    }

    #[test]
    fn jpeg_thumbnail_fits_thumb_edge() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        write_two_tone_jpeg(&p, 1200, 800);
        let img = decode_for_display(&media(p), THUMB_EDGE, Quality::Fast).unwrap();
        assert_eq!((img.width, img.height), (256, 170));
    }

    #[test]
    fn jpeg_orientation_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        write_two_tone_jpeg(&p, 64, 32); // left red, right green
        set_exif(&p, "Exif.Image.Orientation", "6");
        let img = decode_for_display(&media(p), 0, Quality::High).unwrap();
        assert_eq!((img.width, img.height), (32, 64));
        let top = img.pixel(16, 4);
        let bottom = img.pixel(16, 60);
        assert!(top[0] > 200 && top[1] < 80, "top should be red: {top:?}");
        assert!(
            bottom[1] > 200 && bottom[0] < 80,
            "bottom should be green: {bottom:?}"
        );
    }

    #[test]
    fn decodes_png_and_tiff() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        let tif = dir.path().join("a.tif");
        write_png(&png, 600, 300, [1, 2, 3, 255]);
        write_tiff(&tif, 300, 600, [4, 5, 6]);
        let a = decode_for_display(&media(png), 200, Quality::Fast).unwrap();
        let b = decode_for_display(&media(tif), 200, Quality::Fast).unwrap();
        assert_eq!((a.width, a.height), (200, 100));
        assert_eq!((b.width, b.height), (100, 200));
        assert_eq!(b.pixel(50, 50), [4, 5, 6, 255]);
    }

    #[test]
    fn corrupt_png_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.png");
        std::fs::write(&p, b"\x89PNG\r\n\x1a\nbroken").unwrap();
        assert!(decode_for_display(&media(p), 256, Quality::Fast).is_err());
    }

    #[test]
    fn zero_byte_jpeg_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.jpg");
        std::fs::write(&p, b"").unwrap();
        assert!(decode_for_display(&media(p), 256, Quality::Fast).is_err());
    }

    #[test]
    fn guarded_turns_panic_into_error() {
        let r: Result<(), DecodeError> = guarded(|| panic!("decoder bug"));
        assert!(matches!(r, Err(DecodeError::Panic)));
    }

    #[test]
    fn fake_raw_is_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake.CR3");
        std::fs::write(&p, b"not a raw file").unwrap();
        assert!(guarded(|| decode_for_display(&media(p), 256, Quality::Fast)).is_err());
    }
}
