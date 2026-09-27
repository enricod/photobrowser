use fast_image_resize::images::Image as FrImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Rgba {
        assert_eq!(data.len(), width as usize * height as usize * 4, "RGBA buffer size");
        Rgba { width, height, data }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Bilinear: thumbnails.
    Fast,
    /// Lanczos3: full view.
    High,
}

/// Scales (width, height) so the long edge is at most `max_edge`; 0 = unlimited; never upscales.
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if max_edge == 0 || width.max(height) <= max_edge {
        return (width, height);
    }
    let scale = |short: u32, long: u32| ((short as u64 * max_edge as u64) / long as u64).max(1) as u32;
    if width >= height {
        (max_edge, scale(height, width))
    } else {
        (scale(width, height), max_edge)
    }
}

pub fn resize_to_fit(img: Rgba, max_edge: u32, quality: Quality) -> Rgba {
    let (w, h) = fit_within(img.width, img.height, max_edge);
    if (w, h) == (img.width, img.height) {
        return img;
    }
    let src = FrImage::from_vec_u8(img.width, img.height, img.data, PixelType::U8x4)
        .expect("buffer matches dimensions");
    let mut dst = FrImage::new(w, h, PixelType::U8x4);
    let filter = match quality {
        Quality::Fast => FilterType::Bilinear,
        Quality::High => FilterType::Lanczos3,
    };
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(filter))
        .use_alpha(false);
    Resizer::new()
        .resize(&src, &mut dst, &options)
        .expect("same pixel type");
    Rgba::new(w, h, dst.into_vec())
}

/// Applies EXIF orientation (1–8) so the result is upright.
pub fn apply_orientation(img: Rgba, orientation: u8) -> Rgba {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (dw, dh) = if orientation >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; img.data.len()];
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match orientation {
                2 => (w - 1 - dx, dy),
                3 => (w - 1 - dx, h - 1 - dy),
                4 => (dx, h - 1 - dy),
                5 => (dy, dx),
                6 => (dy, h - 1 - dx),
                7 => (w - 1 - dy, h - 1 - dx),
                8 => (w - 1 - dy, dx),
                _ => unreachable!(),
            };
            let s = (sy * w + sx) * 4;
            let d = (dy * dw + dx) * 4;
            out[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    Rgba::new(dw as u32, dh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    /// 2×1 image: red at x=0, green at x=1.
    fn red_green() -> Rgba {
        Rgba::new(2, 1, [RED, GREEN].concat())
    }

    #[test]
    fn fit_within_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(6000, 4000, 256), (256, 170));
        assert_eq!(fit_within(4000, 6000, 256), (170, 256));
        assert_eq!(fit_within(100, 50, 256), (100, 50));
        assert_eq!(fit_within(6000, 4000, 0), (6000, 4000));
        assert_eq!(fit_within(10000, 1, 100), (100, 1));
    }

    #[test]
    fn resize_produces_requested_size() {
        let img = Rgba::new(400, 200, vec![128; 400 * 200 * 4]);
        let out = resize_to_fit(img, 100, Quality::High);
        assert_eq!((out.width, out.height), (100, 50));
        assert_eq!(out.data.len(), 100 * 50 * 4);
    }

    #[test]
    fn orientation_1_is_identity() {
        assert_eq!(apply_orientation(red_green(), 1), red_green());
    }

    #[test]
    fn orientation_3_rotates_180() {
        let out = apply_orientation(red_green(), 3);
        assert_eq!((out.pixel(0, 0), out.pixel(1, 0)), (GREEN, RED));
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        let out = apply_orientation(red_green(), 6);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!((out.pixel(0, 0), out.pixel(0, 1)), (RED, GREEN));
    }

    #[test]
    fn orientation_8_rotates_counter_clockwise() {
        let out = apply_orientation(red_green(), 8);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!((out.pixel(0, 0), out.pixel(0, 1)), (GREEN, RED));
    }

    #[test]
    fn orientations_5_and_7_swap_dimensions() {
        let img = Rgba::new(3, 2, vec![0; 24]);
        for o in [5, 7] {
            let out = apply_orientation(img.clone(), o);
            assert_eq!((out.width, out.height), (2, 3));
        }
    }
}
