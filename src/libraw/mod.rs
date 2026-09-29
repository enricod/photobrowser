use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::decode::Rgba;

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/libraw_bindings.rs"));
}

#[derive(Debug, thiserror::Error)]
pub enum LibRawError {
    #[error("LibRaw could not be initialised")]
    Init,
    #[error("path contains a NUL byte")]
    BadPath,
    #[error("{0}")]
    Code(String),
    #[error("unexpected LibRaw output format")]
    Format,
}

fn check(code: c_int) -> Result<(), LibRawError> {
    if code == 0 {
        return Ok(());
    }
    let msg = unsafe { CStr::from_ptr(sys::libraw_strerror(code)) };
    Err(LibRawError::Code(msg.to_string_lossy().into_owned()))
}

/// LibRaw `sizes.flip` → EXIF orientation.
pub fn flip_to_orientation(flip: i32) -> u8 {
    match flip {
        3 => 3,
        5 => 8,
        6 => 6,
        _ => 1,
    }
}

/// An opened RAW file. Not shared between threads; moved into a worker is fine.
pub struct Raw {
    ptr: *mut sys::libraw_data_t,
}

// SAFETY: the handle is owned exclusively; libraw_r has no global mutable state.
unsafe impl Send for Raw {}

struct MemImage(*mut sys::libraw_processed_image_t);

impl Drop for MemImage {
    fn drop(&mut self) {
        unsafe { sys::libraw_dcraw_clear_mem(self.0) }
    }
}

impl Raw {
    pub fn open(path: &Path) -> Result<Raw, LibRawError> {
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| LibRawError::BadPath)?;
        let ptr = unsafe { sys::libraw_init(0) };
        if ptr.is_null() {
            return Err(LibRawError::Init);
        }
        let raw = Raw { ptr };
        check(unsafe { sys::libraw_open_file(raw.ptr, c_path.as_ptr()) })?;
        Ok(raw)
    }

    pub fn flip(&self) -> i32 {
        unsafe { (*self.ptr).sizes.flip }
    }

    /// The camera's embedded JPEG thumbnail, if it is a JPEG. Not rotated.
    pub fn thumbnail_jpeg(&mut self) -> Result<Option<Vec<u8>>, LibRawError> {
        unsafe {
            check(sys::libraw_unpack_thumb(self.ptr))?;
            let mut err: c_int = 0;
            let img = sys::libraw_dcraw_make_mem_thumb(self.ptr, &mut err);
            if img.is_null() {
                return Err(check(err).err().unwrap_or(LibRawError::Format));
            }
            let img = MemImage(img);
            // Read scalar header fields by value through the raw pointer, never
            // through a `&libraw_processed_image_t` reference: `data` below is
            // bindgen's `[u8; 1]` stand-in for the C flexible array member
            // `data[1]`, and a reference only has provenance over the struct's
            // nominal (1-byte-data) size.
            let type_ = std::ptr::addr_of!((*img.0).type_).read();
            let data_size = std::ptr::addr_of!((*img.0).data_size).read();
            if type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_JPEG {
                return Ok(None);
            }
            // SAFETY: `data_ptr` is derived from the raw allocation pointer `img.0`
            // (via `addr_of!`, not through a reference), so it keeps provenance over
            // the whole allocation made by libraw_dcraw_make_mem_thumb, which is
            // `data_size` bytes long starting at the `data` field.
            let data_ptr = std::ptr::addr_of!((*img.0).data) as *const u8;
            let data = std::slice::from_raw_parts(data_ptr, data_size as usize);
            Ok(Some(data.to_vec()))
        }
    }

    /// Demosaics the sensor data to 8-bit sRGB. Output is already upright.
    pub fn decode(self, half_size: bool) -> Result<Rgba, LibRawError> {
        unsafe {
            let params = &mut (*self.ptr).params;
            params.half_size = half_size as _;
            params.use_camera_wb = 1;
            params.output_bps = 8;
            check(sys::libraw_unpack(self.ptr))?;
            check(sys::libraw_dcraw_process(self.ptr))?;
            let mut err: c_int = 0;
            let img = sys::libraw_dcraw_make_mem_image(self.ptr, &mut err);
            if img.is_null() {
                return Err(check(err).err().unwrap_or(LibRawError::Format));
            }
            let img = MemImage(img);
            // Read scalar header fields by value through the raw pointer, never
            // through a `&libraw_processed_image_t` reference: `data` below is
            // bindgen's `[u8; 1]` stand-in for the C flexible array member
            // `data[1]`, and a reference only has provenance over the struct's
            // nominal (1-byte-data) size.
            let type_ = std::ptr::addr_of!((*img.0).type_).read();
            let colors = std::ptr::addr_of!((*img.0).colors).read();
            let bits = std::ptr::addr_of!((*img.0).bits).read();
            if type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP || colors != 3 || bits != 8 {
                return Err(LibRawError::Format);
            }
            let width = std::ptr::addr_of!((*img.0).width).read();
            let height = std::ptr::addr_of!((*img.0).height).read();
            let data_size = std::ptr::addr_of!((*img.0).data_size).read();
            let (w, h) = (width as u32, height as u32);
            // SAFETY: `data_ptr` is derived from the raw allocation pointer `img.0`
            // (via `addr_of!`, not through a reference), so it keeps provenance over
            // the whole allocation made by libraw_dcraw_make_mem_image, which is
            // `data_size` bytes long starting at the `data` field.
            let data_ptr = std::ptr::addr_of!((*img.0).data) as *const u8;
            let rgb = std::slice::from_raw_parts(data_ptr, data_size as usize);
            let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
            for px in rgb.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Ok(Rgba::new(w, h, rgba))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe { sys::libraw_close(self.ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_flip_to_exif_orientation() {
        assert_eq!(flip_to_orientation(0), 1);
        assert_eq!(flip_to_orientation(3), 3);
        assert_eq!(flip_to_orientation(5), 8);
        assert_eq!(flip_to_orientation(6), 6);
    }

    #[test]
    fn missing_file_is_error() {
        assert!(Raw::open(Path::new("/definitely/missing.cr3")).is_err());
    }

    #[test]
    fn non_raw_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        crate::testutil::write_jpeg(&p, 16, 16, [0, 0, 0]);
        assert!(Raw::open(&p).and_then(|r| r.decode(true)).is_err());
    }
}
