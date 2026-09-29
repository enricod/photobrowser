use std::path::Path;

use super::{DecodeError, Rgba};

/// PNG and TIFF (8/16-bit, any channel layout) → RGBA8.
pub fn decode_with_image_crate(path: &Path) -> Result<Rgba, DecodeError> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(|e| DecodeError::Image(e.to_string()))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Rgba::new(w, h, img.into_raw()))
}
