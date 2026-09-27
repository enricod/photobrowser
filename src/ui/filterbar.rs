use std::rc::Rc;

use gtk::prelude::*;

use super::state::AppState;
use crate::filter::{RejectFilter, SortKey, SortState, TypeFilter};

pub fn build(state: &Rc<AppState>) -> gtk::Box {
    let rating = gtk::DropDown::from_strings(&["All ratings", "★ 1+", "★ 2+", "★ 3+", "★ 4+", "★ 5"]);
    rating.set_tooltip_text(Some("Minimum rating"));
    let reject = gtk::DropDown::from_strings(&["Show rejects", "Hide rejects", "Only rejects"]);
    reject.set_selected(1);
    let kind = gtk::DropDown::from_strings(&["All types", "RAW", "JPEG", "Other"]);
    let sort = gtk::DropDown::from_strings(&["Date ↑", "Date ↓", "Name ↑", "Name ↓"]);
    sort.set_tooltip_text(Some("Sort order"));

    let s = state.clone();
    rating.connect_selected_notify(move |dd| {
        let min = dd.selected().min(5) as u8;
        s.set_filter(|f| f.min_rating = min);
    });
    let s = state.clone();
    reject.connect_selected_notify(move |dd| {
        let mode = match dd.selected() {
            0 => RejectFilter::ShowAll,
            2 => RejectFilter::OnlyRejects,
            _ => RejectFilter::HideRejects,
        };
        s.set_filter(|f| f.reject = mode);
    });
    let s = state.clone();
    kind.connect_selected_notify(move |dd| {
        let kind = match dd.selected() {
            1 => TypeFilter::Raw,
            2 => TypeFilter::Jpeg,
            3 => TypeFilter::Other,
            _ => TypeFilter::All,
        };
        s.set_filter(|f| f.kind = kind);
    });
    let s = state.clone();
    sort.connect_selected_notify(move |dd| {
        let (key, descending) = match dd.selected() {
            1 => (SortKey::CaptureTime, true),
            2 => (SortKey::FileName, false),
            3 => (SortKey::FileName, true),
            _ => (SortKey::CaptureTime, false),
        };
        s.set_sort(SortState { key, descending });
    });

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for w in [&rating, &reject, &kind, &sort] {
        bar.append(w);
    }
    bar
}
