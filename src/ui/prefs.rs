use std::rc::Rc;

use adw::prelude::*;

use super::state::AppState;
use crate::config::Config;

pub fn show(state: &Rc<AppState>) {
    let cfg = state.config.borrow().clone();

    let browsing = adw::PreferencesGroup::builder().title("Browsing").build();
    let advance = adw::SwitchRow::builder()
        .title("Advance After Rating")
        .subtitle("In the full view, go to the next image after setting a rating")
        .active(cfg.auto_advance)
        .build();
    browsing.add(&advance);

    let caches = adw::PreferencesGroup::builder()
        .title("Caches")
        .description("Changes take effect after restarting")
        .build();
    let thumbs = adw::SpinRow::with_range(50.0, 5000.0, 50.0);
    thumbs.set_title("Thumbnails Kept in Memory");
    thumbs.set_value(cfg.thumb_cache_items as f64);
    let fulls = adw::SpinRow::with_range(2.0, 30.0, 1.0);
    fulls.set_title("Full-Size Images Kept in Memory");
    fulls.set_value(cfg.full_cache_items as f64);
    let disk = adw::SpinRow::with_range(100.0, 100_000.0, 100.0);
    disk.set_title("Thumbnail Disk Cache (MB)");
    disk.set_value(cfg.disk_cache_mb as f64);
    caches.add(&thumbs);
    caches.add(&fulls);
    caches.add(&disk);

    let page = adw::PreferencesPage::new();
    page.add(&browsing);
    page.add(&caches);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let s = state.clone();
    dialog.connect_closed(move |_| {
        let mut c = s.config.borrow_mut();
        c.auto_advance = advance.is_active();
        c.thumb_cache_items = thumbs.value() as usize;
        c.full_cache_items = fulls.value() as usize;
        c.disk_cache_mb = disk.value() as u64;
        let _ = c.save_to(&Config::default_path());
    });
    dialog.present(Some(&state.widgets().window));
}
