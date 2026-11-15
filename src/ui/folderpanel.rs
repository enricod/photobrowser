//! The "Show folders" panel on the right of the grid: the open folder's subfolders as a
//! filterable list. A single click (or Enter) opens one; Enter in the filter box opens the
//! first match. The list is virtualized, so hundreds of folders stay smooth.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

/// A folder's label in the list: its own name.
pub fn folder_label(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

/// Whether `name` passes the filter: case-insensitive, matching anywhere; an empty filter
/// matches everything.
pub fn matches(name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || name.to_lowercase().contains(&query)
}

/// The line under the filter box.
pub fn count_text(shown: u32, total: u32) -> String {
    match (shown, total) {
        (_, 0) => "No subfolders".into(),
        (s, t) if s == t => format!("{t} folder{}", if t == 1 { "" } else { "s" }),
        (s, t) => format!("{s} of {t}"),
    }
}

fn path_of(obj: &glib::Object) -> Option<PathBuf> {
    obj.downcast_ref::<glib::BoxedAnyObject>()
        .map(|b| b.borrow::<PathBuf>().clone())
}

#[derive(Clone)]
pub struct FolderPanel {
    pub root: gtk::Box,
    store: gio::ListStore,
    filtered: gtk::FilterListModel,
    search: gtk::SearchEntry,
    count: gtk::Label,
}

impl FolderPanel {
    /// `open` is called with the folder the user picked.
    pub fn new(open: impl Fn(PathBuf) + 'static) -> FolderPanel {
        let open = Rc::new(open);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query: Rc<RefCell<String>> = Rc::default();
        let q = query.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            path_of(obj).is_some_and(|p| matches(&folder_label(&p), &q.borrow()))
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_start(6);
            row.set_margin_end(6);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            row.append(&gtk::Image::from_icon_name("folder-symbolic"));
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            row.append(&label);
            item.set_child(Some(&row));
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let (Some(path), Some(row)) = (
                item.item().and_then(|o| path_of(&o)),
                item.child().and_downcast::<gtk::Box>(),
            ) else {
                return;
            };
            if let Some(label) = row.last_child().and_downcast::<gtk::Label>() {
                label.set_label(&folder_label(&path));
            }
            row.set_tooltip_text(Some(&path.to_string_lossy()));
        });

        let list = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(filtered.clone())))
            .factory(&factory)
            .single_click_activate(true)
            .build();
        list.add_css_class("navigation-sidebar");
        let o = open.clone();
        list.connect_activate(move |view, position| {
            let path = view
                .model()
                .and_then(|m| m.item(position))
                .and_then(|obj| path_of(&obj));
            if let Some(path) = path {
                o(path);
            }
        });

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filter folders")
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .build();
        let count = gtk::Label::builder()
            .xalign(0.0)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        count.add_css_class("caption");
        count.add_css_class("dim-label");

        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search);
        root.append(&count);
        root.append(&scroller);

        let panel = FolderPanel {
            root,
            store,
            filtered,
            search,
            count,
        };

        let p = panel.clone();
        panel.search.connect_search_changed(move |entry| {
            *query.borrow_mut() = entry.text().to_string();
            filter.changed(gtk::FilterChange::Different);
            p.update_count();
        });
        let p = panel.clone();
        panel.search.connect_activate(move |_| {
            if let Some(path) = p.filtered.item(0).and_then(|obj| path_of(&obj)) {
                open(path);
            }
        });
        panel.update_count();
        panel
    }

    /// Replaces the list (after a scan), keeping the current filter.
    pub fn set_folders(&self, folders: &[PathBuf]) {
        let objects: Vec<glib::BoxedAnyObject> = folders
            .iter()
            .map(|p| glib::BoxedAnyObject::new(p.clone()))
            .collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.update_count();
    }

    /// Clears the filter, e.g. when another folder is opened.
    pub fn clear_filter(&self) {
        self.search.set_text("");
    }

    fn update_count(&self) {
        self.count
            .set_label(&count_text(self.filtered.n_items(), self.store.n_items()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_case_insensitively_anywhere() {
        assert!(matches("2024-05-06", "2024-05"));
        assert!(matches("2024-05-06", "05-06"));
        assert!(matches("Dolomiti", "dolo"));
        assert!(matches("Dolomiti", "  MIT "));
        assert!(matches("anything", ""));
        assert!(!matches("2024-04-06", "2024-05"));
    }

    #[test]
    fn count_line() {
        assert_eq!(count_text(0, 0), "No subfolders");
        assert_eq!(count_text(1, 1), "1 folder");
        assert_eq!(count_text(366, 366), "366 folders");
        assert_eq!(count_text(31, 366), "31 of 366");
        assert_eq!(count_text(0, 366), "0 of 366");
    }

    #[test]
    fn labels_are_folder_names() {
        assert_eq!(folder_label(Path::new("/p/2024/2024-02-11")), "2024-02-11");
        assert_eq!(folder_label(Path::new("/")), "/");
    }
}
