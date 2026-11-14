//! The bar under the header showing the open folder's path.

use std::path::Path;

use adw::prelude::*;
use gtk::glib;

/// A folder path for display, with the home folder shortened to `~` (e.g. "~/Docs").
pub fn display_dir(path: &Path) -> String {
    display_dir_with_home(path, &glib::home_dir())
}

fn display_dir_with_home(path: &Path, home: &Path) -> String {
    // A root or empty home would turn every path into "~/…".
    if home.parent().is_none() {
        return path.display().to_string();
    }
    // `strip_prefix` compares whole components, so "/home/enrico2" is not under "/home/enrico".
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Shown in the grid and the viewer, hidden while no folder is open.
#[derive(Clone)]
pub struct PathBar {
    pub root: gtk::Box,
    label: gtk::Label,
}

impl PathBar {
    pub fn new() -> PathBar {
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.add_css_class("dim-label");
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            // Keep the start and the folder's own name visible on long paths.
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .selectable(true)
            .build();
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .visible(false)
            .build();
        root.append(&icon);
        root.append(&label);
        PathBar { root, label }
    }

    /// Shows `folder`, or hides the bar when there is none.
    pub fn set_folder(&self, folder: Option<&Path>) {
        match folder {
            Some(folder) => {
                self.label.set_label(&display_dir(folder));
                self.root.set_tooltip_text(Some(&folder.to_string_lossy()));
                self.root.set_visible(true);
            }
            None => {
                self.label.set_label("");
                self.root.set_tooltip_text(None);
                self.root.set_visible(false);
            }
        }
    }
}

impl Default for PathBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_dir_shortens_home() {
        let home = Path::new("/home/enrico");
        let show = |p: &str| display_dir_with_home(Path::new(p), home);
        assert_eq!(show("/home/enrico/Docs"), "~/Docs");
        assert_eq!(show("/home/enrico/Pictures/2026"), "~/Pictures/2026");
        assert_eq!(show("/home/enrico"), "~");
        assert_eq!(show("/run/media/enrico/USB"), "/run/media/enrico/USB");
        assert_eq!(show("/home/enrico2/x"), "/home/enrico2/x");
        assert_eq!(
            display_dir_with_home(Path::new("/srv/x"), Path::new("/")),
            "/srv/x"
        );
    }
}
