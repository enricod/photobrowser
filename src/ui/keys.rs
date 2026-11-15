use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::dialogs;
use super::state::AppState;
use crate::catalog::rating::RatingChange;
use crate::fileops::OpKind;

/// A group in the Keyboard Shortcuts dialog: (description, accelerators) pairs, in
/// `gtk_accelerator_parse` syntax (alternatives separated by spaces, ranges with "...").
/// Keep in sync with `handle` below.
pub struct ShortcutSection {
    pub title: &'static str,
    pub items: &'static [(&'static str, &'static str)],
}

pub const SHORTCUTS: &[ShortcutSection] = &[
    ShortcutSection {
        title: "General",
        items: &[
            ("Open folder", "<Control>o"),
            ("Open the parent folder", "<Alt>Up"),
            ("Focus the filter bar", "<Control>f"),
            ("Select all visible photos", "<Control>a"),
            ("Keyboard shortcuts", "<Control>question"),
        ],
    },
    ShortcutSection {
        title: "Rating",
        items: &[
            ("Set rating", "1...5"),
            ("Clear rating", "0"),
            ("Toggle reject", "x"),
        ],
    },
    ShortcutSection {
        title: "Files",
        items: &[
            ("Move to folder…", "m"),
            ("Copy to folder…", "c"),
            ("Move to the last destination", "<Control><Shift>m"),
            ("Open in the favourite program", "e"),
            ("Open with…", "<Shift>e"),
        ],
    },
    ShortcutSection {
        title: "Grid",
        items: &[
            ("Open the full view (or double-click)", "Return"),
            ("Thumbnail size (or Ctrl+scroll)", "plus minus"),
        ],
    },
    ShortcutSection {
        title: "Full View",
        items: &[
            ("Next photo", "Right Down space"),
            ("Previous photo", "Left Up BackSpace"),
            ("Fit ↔ 100 % (or double-click)", "z"),
            ("Info overlay", "i"),
            ("Filmstrip", "f"),
            ("Back to the grid", "Escape"),
        ],
    },
];

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    // Typing in a text field (the folder filter, …) must not trigger shortcuts: "2024-05"
    // would otherwise set ratings.
    if GtkWindowExt::focus(&state.widgets().window).is_some_and(|w| w.is::<gtk::Editable>()) {
        return glib::Propagation::Proceed;
    }
    if key == gdk::Key::Up && mods.contains(gdk::ModifierType::ALT_MASK) {
        state.open_parent();
        return glib::Propagation::Stop;
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
        (true, Some('a')) if !shift && !state.viewer_active() => state.select_all(),
        (true, Some('?')) => dialogs::shortcuts(state),
        (true, Some('m')) if shift => state.move_to_last(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('m')) => dialogs::choose_destination(state, OpKind::Move),
        (false, Some('c')) => dialogs::choose_destination(state, OpKind::Copy),
        (false, Some('e')) if shift => super::openwith::open_with(state),
        (false, Some('e')) => super::openwith::open_in_favourite(state),
        (false, Some('+' | '=')) if !state.viewer_active() => state.zoom_thumbs(1),
        (false, Some('-')) if !state.viewer_active() => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shortcut_table_is_complete_and_unambiguous() {
        let mut seen = HashSet::new();
        for section in SHORTCUTS {
            assert!(!section.title.is_empty());
            assert!(!section.items.is_empty(), "{}", section.title);
            for (title, accels) in section.items {
                assert!(!title.is_empty());
                for accel in accels.split(' ') {
                    assert!(!accel.is_empty(), "{title}: {accels:?}");
                    // The same key may mean different things in the grid and the full view
                    // (e.g. F), but never twice in one context.
                    assert!(
                        seen.insert((section.title == "Full View", accel)),
                        "{accel} listed twice"
                    );
                }
            }
        }
    }
}
