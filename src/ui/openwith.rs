//! "Open With…" and "Open in ‹favourite›": hand the targeted photo to another program.
//!
//! Natively, Photobrowser lists the programs for the file's type itself and remembers a
//! favourite (its desktop ID, in the config). Inside a Flatpak sandbox other programs are
//! invisible, so both actions go through the desktop portal instead: "Open With…" shows the
//! system's chooser, and the favourite is the system's default app for the file type.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::state::AppState;
use crate::media::Item;

/// True when running inside a Flatpak sandbox.
pub fn in_sandbox() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// The file an item is opened as: its RAW if it has one (that is what editors want),
/// otherwise its JPEG/other file.
pub fn file_to_open(item: &Item) -> PathBuf {
    item.raw_file()
        .unwrap_or_else(|| item.display_file())
        .path
        .clone()
}

/// The remembered favourite program, if it is (still) installed. Native builds only.
pub fn favourite(state: &AppState) -> Option<gio::AppInfo> {
    let id = state.config.borrow().open_with_app.clone()?;
    gio::AppInfo::all()
        .into_iter()
        .find(|app| app.id().is_some_and(|app_id| app_id == id.as_str()))
}

/// The menu label for the "open in favourite" action.
pub fn favourite_label(state: &AppState) -> String {
    if in_sandbox() {
        return "Open in Default App".into();
    }
    match favourite(state) {
        Some(app) => format!("Open in {}", app.display_name()),
        None => "Open in Favourite App…".into(),
    }
}

/// The single photo the actions apply to, or `None` (after a toast) for zero or several.
fn single_target(state: &Rc<AppState>) -> Option<PathBuf> {
    let targets = state.rating_targets();
    match targets.as_slice() {
        [one] => Some(one.with_item(file_to_open)),
        [] => {
            state.toast("Select a photo first");
            None
        }
        _ => {
            state.toast("Select a single photo to open it in another program");
            None
        }
    }
}

/// `E`: opens the photo in the favourite program, asking first if there is none yet.
pub fn open_in_favourite(state: &Rc<AppState>) {
    let Some(path) = single_target(state) else {
        return;
    };
    if in_sandbox() {
        launch_via_portal(state, &path, false);
        return;
    }
    match favourite(state) {
        Some(app) => launch(state, &app, &path),
        None => choose(state, path),
    }
}

/// `Shift+E`: always asks which program to use.
pub fn open_with(state: &Rc<AppState>) {
    let Some(path) = single_target(state) else {
        return;
    };
    if in_sandbox() {
        launch_via_portal(state, &path, true);
    } else {
        choose(state, path);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn launch(state: &Rc<AppState>, app: &gio::AppInfo, path: &Path) {
    let context = WidgetExt::display(&state.widgets().window).app_launch_context();
    match app.launch(&[gio::File::for_path(path)], Some(&context)) {
        Ok(()) => state.toast(&format!(
            "Opening {} in {}…",
            file_name(path),
            app.display_name()
        )),
        Err(e) => state.toast(&format!("Couldn't open {}: {e}", app.display_name())),
    }
}

/// Opens through the desktop portal: the system's chooser (`ask`) or its default app.
fn launch_via_portal(state: &Rc<AppState>, path: &Path, ask: bool) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    launcher.set_always_ask(ask);
    let s = state.clone();
    let name = file_name(path);
    launcher.launch(
        Some(&state.widgets().window),
        gio::Cancellable::NONE,
        move |result| match result {
            Ok(()) => {
                if !ask {
                    s.toast(&format!("Opening {name}…"));
                }
            }
            // Cancelling the chooser is not an error worth reporting.
            Err(e) if e.matches(gtk::DialogError::Dismissed) => {}
            Err(e) => s.toast(&format!("Couldn't open {name}: {e}")),
        },
    );
}

/// Programs that can open `path`, by name; all visible programs if none claims its type.
fn programs_for(path: &Path) -> Vec<gio::AppInfo> {
    let (content_type, _) = gio::content_type_guess(Some(path), None);
    let mut apps = gio::AppInfo::all_for_type(&content_type);
    if apps.is_empty() {
        apps = gio::AppInfo::all()
            .into_iter()
            .filter(|a| a.should_show())
            .collect();
    }
    apps.sort_by_key(|a| a.display_name().to_lowercase());
    apps.dedup_by(|a, b| a.id() == b.id());
    apps
}

/// The native chooser: programs for the file's type, with "Remember as favourite".
fn choose(state: &Rc<AppState>, path: PathBuf) {
    let apps = programs_for(&path);
    if apps.is_empty() {
        state.toast("No program found to open this file");
        return;
    }
    let dialog = adw::Dialog::builder()
        .title(format!("Open {}", file_name(&path)))
        .content_width(420)
        .content_height(480)
        .build();

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let remember = gtk::CheckButton::builder()
        .label("Remember as favourite (E)")
        .active(true)
        .build();
    let current = state.config.borrow().open_with_app.clone();
    for app in &apps {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&app.display_name()).as_str())
            .activatable(true)
            .build();
        if let Some(icon) = app.icon() {
            let image = gtk::Image::from_gicon(&icon);
            image.set_icon_size(gtk::IconSize::Large);
            row.add_prefix(&image);
        }
        if app.id().is_some() && app.id() == current.as_deref().map(glib::GString::from) {
            row.set_subtitle("Favourite");
        }
        let (s, app, path) = (state.clone(), app.clone(), path.clone());
        let (dlg, remember) = (dialog.downgrade(), remember.downgrade());
        row.connect_activated(move |_| {
            if remember.upgrade().is_some_and(|r| r.is_active())
                && let Some(id) = app.id()
            {
                s.set_open_with_app(Some(id.to_string()));
            }
            if let Some(dlg) = dlg.upgrade() {
                dlg.close();
            }
            launch(&s, &app, &path);
        });
        list.append(&row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&list);
    content.append(&remember);
    if current.is_some() {
        let forget = gtk::Button::with_label("Forget Favourite");
        forget.add_css_class("flat");
        forget.set_halign(gtk::Align::Start);
        let (s, dlg) = (state.clone(), dialog.downgrade());
        forget.connect_clicked(move |_| {
            s.set_open_with_app(None);
            if let Some(dlg) = dlg.upgrade() {
                dlg.close();
            }
            s.toast("Favourite program forgotten");
        });
        content.append(&forget);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .child(&content)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&state.widgets().window));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::pairing::group_into_items;
    use crate::media::{FileKind, MediaFile};

    fn mf(path: &str) -> MediaFile {
        MediaFile {
            path: PathBuf::from(path),
            kind: FileKind::from_path(Path::new(path)).unwrap(),
            size: 1,
            mtime_ns: 1,
        }
    }

    #[test]
    fn opens_the_raw_of_a_pair() {
        let items = group_into_items(vec![mf("/p/IMG_1.JPG"), mf("/p/IMG_1.CR3")]);
        assert_eq!(items.len(), 1);
        assert_eq!(file_to_open(&items[0]), PathBuf::from("/p/IMG_1.CR3"));
    }

    #[test]
    fn opens_a_lone_jpeg_or_raw_as_is() {
        let items = group_into_items(vec![mf("/p/a.jpg"), mf("/p/b.ARW")]);
        let mut opened: Vec<PathBuf> = items.iter().map(file_to_open).collect();
        opened.sort();
        assert_eq!(
            opened,
            [PathBuf::from("/p/a.jpg"), PathBuf::from("/p/b.ARW")]
        );
    }
}
