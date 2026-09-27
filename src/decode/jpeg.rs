use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor};

use super::{DecodeError, Rgba};

fn jerr(e: turbojpeg::Error) -> DecodeError {
    DecodeError::Jpeg(e.to_string())
}

/// Image dimensions from the JPEG header, without decoding.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let header = Decompressor::new().ok()?.read_header(data).ok()?;
    Some((header.width as u32, header.height as u32))
}

/// Smallest supported DCT downscale whose long edge still covers `target_edge` (0 = full size).
pub fn choose_scaling(width: usize, height: usize, target_edge: u32) -> ScalingFactor {
    let one = ScalingFactor::new(1, 1);
    if target_edge == 0 {
        return one;
    }
    let long = width.max(height);
    Decompressor::supported_scaling_factors()
        .into_iter()
        .filter(|f| f.num() <= f.denom())
        .filter(|f| f.scale(long) >= target_edge as usize)
        .min_by_key(|f| f.scale(long))
        .unwrap_or(one)
}

pub fn decode_jpeg(data: &[u8], target_edge: u32) -> Result<Rgba, DecodeError> {
    let mut d = Decompressor::new().map_err(jerr)?;
    let header = d.read_header(data).map_err(jerr)?;
    let factor = choose_scaling(header.width, header.height, target_edge);
    d.set_scaling_factor(factor).map_err(jerr)?;
    let scaled = header.scaled(factor);
    let (w, h) = (scaled.width, scaled.height);
    let mut pixels = vec![0u8; w * h * 4];
    d.decompress(
        data,
        Image { pixels: pixels.as_mut_slice(), width: w, pitch: w * 4, height: h, format: PixelFormat::RGBA },
    )
    .map_err(jerr)?;
    Ok(Rgba::new(w as u32, h as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::write_jpeg;

    fn sample(w: u32, h: u32) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jpg");
        write_jpeg(&path, w, h, [200, 100, 50]);
        std::fs::read(path).unwrap()
    }

    #[test]
    fn decodes_full_size() {
        let img = decode_jpeg(&sample(800, 600), 0).unwrap();
        assert_eq!((img.width, img.height), (800, 600));
        let [r, g, b, a] = img.pixel(400, 300);
        assert!(r > 180 && g > 80 && g < 120 && b < 70 && a == 255);
    }

    #[test]
    fn decodes_at_reduced_scale() {
        let img = decode_jpeg(&sample(800, 600), 100).unwrap();
        assert!(img.long_edge() >= 100 && img.long_edge() < 200, "{}x{}", img.width, img.height);
    }

    #[test]
    fn reads_dimensions() {
        assert_eq!(dimensions(&sample(320, 240)), Some((320, 240)));
        assert_eq!(dimensions(b"nope"), None);
    }

    #[test]
    fn garbage_jpeg_is_error() {
        assert!(decode_jpeg(b"definitely not a jpeg", 0).is_err());
    }

    #[test]
    fn truncated_jpeg_does_not_panic() {
        let data = sample(800, 600);
        let _ = decode_jpeg(&data[..data.len() / 3], 256); // Ok (partial) or Err, never a panic
    }
}
