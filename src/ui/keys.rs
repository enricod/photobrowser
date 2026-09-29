use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::dialogs;
use super::state::AppState;
use crate::catalog::rating::RatingChange;
use crate::fileops::OpKind;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
    let ch = key.to_lower().to_unicode();

    if state.viewer_active() && !ctrl {
        match key {
            gdk::Key::Escape => state.close_viewer(),
            gdk::Key::Right | gdk::Key::Down | gdk::Key::space => state.step(1),
            gdk::Key::Left | gdk::Key::Up | gdk::Key::BackSpace => state.step(-1),
            _ => match ch {
                Some('z') => state.toggle_zoom(),
                Some('i') => state.toggle_info(),
                Some('f') => state.widgets().viewer.toggle_strip(),
                _ => return handle_common(state, ctrl, shift, ch),
            },
        }
        return glib::Propagation::Stop;
    }
    handle_common(state, ctrl, shift, ch)
}

fn handle_common(
    state: &Rc<AppState>,
    ctrl: bool,
    shift: bool,
    ch: Option<char>,
) -> glib::Propagation {
    match (ctrl, ch) {
        (true, Some('o')) => state.choose_folder(),
        (true, Some('f')) => state.focus_filters(),
        (true, Some('m')) if shift => state.move_to_last(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('m')) => dialogs::choose_destination(state, OpKind::Move),
        (false, Some('c')) => dialogs::choose_destination(state, OpKind::Copy),
        (false, Some('+' | '=')) if !state.viewer_active() => state.zoom_thumbs(1),
        (false, Some('-')) if !state.viewer_active() => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
