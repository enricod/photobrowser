use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::state::AppState;
use crate::fileops::OpKind;
use crate::organize::{self, OrganizePattern};

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

/// "Organize…": sort the selected photos into dated folders under a chosen root.
pub fn organize(state: &Rc<AppState>) {
    let targets = state.rating_targets();
    if targets.is_empty() {
        state.toast("Select one or more images first");
        return;
    }
    let pattern = OrganizePattern::default();
    let dates = state.shot_dates(&targets);
    let root = std::rc::Rc::new(std::cell::RefCell::new(
        state.folder().unwrap_or_else(glib::home_dir),
    ));

    let dialog = adw::Dialog::builder()
        .title("Organize Photos")
        .content_width(480)
        .build();

    let group = adw::PreferencesGroup::new();
    let sort_by = adw::ComboRow::builder()
        .title("Sort by")
        .model(&gtk::StringList::new(&["Shot date"]))
        .build();
    group.add(&sort_by);
    let layout = adw::ActionRow::builder()
        .title("Folder layout")
        .subtitle(pattern.label())
        .build();
    group.add(&layout);
    let destination = adw::ActionRow::builder()
        .title("Destination")
        .subtitle(glib::markup_escape_text(&root.borrow().to_string_lossy()).as_str())
        .build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    destination.add_suffix(&choose);
    group.add(&destination);
    let action = adw::ComboRow::builder()
        .title("Action")
        .model(&gtk::StringList::new(&["Move", "Copy"]))
        .build();
    group.add(&action);

    let plan = organize::plan(&dates, &root.borrow(), pattern);
    let items = targets.len();
    let dated = items - plan.undated;
    let mut summary_text = format!(
        "{dated} item{} → {} folder{}",
        if dated == 1 { "" } else { "s" },
        plan.folders,
        if plan.folders == 1 { "" } else { "s" }
    );
    if plan.undated > 0 {
        summary_text.push_str(&format!(
            " · {} without a shot date will be skipped",
            plan.undated
        ));
    }
    if dated == 0 {
        summary_text = "None of the selected photos has a shot date".into();
    }
    let summary = gtk::Label::builder()
        .label(&summary_text)
        .wrap(true)
        .xalign(0.0)
        .build();
    summary.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&group);
    content.append(&summary);

    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::with_label("Organize");
    run.add_css_class("suggested-action");
    run.set_sensitive(dated > 0);
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&run);

    // Weak: the dialog owns these widgets, so a strong capture would keep it alive forever.
    let dlg = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dlg) = dlg.upgrade() {
            dlg.close();
        }
    });

    let (s, r, row) = (state.clone(), root.clone(), destination.downgrade());
    choose.connect_clicked(move |_| {
        let chooser = gtk::FileDialog::builder()
            .title("Organize Into")
            .modal(true)
            .initial_folder(&gio::File::for_path(&*r.borrow()))
            .build();
        let (r, row) = (r.clone(), row.clone());
        chooser.select_folder(
            Some(&s.widgets().window),
            gio::Cancellable::NONE,
            move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    if let Some(row) = row.upgrade() {
                        row.set_subtitle(
                            glib::markup_escape_text(&path.to_string_lossy()).as_str(),
                        );
                    }
                    *r.borrow_mut() = path;
                }
            },
        );
    });

    let (s, dlg, action) = (state.clone(), dialog.downgrade(), action.downgrade());
    run.connect_clicked(move |_| {
        let kind = match action.upgrade().map(|a| a.selected()) {
            Some(1) => OpKind::Copy,
            _ => OpKind::Move,
        };
        if let Some(dlg) = dlg.upgrade() {
            dlg.close();
        }
        s.start_organize(targets.clone(), root.borrow().clone(), kind);
    });

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&state.widgets().window));
}
