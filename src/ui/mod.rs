use adw::prelude::*;

pub const APP_ID: &str = "io.github.enricod.PhotoBrowser";

pub fn run() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Photobrowser")
            .default_width(1400)
            .default_height(900)
            .build();
        window.present();
    });
    app.run_with_args::<&str>(&[])
}
