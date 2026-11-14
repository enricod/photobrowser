use photobrowser::decode::helper;

fn main() -> gtk::glib::ExitCode {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new(helper::HELPER_FLAG)) {
        // LibRaw decode for the app, in its own process (no GTK): see `decode::helper`.
        std::process::exit(helper::helper_main(args.collect()));
    }
    photobrowser::ui::run()
}
