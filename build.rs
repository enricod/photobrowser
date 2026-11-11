use std::env;
use std::path::PathBuf;

fn main() {
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

/// Debian ships gexiv2 0.16 as `gexiv2-0.16`; Fedora ships 0.14 as `gexiv2`. `src/gexiv2.rs`
/// only uses entry points whose signatures match in both.
fn link_gexiv2() {
    let found = pkg_config::Config::new()
        .atleast_version("0.16")
        .probe("gexiv2-0.16")
        .or_else(|_| {
            pkg_config::Config::new()
                .atleast_version("0.14")
                .probe("gexiv2")
        });
    if let Err(e) = found {
        panic!(
            "gexiv2 not found (install libgexiv2_0.14-devel on Fedora or libgexiv2-dev on Debian): {e}"
        );
    }
    // g_object_unref, g_free, g_error_free.
    pkg_config::probe_library("gobject-2.0").expect("gobject-2.0 not found");
}
