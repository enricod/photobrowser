//! Fixture helpers for unit tests. Images are generated, never checked in.
use std::path::Path;

pub fn write_jpeg(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
}

/// Left half red, right half green.
pub fn write_two_tone_jpeg(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 255, 0]) }
    })
    .save_with_format(path, image::ImageFormat::Jpeg)
    .unwrap();
}

pub fn write_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    image::RgbaImage::from_pixel(w, h, image::Rgba(rgba))
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}

pub fn write_tiff(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .save_with_format(path, image::ImageFormat::Tiff)
        .unwrap();
}

/// Writes one EXIF tag into an existing image file.
pub fn set_exif(path: &Path, tag: &str, value: &str) {
    let meta = crate::exif::open_path(path).unwrap();
    meta.set_tag_string(tag, value).unwrap();
    crate::exif::save(&meta, path).unwrap();
}
