use gtk::gdk;

pub const THUMB_SIZE_MIN: i32 = 128;
pub const THUMB_SIZE_MAX: i32 = 512;
pub const THUMB_SIZE_STEP: i32 = 32;

const BASE_CSS: &str = "
.pb-cell { padding: 6px; }
.pb-stars { font-size: 0.9em; color: @accent_color; }
.pb-badge { font-size: 0.7em; font-weight: bold; padding: 1px 4px; margin: 4px;
            border-radius: 4px; background: alpha(black, 0.6); color: white; }
.pb-reject { font-weight: bold; padding: 0 5px; margin: 4px; border-radius: 4px;
             background: @error_bg_color; color: @error_fg_color; }
.pb-strip-thumb { min-width: 72px; min-height: 72px; }
.pb-osd { background: alpha(black, 0.6); color: white; padding: 8px 12px;
          border-radius: 8px; margin: 12px; }
.pb-viewer { background: black; }
";

thread_local! {
    static SIZE_PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
}

pub fn load() {
    let display = gdk::Display::default().expect("no display");
    let base = gtk::CssProvider::new();
    base.load_from_string(BASE_CSS);
    gtk::style_context_add_provider_for_display(&display, &base, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    SIZE_PROVIDER.with(|p| {
        gtk::style_context_add_provider_for_display(&display, p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION)
    });
}

/// Sets the on-screen thumbnail size for every grid cell at once.
pub fn set_thumb_size(px: i32) {
    let px = px.clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
    SIZE_PROVIDER.with(|p| {
        p.load_from_string(&format!(".pb-thumb {{ min-width: {px}px; min-height: {px}px; }}"))
    });
}
