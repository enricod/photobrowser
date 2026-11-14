use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    watch_pkg_config_dirs();
    link_gexiv2();

    // libraw_r is the thread-safe build; each worker owns its own handle.
    let lib = pkg_config::Config::new()
        .atleast_version("0.21")
        .probe("libraw_r")
        .expect("LibRaw not found: install LibRaw-devel (Fedora) or libraw-dev (Debian)");

    println!("cargo:rerun-if-changed=src/libraw/wrapper.h");
    let mut builder = bindgen::Builder::default()
        .header("src/libraw/wrapper.h")
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_type("LibRaw_.*")
        .allowlist_var("LIBRAW_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for path in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }
    builder
        .generate()
        .expect("bindgen failed on libraw.h")
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("libraw_bindings.rs"))
        .expect("could not write bindings");
}

/// Debian ships gexiv2 0.16 as `gexiv2-0.16`; Fedora ships 0.14 as `gexiv2`.
///
/// 0.16 deprecates the `gexiv2_metadata_try_*` entry points in favour of plain-named ones with
/// identical signatures (in 0.14 the plain names take no `GError**`). When 0.16 is found we set
/// `cfg(gexiv2_016)`, and `src/gexiv2.rs` links its `try_*` declarations to the plain names.
fn link_gexiv2() {
    println!("cargo:rustc-check-cfg=cfg(gexiv2_016)");
    let v016 = pkg_config::Config::new()
        .atleast_version("0.16")
        .probe("gexiv2-0.16");
    if v016.is_ok() {
        println!("cargo:rustc-cfg=gexiv2_016");
    } else if let Err(e) = pkg_config::Config::new()
        .atleast_version("0.14")
        .probe("gexiv2")
    {
        panic!(
            "gexiv2 not found (install libgexiv2_0.14-devel on Fedora or libgexiv2-dev on Debian): {e}"
        );
    }
    // g_object_unref, g_free, g_error_free.
    pkg_config::probe_library("gobject-2.0").expect("gobject-2.0 not found");
}

/// Reruns this script when a `.pc` file is added, removed or changed in pkg-config's search path.
///
/// Without this, Cargo keeps the cached probe results after a system upgrade, e.g. a switch from
/// `gexiv2` (0.14) to `gexiv2-0.16`, and links the old library until `cargo clean`. Directories
/// are watched rather than files because the file that matters may not exist yet.
fn watch_pkg_config_dirs() {
    let pkg_config = env::var_os("PKG_CONFIG").unwrap_or_else(|| "pkg-config".into());
    let default_path = match Command::new(&pkg_config)
        .args(["--variable", "pc_path", "pkg-config"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => {
            println!(
                "cargo:warning=could not query pkg-config's search path; \
                 system library changes will need `cargo clean`"
            );
            String::new()
        }
    };
    let extra_path = env::var_os("PKG_CONFIG_PATH").unwrap_or_default();
    let dirs = env::split_paths(&extra_path).chain(env::split_paths(&default_path));
    for dir in dirs {
        // A missing path counts as always changed and would rerun this script on every build.
        if Path::new(&dir).is_dir() {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
    }
}
