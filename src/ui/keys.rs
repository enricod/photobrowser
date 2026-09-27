use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::state::AppState;
use crate::catalog::rating::RatingChange;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    match (ctrl, key.to_lower().to_unicode()) {
        (true, Some('o')) => state.choose_folder(),
        (true, Some('f')) => state.focus_filters(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('+' | '=')) => state.zoom_thumbs(1),
        (false, Some('-')) => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
