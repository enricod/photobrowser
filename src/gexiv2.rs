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

    // The `try_*` names exist in both 0.14 and 0.16, but 0.16 deprecates them in favour of
    // plain names with identical signatures. Under `cfg(gexiv2_016)` (set by build.rs) each
    // `try_*` declaration links to the plain symbol, so call sites stay version-agnostic.
    unsafe extern "C" {
        pub fn g_object_unref(object: *mut c_void);
        pub fn g_free(mem: *mut c_void);
        pub fn g_error_free(error: *mut GError);
        #[cfg(test)]
        pub fn g_quark_from_static_string(string: *const c_char) -> u32;
        #[cfg(test)]
        pub fn g_error_new_literal(domain: u32, code: c_int, message: *const c_char)
        -> *mut GError;

        pub fn gexiv2_initialize() -> GBoolean;
        pub fn gexiv2_log_set_level(level: c_int);
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_register_xmp_namespace")]
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

        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_has_tag")]
        pub fn gexiv2_metadata_try_has_tag(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_tag_string")]
        pub fn gexiv2_metadata_try_get_tag_string(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> *mut c_char;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_set_tag_string")]
        pub fn gexiv2_metadata_try_set_tag_string(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            value: *const c_char,
            error: *mut *mut GError,
        ) -> GBoolean;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_tag_long")]
        pub fn gexiv2_metadata_try_get_tag_long(
            this: *mut GExiv2Metadata,
            tag: *const c_char,
            error: *mut *mut GError,
        ) -> c_long;

        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_exposure_time")]
        pub fn gexiv2_metadata_try_get_exposure_time(
            this: *mut GExiv2Metadata,
            nom: *mut c_int,
            den: *mut c_int,
            error: *mut *mut GError,
        ) -> GBoolean;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_fnumber")]
        pub fn gexiv2_metadata_try_get_fnumber(
            this: *mut GExiv2Metadata,
            error: *mut *mut GError,
        ) -> c_double;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_iso_speed")]
        pub fn gexiv2_metadata_try_get_iso_speed(
            this: *mut GExiv2Metadata,
            error: *mut *mut GError,
        ) -> c_int;
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_focal_length")]
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
        #[cfg_attr(gexiv2_016, link_name = "gexiv2_metadata_get_preview_image")]
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

/// An owned `gchar*` returned by gexiv2, released with `g_free`. May be null.
///
/// Returned from `with_error` closures so the string is freed even when the call also sets an
/// error and `with_error` discards the value.
struct GString(*mut c_char);

impl GString {
    fn as_c_str(&self) -> Option<&CStr> {
        (!self.0.is_null()).then(|| unsafe { CStr::from_ptr(self.0) })
    }
}

impl Drop for GString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sys::g_free(self.0.cast()) }
        }
    }
}

/// An owned `GExiv2PreviewImage*`, released with `g_object_unref`. May be null.
///
/// Like `GString`, this frees the image when a call returns one and also sets an error.
struct PreviewImage(*mut sys::GExiv2PreviewImage);

impl Drop for PreviewImage {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sys::g_object_unref(self.0.cast()) }
        }
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

    pub fn has_tag(&self, tag: &str) -> Result<bool> {
        let tag = cstring(tag)?;
        with_error(|e| unsafe { sys::gexiv2_metadata_try_has_tag(self.raw, tag.as_ptr(), e) })
            .map(|v| v != 0)
    }

    pub fn get_tag_string(&self, tag: &str) -> Result<String> {
        let tag = cstring(tag)?;
        let value = with_error(|e| {
            GString(unsafe { sys::gexiv2_metadata_try_get_tag_string(self.raw, tag.as_ptr(), e) })
        })?;
        let s = value.as_c_str().ok_or(Error::NoValue)?;
        Ok(s.to_string_lossy().into_owned())
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
        let image = with_error(|e| {
            PreviewImage(unsafe {
                sys::gexiv2_metadata_try_get_preview_image(self.meta.raw, self.props, e)
            })
        })?;
        if image.0.is_null() {
            return Err(Error::NoValue);
        }
        let mut size: c_uint = 0;
        let data = unsafe { sys::gexiv2_preview_image_get_data(image.0, &mut size) };
        if data.is_null() {
            return Err(Error::NoValue);
        }
        // `image` owns the buffer and outlives this copy.
        Ok(unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct DropFlag<'a>(&'a Cell<bool>);

    impl Drop for DropFlag<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    fn set_error(slot: *mut *mut sys::GError, message: &CStr) {
        unsafe {
            let domain = sys::g_quark_from_static_string(c"photobrowser-test".as_ptr());
            *slot = sys::g_error_new_literal(domain, 1, message.as_ptr());
        }
    }

    #[test]
    fn with_error_drops_value_when_error_is_set() {
        let dropped = Cell::new(false);
        let result = with_error(|e| {
            set_error(e, c"boom");
            DropFlag(&dropped)
        });
        assert!(matches!(result, Err(Error::Gexiv2(ref m)) if m == "boom"));
        assert!(
            dropped.get(),
            "value returned alongside an error must be dropped"
        );
    }

    #[test]
    fn with_error_returns_value_when_no_error() {
        let dropped = Cell::new(false);
        let result = with_error(|_| DropFlag(&dropped));
        assert!(result.is_ok());
        assert!(!dropped.get());
    }

    #[test]
    fn null_owned_pointers_drop_safely() {
        drop(GString(ptr::null_mut()));
        drop(PreviewImage(ptr::null_mut()));
        assert!(GString(ptr::null_mut()).as_c_str().is_none());
    }
}
