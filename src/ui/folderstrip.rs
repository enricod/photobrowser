//! The strip of subfolders above the photo grid ("Show folders"). A single click (or Enter /
//! Space on a focused tile) opens a folder, like the path bar's ↑ button for the parent.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

/// A tile's label: the folder's own name.
pub fn tile_label(folder: &std::path::Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

#[derive(Clone)]
pub struct FolderStrip {
    pub root: gtk::ScrolledWindow,
    flow: gtk::FlowBox,
    paths: Rc<RefCell<Vec<PathBuf>>>,
}

impl FolderStrip {
    /// `open` is called with the folder of a clicked (or Enter/Space-activated) tile.
    pub fn new(open: impl Fn(PathBuf) + 'static) -> FolderStrip {
        let flow = gtk::FlowBox::builder()
            // Tiles open on click: nothing stays selected.
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .homogeneous(false)
            .row_spacing(6)
            .column_spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .max_children_per_line(64)
            .build();
        let root = gtk::ScrolledWindow::builder()
            .child(&flow)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            // About two rows of tiles; more scroll within the strip.
            .max_content_height(96)
            .visible(false)
            .build();
        let paths: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        let p = paths.clone();
        flow.connect_child_activated(move |_, child| {
            let path = usize::try_from(child.index())
                .ok()
                .and_then(|i| p.borrow().get(i).cloned());
            if let Some(path) = path {
                open(path);
            }
        });
        FolderStrip { root, flow, paths }
    }

    /// Shows a tile per subfolder, or hides the strip when it is disabled or there are none.
    pub fn show(&self, subfolders: &[PathBuf], enabled: bool) {
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }
        let shown: &[PathBuf] = if enabled { subfolders } else { &[] };
        for folder in shown {
            let icon = gtk::Image::from_icon_name("folder-symbolic");
            let label = gtk::Label::builder()
                .label(tile_label(folder))
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(24)
                .build();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            content.append(&icon);
            content.append(&label);
            content.set_margin_start(6);
            content.set_margin_end(6);
            content.set_margin_top(4);
            content.set_margin_bottom(4);
            content.set_tooltip_text(Some(&folder.to_string_lossy()));
            self.flow.append(&content);
        }
        *self.paths.borrow_mut() = shown.to_vec();
        self.root.set_visible(!shown.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn tiles_are_labelled_with_the_folder_name() {
        assert_eq!(tile_label(Path::new("/p/2026/Dolomiti")), "Dolomiti");
        assert_eq!(tile_label(Path::new("/p/2026-11-14")), "2026-11-14");
        assert_eq!(tile_label(Path::new("/")), "/");
    }
}
