//! Minimal safe bindings to gexiv2, replacing the `rexiv2` crate.
//!
//! Only entry points whose C signature is identical in gexiv2 0.14 (Fedora's
//! `gexiv2`) and 0.16 (Debian's `gexiv2-0.16`) are declared: the `try_*` variants,
//! which take a `GError **`. gexiv2 0.16 added that argument to the plain variants
//! (`has_tag`, `get_tag_string`, …), so bindings written for 0.14 — such as
//! `gexiv2-sys` 1.4 — pass a garbage error pointer and crash. Objects are released
//! with `g_object_unref`: 0.16 dropped `gexiv2_*_free`, which were wrappers for it.
//!
//! Go through `crate::exif` for anything that parses, encodes or reads previews:
//! those wrappers hold the exiv2 lock.

use std::ffi::{CStr, CString, OsStr, c_char, c_double, c_int, c_long, c_uint, c_void};
use std::os::unix::ffi::OsStrExt;
use std::ptr;

mod sys {
    use super::*;

    #[repr(C)]
    pub struct GError {
        pub domain: u32,
        pub code: c_int,
        pub message: *mut c_char,
    }

    #[repr(C)]
    pub struct GExiv2Metadata {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct GExiv2PreviewProperties {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct GExiv2PreviewImage {
        _private: [u8; 0],
    }

    pub type GBoolean = c_int;
    pub const GEXIV2_LOG_LEVEL_MUTE: c_int = 4;

    unsafe extern "C" {
        pub fn g_object_unref(object: *mut c_void);
        pub fn g_free(mem: *mut c_void);
        pub fn g_error_free(error: *mut GError);

        pub fn gexiv2_initialize() -> GBoolean;
        pub fn gexiv2_log_set_level(level: c_int);
        pub fn gexiv2_metadata_try_register_xmp_namespace(
            name: *const c_char,
            prefix: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;

        pub fn gexiv2_metadata_new() -> *mut GExiv2Metadata;
        pub fn gexiv2_metadata_open_path(
            this: *mut GExiv2Metadata,
            path: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;
        pub fn gexiv2_metadata_open_buf(
            this: *mut GExiv2Metadata,
            data: *const u8,
            n_data: c_long,
            error: *mut *mut GError,
        ) -> GBoolean;
        pub fn gexiv2_metadata_save_file(
            this: *mut GExiv2Metadata,
            path: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;

        pub fn gexiv2_metadata_try_has_tag(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;
        pub fn gexiv2_metadata_try_get_tag_string(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> *mut c_char;
        pub fn gexiv2_metadata_try_set_tag_string(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            value: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;
        pub fn gexiv2_metadata_try_get_tag_long(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> c_long;

        pub fn gexiv2_metadata_try_get_exposure_time(
            this: *mut GExiv2Metadata,
            nom: *mut c_int,
            den: *mut c_int,
            error: *mut *mut GError,
        ) -> GBoolean;
        pub fn gexiv2_metadata_try_get_fnumber(
            this: *mut GExiv2Metadata,
            error: *mut *mut GError,
        ) -> c_double;
        pub fn gexiv2_metadata_try_get_iso_speed(
            this: *mut GExiv2Metadata,
            error: *mut *mut GError,
        ) -> c_int;
        pub fn gexiv2_metadata_try_get_focal_length(
            this: *mut GExiv2Metadata,
            error: *mut *mut GError,
        ) -> c_double;

        pub fn gexiv2_metadata_get_preview_properties(
            this: *mut GExiv2Metadata,
        ) -> *mut *mut GExiv2PreviewProperties;
        pub fn gexiv2_preview_properties_get_width(this: *mut GExiv2PreviewProperties) -> u32;
        pub fn gexiv2_preview_properties_get_height(this: *mut GExiv2PreviewProperties) -> u32;
        pub fn gexiv2_preview_properties_get_extension(
            this: *mut GExiv2PreviewProperties,
        ) -> *const c_char;
        pub fn gexiv2_metadata_try_get_preview_image(
            this: *mut GExiv2Metadata,
            props: *mut GExiv2PreviewProperties,
            error: *mut *mut GError,
        ) -> *mut GExiv2PreviewImage;
        pub fn gexiv2_preview_image_get_data(
            this: *mut GExiv2PreviewImage,
            size: *mut c_uint,
        ) -> *const u8;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Gexiv2(String),
    #[error("no value")]
    NoValue,
    #[error("string contains a NUL byte")]
    Nul,
}

pub type Result<T> = std::result::Result<T, Error>;

fn cstring(s: impl AsRef<[u8]>) -> Result<CString> {
    CString::new(s.as_ref()).map_err(|_| Error::Nul)
}

/// Takes ownership of `err` (if set) and turns it into an `Error`.
unsafe fn take_error(err: *mut sys::GError) -> Option<Error> {
    if err.is_null() {
        return None;
    }
    let msg = unsafe {
        let m = (*err).message;
        let text = if m.is_null() {
            "unknown gexiv2 error".to_owned()
        } else {
            CStr::from_ptr(m).to_string_lossy().into_owned()
        };
        sys::g_error_free(err);
        text
    };
    Some(Error::Gexiv2(msg))
}

/// Calls `f` with a fresh `GError` slot; returns its value, or the error if one was set.
fn with_error<T>(f: impl FnOnce(*mut *mut sys::GError) -> T) -> Result<T> {
    let mut err: *mut sys::GError = ptr::null_mut();
    let value = f(&mut err);
    match unsafe { take_error(err) } {
        Some(e) => Err(e),
        None => Ok(value),
    }
}

fn check(ok: sys::GBoolean, err: Result<()>) -> Result<()> {
    err?;
    if ok == 0 {
        return Err(Error::Gexiv2("gexiv2 call failed".into()));
    }
    Ok(())
}

pub fn initialize() -> Result<()> {
    if unsafe { sys::gexiv2_initialize() } == 0 {
        return Err(Error::Gexiv2("initialisation failed".into()));
    }
    Ok(())
}

pub fn mute_log() {
    unsafe { sys::gexiv2_log_set_level(sys::GEXIV2_LOG_LEVEL_MUTE) }
}

pub fn register_xmp_namespace(name: &str, prefix: &str) -> Result<()> {
    let (name, prefix) = (cstring(name)?, cstring(prefix)?);
    let mut ok = 0;
    let err = with_error(|e| {
        ok = unsafe {
            sys::gexiv2_metadata_try_register_xmp_namespace(name.as_ptr(), prefix.as_ptr(), e)
        };
    });
    check(ok, err)
}

/// A file's or buffer's metadata. Not `Send`: used on the thread that opened it.
pub struct Metadata {
    raw: *mut sys::GExiv2Metadata,
}

impl Drop for Metadata {
    fn drop(&mut self) {
        unsafe { sys::g_object_unref(self.raw.cast()) }
    }
}

impl Metadata {
    fn new() -> Result<Metadata> {
        let raw = unsafe { sys::gexiv2_metadata_new() };
        if raw.is_null() {
            return Err(Error::Gexiv2("could not allocate metadata".into()));
        }
        Ok(Metadata { raw })
    }

    /// Use `crate::exif::open_path` instead: it holds the exiv2 lock.
    pub(crate) fn new_from_path(path: &OsStr) -> Result<Metadata> {
        let c_path = cstring(path.as_bytes())?;
        let meta = Metadata::new()?;
        let mut ok = 0;
        let err = with_error(|e| {
            ok = unsafe { sys::gexiv2_metadata_open_path(meta.raw, c_path.as_ptr(), e) };
        });
        check(ok, err)?;
        Ok(meta)
    }

    /// Use `crate::exif::open_buffer` instead: it holds the exiv2 lock.
    pub(crate) fn new_from_buffer(data: &[u8]) -> Result<Metadata> {
        let len =
            c_long::try_from(data.len()).map_err(|_| Error::Gexiv2("buffer too large".into()))?;
        let meta = Metadata::new()?;
        let mut ok = 0;
        let err = with_error(|e| {
            ok = unsafe { sys::gexiv2_metadata_open_buf(meta.raw, data.as_ptr(), len, e) };
        });
        check(ok, err)?;
        Ok(meta)
    }

    /// Use `crate::exif::save` instead: it holds the exiv2 lock.
    pub(crate) fn save_to_file(&self, path: &OsStr) -> Result<()> {
        let c_path = cstring(path.as_bytes())?;
        let mut ok = 0;
        let err = with_error(|e| {
            ok = unsafe { sys::gexiv2_metadata_save_file(self.raw, c_path.as_ptr(), e) };
        });
        check(ok, err)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        let Ok(tag) = cstring(tag) else { return false };
        with_error(|e| unsafe { sys::gexiv2_metadata_try_has_tag(self.raw, tag.as_ptr(), e) })
            .is_ok_and(|v| v != 0)
    }

    pub fn get_tag_string(&self, tag: &str) -> Result<String> {
        let tag = cstring(tag)?;
        let value = with_error(|e| unsafe {
            sys::gexiv2_metadata_try_get_tag_string(self.raw, tag.as_ptr(), e)
        })?;
        if value.is_null() {
            return Err(Error::NoValue);
        }
        let s = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        unsafe { sys::g_free(value.cast()) };
        Ok(s)
    }

    pub fn set_tag_string(&self, tag: &str, value: &str) -> Result<()> {
        let (tag, value) = (cstring(tag)?, cstring(value)?);
        let mut ok = 0;
        let err = with_error(|e| {
            ok = unsafe {
                sys::gexiv2_metadata_try_set_tag_string(self.raw, tag.as_ptr(), value.as_ptr(), e)
            };
        });
        check(ok, err)
    }

    /// The tag's integer value; 0 when missing or not numeric.
    pub fn get_tag_numeric(&self, tag: &str) -> i32 {
        let Ok(tag) = cstring(tag) else { return 0 };
        with_error(|e| unsafe { sys::gexiv2_metadata_try_get_tag_long(self.raw, tag.as_ptr(), e) })
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0)
    }

    /// Exposure time as (numerator, denominator).
    pub fn get_exposure_time(&self) -> Option<(i32, i32)> {
        let (mut num, mut den) = (0, 0);
        let ok = with_error(|e| unsafe {
            sys::gexiv2_metadata_try_get_exposure_time(self.raw, &mut num, &mut den, e)
        })
        .ok()?;
        (ok != 0).then_some((num, den))
    }

    pub fn get_fnumber(&self) -> Option<f64> {
        with_error(|e| unsafe { sys::gexiv2_metadata_try_get_fnumber(self.raw, e) })
            .ok()
            .filter(|v| *v >= 0.0)
    }

    pub fn get_iso_speed(&self) -> Option<i32> {
        with_error(|e| unsafe { sys::gexiv2_metadata_try_get_iso_speed(self.raw, e) })
            .ok()
            .filter(|v| *v > 0)
    }

    pub fn get_focal_length(&self) -> Option<f64> {
        with_error(|e| unsafe { sys::gexiv2_metadata_try_get_focal_length(self.raw, e) })
            .ok()
            .filter(|v| *v >= 0.0)
    }

    /// Embedded previews. Use `crate::exif::largest_jpeg_preview` instead: it holds the
    /// exiv2 lock.
    pub(crate) fn get_preview_images(&self) -> Vec<Preview<'_>> {
        let list = unsafe { sys::gexiv2_metadata_get_preview_properties(self.raw) };
        let mut out = Vec::new();
        if list.is_null() {
            return out;
        }
        // The NULL-terminated array and its entries are owned by the metadata object.
        let mut i = 0;
        loop {
            let props = unsafe { *list.add(i) };
            if props.is_null() {
                break;
            }
            out.push(Preview { meta: self, props });
            i += 1;
        }
        out
    }
}

/// One embedded preview; borrows the metadata that owns it.
pub struct Preview<'a> {
    meta: &'a Metadata,
    props: *mut sys::GExiv2PreviewProperties,
}

impl Preview<'_> {
    pub fn width(&self) -> u32 {
        unsafe { sys::gexiv2_preview_properties_get_width(self.props) }
    }

    pub fn height(&self) -> u32 {
        unsafe { sys::gexiv2_preview_properties_get_height(self.props) }
    }

    /// File extension including the dot, e.g. ".jpg".
    pub fn extension(&self) -> Option<String> {
        let ext = unsafe { sys::gexiv2_preview_properties_get_extension(self.props) };
        if ext.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(ext) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    pub fn data(&self) -> Result<Vec<u8>> {
        let image = with_error(|e| unsafe {
            sys::gexiv2_metadata_try_get_preview_image(self.meta.raw, self.props, e)
        })?;
        if image.is_null() {
            return Err(Error::NoValue);
        }
        let mut size: c_uint = 0;
        let result = unsafe {
            let data = sys::gexiv2_preview_image_get_data(image, &mut size);
            if data.is_null() {
                Err(Error::NoValue)
            } else {
                Ok(std::slice::from_raw_parts(data, size as usize).to_vec())
            }
        };
        unsafe { sys::g_object_unref(image.cast()) };
        result
    }
}
