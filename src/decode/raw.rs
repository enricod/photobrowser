use std::path::Path;

use super::{DecodeError, Rgba, jpeg};
use crate::exif;
use crate::libraw::{Raw, flip_to_orientation};

/// Full view never uses a preview smaller than this.
const MIN_FULL_PREVIEW_EDGE: u32 = 1024;

fn needed_edge(target_edge: u32) -> u32 {
    if target_edge == 0 {
        MIN_FULL_PREVIEW_EDGE
    } else {
        target_edge.min(MIN_FULL_PREVIEW_EDGE)
    }
}

/// Best embedded preview for display, plus the EXIF orientation still to apply.
pub fn decode_raw_preview(path: &Path, target_edge: u32) -> Result<(Rgba, u8), DecodeError> {
    let needed = needed_edge(target_edge);

    if let Ok(meta) = exif::open_path(path)
        && let Some(data) = largest_exiv2_preview(&meta, needed)
        && let Ok(img) = jpeg::decode_jpeg(&data, target_edge)
    {
        return Ok((img, exif::orientation(&meta)));
    }

    let mut raw = Raw::open(path)?;
    let orientation = flip_to_orientation(raw.flip());
    if let Ok(Some(data)) = raw.thumbnail_jpeg() {
        let big_enough = jpeg::dimensions(&data).is_some_and(|(w, h)| w.max(h) >= needed);
        if big_enough && let Ok(img) = jpeg::decode_jpeg(&data, target_edge) {
            return Ok((img, orientation));
        }
    }
    Ok((raw.decode(true)?, 1))
}

fn largest_exiv2_preview(meta: &rexiv2::Metadata, needed: u32) -> Option<Vec<u8>> {
    let previews = meta.get_preview_images()?;
    let best = previews
        .into_iter()
        .filter(|p| {
            p.get_extension()
                .map(|e| e.trim_start_matches('.').eq_ignore_ascii_case("jpg"))
                .unwrap_or(false)
        })
        .max_by_key(|p| p.get_width() as u64 * p.get_height() as u64)?;
    if best.get_width().max(best.get_height()) < needed {
        return None;
    }
    best.get_data().ok()
}

/// Full-resolution demosaic (slow: 0.5–2 s). Upright.
pub fn decode_raw_full(path: &Path) -> Result<Rgba, DecodeError> {
    Ok(Raw::open(path)?.decode(false)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::path::PathBuf;

    /// RAW samples come from $PB_RAW_SAMPLES (a folder of your own RAW files).
    /// Without it these tests are skipped, loudly.
    fn samples() -> Vec<PathBuf> {
        let Ok(dir) = std::env::var("PB_RAW_SAMPLES") else {
            eprintln!("PB_RAW_SAMPLES not set: skipping RAW sample tests");
            return Vec::new();
        };
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| FileKind::from_path(p) == Some(FileKind::Raw))
            .collect()
    }

    #[test]
    fn needed_edge_rules() {
        assert_eq!(needed_edge(256), 256);
        assert_eq!(needed_edge(3840), 1024);
        assert_eq!(needed_edge(0), 1024);
    }

    #[test]
    fn previews_every_sample() {
        for p in samples() {
            let (img, orientation) =
                decode_raw_preview(&p, 256).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert!(
                img.long_edge() >= 256,
                "{}: {}x{}",
                p.display(),
                img.width,
                img.height
            );
            assert!((1..=8).contains(&orientation));
        }
    }

    #[test]
    fn full_decodes_every_sample() {
        for p in samples() {
            let img = decode_raw_full(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert!(img.long_edge() > 1000, "{}", p.display());
        }
    }
}
