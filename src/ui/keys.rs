use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::state::AppState;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    match (ctrl, key.to_lower().to_unicode()) {
        (true, Some('o')) => state.choose_folder(),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
