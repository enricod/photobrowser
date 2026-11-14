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

/// Most pixels a single JPEG decode may produce: 256 MP, i.e. 1 GiB of RGBA. A header can
/// claim up to 65535×65535 (~17 GB); a failed allocation aborts the process, which no
/// `catch_unwind` can stop.
pub const MAX_DECODE_PIXELS: usize = 256 * 1024 * 1024;

/// Like `choose_scaling`, but if that would exceed `max_pixels`, the largest DCT downscale
/// that fits instead. `None` if even the smallest one does not fit.
pub fn choose_scaling_within(
    width: usize,
    height: usize,
    target_edge: u32,
    max_pixels: usize,
) -> Option<ScalingFactor> {
    let pixels = |f: &ScalingFactor| f.scale(width).saturating_mul(f.scale(height));
    let preferred = choose_scaling(width, height, target_edge);
    if pixels(&preferred) <= max_pixels {
        return Some(preferred);
    }
    Decompressor::supported_scaling_factors()
        .into_iter()
        .filter(|f| f.num() <= f.denom())
        .filter(|f| pixels(f) <= max_pixels)
        .max_by_key(pixels)
}

pub fn decode_jpeg(data: &[u8], target_edge: u32) -> Result<Rgba, DecodeError> {
    decode_jpeg_within(data, target_edge, MAX_DECODE_PIXELS)
}

/// `decode_jpeg` with an explicit pixel budget. An image larger than the budget is decoded
/// at the largest DCT downscale that fits, and the buffer is allocated fallibly, so an
/// oversized or lying header gives an error or a smaller image, never an abort.
fn decode_jpeg_within(
    data: &[u8],
    target_edge: u32,
    max_pixels: usize,
) -> Result<Rgba, DecodeError> {
    let mut d = Decompressor::new().map_err(jerr)?;
    let header = d.read_header(data).map_err(jerr)?;
    let factor = choose_scaling_within(header.width, header.height, target_edge, max_pixels)
        .ok_or_else(|| {
            DecodeError::Jpeg(format!(
                "{}×{} is too large to decode",
                header.width, header.height
            ))
        })?;
    d.set_scaling_factor(factor).map_err(jerr)?;
    let scaled = header.scaled(factor);
    let (w, h) = (scaled.width, scaled.height);
    let len = w * h * 4;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| DecodeError::Jpeg(format!("not enough memory for {w}×{h}")))?;
    pixels.resize(len, 0);
    d.decompress(
        data,
        Image {
            pixels: pixels.as_mut_slice(),
            width: w,
            pitch: w * 4,
            height: h,
            format: PixelFormat::RGBA,
        },
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
        assert!(
            img.long_edge() >= 100 && img.long_edge() < 200,
            "{}x{}",
            img.width,
            img.height
        );
    }

    #[test]
    fn huge_header_is_scaled_to_fit_the_budget() {
        let f = choose_scaling_within(60_000, 60_000, 0, MAX_DECODE_PIXELS).unwrap();
        assert!(f.num() < f.denom(), "must downscale: {f:?}");
        assert!(f.scale(60_000) * f.scale(60_000) <= MAX_DECODE_PIXELS);
    }

    #[test]
    fn normal_sizes_keep_their_scaling() {
        for (w, h, target) in [(8256, 5504, 0), (8256, 5504, 256), (800, 600, 100)] {
            assert_eq!(
                choose_scaling_within(w, h, target, MAX_DECODE_PIXELS),
                Some(choose_scaling(w, h, target)),
                "{w}x{h} target {target}"
            );
        }
    }

    #[test]
    fn decode_over_budget_returns_a_smaller_image() {
        let img = decode_jpeg_within(&sample(64, 64), 0, 32 * 32).unwrap();
        assert_eq!((img.width, img.height), (32, 32));
    }

    #[test]
    fn decode_beyond_smallest_scale_is_an_error() {
        // 64×64 at 1/8 is 8×8 = 64 px: a budget of 63 cannot be met.
        assert!(matches!(
            decode_jpeg_within(&sample(64, 64), 0, 63),
            Err(DecodeError::Jpeg(_))
        ));
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
