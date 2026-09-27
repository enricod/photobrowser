use std::env;
use std::path::PathBuf;

fn main() {
    // libraw_r is the thread-safe build; each worker owns its own handle.
    let lib = pkg_config::Config::new()
        .atleast_version("0.21")
        .probe("libraw_r")
        .expect("LibRaw not found: sudo dnf install LibRaw-devel");

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
