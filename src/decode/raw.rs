use std::path::Path;

use super::{DecodeError, Rgba, helper, jpeg};
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
        && let Some(data) = exif::largest_jpeg_preview(&meta, needed)
        && let Ok(img) = jpeg::decode_jpeg(&data, target_edge)
    {
        return Ok((img, exif::orientation(&meta)));
    }
    isolated(
        || helper::preview_args(path, target_edge),
        helper::PREVIEW_TIMEOUT,
        || libraw_preview(path, target_edge),
    )
}

/// Full-resolution demosaic (slow: 0.5–2 s). Upright.
pub fn decode_raw_full(path: &Path) -> Result<Rgba, DecodeError> {
    isolated(
        || helper::full_args(path),
        helper::FULL_TIMEOUT,
        || libraw_full(path),
    )
    .map(|(img, _)| img)
}

/// Runs a LibRaw decode in a helper process (see `helper`), so a crash inside LibRaw cannot
/// take the app down. Unit tests run it in-process: the test binary cannot act as the helper.
/// If the helper cannot be started at all (e.g. the binary was replaced on disk), it falls
/// back to decoding in-process rather than failing every RAW.
#[cfg(not(test))]
fn isolated(
    args: impl FnOnce() -> Vec<std::ffi::OsString>,
    timeout: std::time::Duration,
    in_process: impl FnOnce() -> Result<(Rgba, u8), DecodeError>,
) -> Result<(Rgba, u8), DecodeError> {
    match helper::current_program() {
        Ok(program) => match helper::run_helper(&program, &args(), timeout) {
            Err(DecodeError::Io(e)) => {
                eprintln!("RAW decoder helper unavailable ({e}); decoding in-process");
                in_process()
            }
            other => other,
        },
        Err(e) => {
            eprintln!("RAW decoder helper unavailable ({e}); decoding in-process");
            in_process()
        }
    }
}

#[cfg(test)]
fn isolated(
    _args: impl FnOnce() -> Vec<std::ffi::OsString>,
    _timeout: std::time::Duration,
    in_process: impl FnOnce() -> Result<(Rgba, u8), DecodeError>,
) -> Result<(Rgba, u8), DecodeError> {
    in_process()
}

/// The LibRaw part of `decode_raw_preview`: the embedded thumbnail if big enough, else a
/// half-size demosaic. Runs inside the helper process.
pub fn libraw_preview(path: &Path, target_edge: u32) -> Result<(Rgba, u8), DecodeError> {
    let needed = needed_edge(target_edge);
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

/// Full-resolution LibRaw demosaic, already upright. Runs inside the helper process.
pub fn libraw_full(path: &Path) -> Result<(Rgba, u8), DecodeError> {
    Ok((Raw::open(path)?.decode(false)?, 1))
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
