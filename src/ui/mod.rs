pub mod filterbar;
pub mod grid;
pub mod item_object;
pub mod keys;
pub mod state;
pub mod style;
pub mod viewer;
pub mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

pub use item_object::stars_text;

pub const APP_ID: &str = "io.github.enricod.PhotoBrowser";

/// `photobrowser [FOLDER]`
pub fn run() -> glib::ExitCode {
    crate::exif::init();
    let start_folder = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| style::load());
    app.connect_activate(move |app| window::build(app, start_folder.clone()));
    app.run_with_args::<&str>(&[])
}
