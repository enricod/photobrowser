use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::state::AppState;
use crate::fileops::OpKind;

/// "Move/Copy to…": recent destinations plus a folder chooser.
pub fn choose_destination(state: &Rc<AppState>, kind: OpKind) {
    let targets = state.rating_targets();
    if targets.is_empty() {
        state.toast("Select one or more images first");
        return;
    }
    let title = match kind {
        OpKind::Move => "Move to Folder",
        OpKind::Copy => "Copy to Folder",
    };
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(460)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let recent: Vec<_> = state
        .index
        .borrow()
        .recent_destinations()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.is_dir() && Some(d) != state.folder().as_ref())
        .collect();
    if !recent.is_empty() {
        let heading = gtk::Label::builder()
            .label("Recent folders")
            .xalign(0.0)
            .build();
        heading.add_css_class("heading");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for dir in recent {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(glib::markup_escape_text(&dir.to_string_lossy()).as_str())
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (s, dlg, t) = (state.clone(), dialog.downgrade(), targets.clone());
            row.connect_activated(move |_| {
                if let Some(dlg) = dlg.upgrade() {
                    dlg.close();
                }
                s.start_transfer(t.clone(), dir.clone(), kind);
            });
            list.append(&row);
        }
        content.append(&heading);
        content.append(&list);
    }

    let choose = gtk::Button::with_label("Choose Folder…");
    choose.add_css_class("pill");
    choose.set_halign(gtk::Align::Center);
    // Weak: the dialog owns these widgets, so a strong capture would keep it alive forever.
    let (s, dlg) = (state.clone(), dialog.downgrade());
    choose.connect_clicked(move |_| {
        if let Some(dlg) = dlg.upgrade() {
            dlg.close();
        }
        let chooser = gtk::FileDialog::builder().title(title).modal(true).build();
        let s2 = s.clone();
        let t = targets.clone();
        chooser.select_folder(
            Some(&s.widgets().window),
            gio::Cancellable::NONE,
            move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    s2.start_transfer(t, path, kind);
                }
            },
        );
    });
    content.append(&choose);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&state.widgets().window));
}
