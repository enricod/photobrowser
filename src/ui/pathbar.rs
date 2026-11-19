//! The bar under the header showing the open folder's path.

use std::cell::RefCell;
use std::path::{Component, Path};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

/// A folder path as shown to the user: where it lives (`~`, `/`, `smb://nas01.local`)
/// and the folders below that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayPath {
    head: String,
    parts: Vec<String>,
}

impl DisplayPath {
    fn new(head: impl Into<String>, parts: Vec<String>) -> DisplayPath {
        DisplayPath {
            head: head.into(),
            parts,
        }
    }

    /// `head` followed by `tail`, e.g. "smb://nas01.local" + ["a", "b"].
    fn join(&self, tail: &[String]) -> String {
        let mut s = self.head.clone();
        for (i, part) in tail.iter().enumerate() {
            if i > 0 || !s.ends_with('/') {
                s.push('/');
            }
            s.push_str(part);
        }
        s
    }

    /// The whole path, e.g. "smb://nas01.local/enrico-documents/Pictures".
    pub fn full(&self) -> String {
        self.join(&self.parts)
    }

    /// The path from longest to shortest: whole, then with ever more folders after the head
    /// replaced by "…". The last two folders are always kept.
    pub fn candidates(&self) -> Vec<String> {
        let mut out = vec![self.full()];
        let n = self.parts.len();
        for first_kept in 1..n.saturating_sub(1) {
            let mut tail = vec!["…".to_owned()];
            tail.extend_from_slice(&self.parts[first_kept..]);
            out.push(self.join(&tail));
        }
        out
    }
}

/// A folder path for display, with the home folder shortened to `~` (e.g. "~/Docs") and
/// network shares mounted by GVFS shown by their address (e.g. "smb://nas01.local/docs").
pub fn display_dir(path: &Path) -> String {
    display_path(path, &glib::home_dir()).full()
}

pub fn display_path(path: &Path, home: &Path) -> DisplayPath {
    if let Some(remote) = gvfs_display_path(path) {
        return remote;
    }
    // A root or empty home would turn every path into "~/…".
    // `strip_prefix` compares whole components, so "/home/enrico2" is not under "/home/enrico".
    if home.parent().is_some()
        && let Ok(rest) = path.strip_prefix(home)
    {
        return DisplayPath::new("~", names(rest));
    }
    if path.is_absolute() {
        DisplayPath::new("/", names(path))
    } else {
        DisplayPath::new("", names(path))
    }
}

fn names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// A path inside a GVFS FUSE mount (`/run/user/<uid>/gvfs/<mount>/…`) as the address of the
/// share it belongs to, or `None` for any other path or a mount type it doesn't know.
fn gvfs_display_path(path: &Path) -> Option<DisplayPath> {
    let all = names(path);
    let [run, user, uid, gvfs, mount, rest @ ..] = all.as_slice() else {
        return None;
    };
    if !path.is_absolute()
        || run != "run"
        || user != "user"
        || !uid.bytes().all(|b| b.is_ascii_digit())
        || gvfs != "gvfs"
    {
        return None;
    }
    let (kind, params) = mount.split_once(':')?;
    let param = |key: &str| {
        params.split(',').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| unescape(v))
        })
    };
    // "user@host:port", the authority part of the address.
    let authority = |host: String| {
        let mut s = String::new();
        if let Some(user) = param("user") {
            s.push_str(&user);
            s.push('@');
        }
        s.push_str(&host);
        if let Some(port) = param("port") {
            s.push(':');
            s.push_str(&port);
        }
        s
    };
    let mut parts = Vec::new();
    let head = match kind {
        "smb-share" => {
            parts.extend(param("share"));
            format!("smb://{}", authority(param("server")?))
        }
        "smb-server" => format!("smb://{}", authority(param("server")?)),
        "afp-volume" => {
            parts.extend(param("volume"));
            format!("afp://{}", authority(param("host")?))
        }
        "dav" => {
            let scheme = if param("ssl").as_deref() == Some("true") {
                "davs"
            } else {
                "dav"
            };
            parts.extend(
                param("prefix")
                    .map(|p| names(Path::new(&p)))
                    .unwrap_or_default(),
            );
            format!("{scheme}://{}", authority(param("host")?))
        }
        "nfs" => {
            parts.extend(
                param("prefix")
                    .map(|p| names(Path::new(&p)))
                    .unwrap_or_default(),
            );
            format!("nfs://{}", authority(param("host")?))
        }
        "sftp" | "ftp" | "ftps" | "ftpis" | "afc" | "mtp" | "gphoto2" => {
            let host = param("host").or_else(|| param("server"))?;
            format!("{kind}://{}", authority(host))
        }
        _ => return None,
    };
    parts.extend(rest.iter().cloned());
    Some(DisplayPath::new(head, parts))
}

/// GVFS escapes characters that would break a mount name, e.g. "%2C" for ",".
fn unescape(s: &str) -> String {
    glib::Uri::unescape_string(s, None::<&str>)
        .map(|u| u.to_string())
        .unwrap_or_else(|| s.to_owned())
}

/// Shown in the grid and the viewer, hidden while no folder is open.
#[derive(Clone)]
pub struct PathBar {
    pub root: gtk::Box,
    label: gtk::Label,
    /// Opens the parent folder; insensitive at the root.
    pub up: gtk::Button,
    /// "Show folders": the subfolder strip above the grid.
    pub show_folders: gtk::CheckButton,
    /// The open folder's path, refitted to the label whenever its width changes.
    shown: Rc<RefCell<Option<DisplayPath>>>,
    /// Takes the label's place in the layout and reports its width.
    sizer: gtk::DrawingArea,
}

impl PathBar {
    pub fn new() -> PathBar {
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.add_css_class("dim-label");
        let label = gtk::Label::builder()
            .xalign(0.0)
            // A last resort when even the shortest form doesn't fit.
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .selectable(true)
            .build();
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        // The label is laid over a drawing area that fills the free width: its `resize`
        // says how wide the path may be, without the label's own text feeding back into it.
        let sizer = gtk::DrawingArea::builder().hexpand(true).build();
        let slot = gtk::Overlay::new();
        slot.set_child(Some(&sizer));
        slot.add_overlay(&label);
        slot.set_measure_overlay(&label, true);
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .visible(false)
            .build();
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Open Parent Folder (Alt+↑)")
            .valign(gtk::Align::Center)
            .build();
        up.add_css_class("flat");
        root.append(&up);
        root.append(&icon);
        root.append(&slot);
        let show_folders = gtk::CheckButton::builder()
            .label("Show folders")
            .valign(gtk::Align::Center)
            .build();
        show_folders.add_css_class("caption");
        root.append(&show_folders);
        let bar = PathBar {
            root,
            label,
            up,
            show_folders,
            shown: Rc::default(),
            sizer,
        };
        let b = bar.clone();
        bar.sizer.connect_resize(move |_, _, _| {
            // Changing the label while it is being allocated would warn: do it right after.
            let b = b.clone();
            glib::idle_add_local_once(move || b.fit());
        });
        bar
    }

    /// Shows `folder`, or hides the bar when there is none.
    pub fn set_folder(&self, folder: Option<&Path>) {
        match folder {
            Some(folder) => {
                *self.shown.borrow_mut() = Some(display_path(folder, &glib::home_dir()));
                self.fit();
                self.label.set_tooltip_text(Some(&folder.to_string_lossy()));
                self.up.set_sensitive(folder.parent().is_some());
                self.root.set_visible(true);
            }
            None => {
                *self.shown.borrow_mut() = None;
                self.label.set_label("");
                self.label.set_tooltip_text(None);
                self.root.set_visible(false);
            }
        }
    }

    /// Shows the longest form of the path that fits the available width.
    fn fit(&self) {
        let Some(shown) = self.shown.borrow().clone() else {
            return;
        };
        let width = self.sizer.width();
        let candidates = shown.candidates();
        let text = candidates
            .iter()
            .find(|c| width <= 0 || self.label.create_pango_layout(Some(c)).pixel_size().0 <= width)
            .or(candidates.last())
            .cloned()
            .unwrap_or_default();
        if self.label.label() != text {
            self.label.set_label(&text);
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

    const SHARE: &str = "/run/user/1000/gvfs/smb-share:server=nas01.local,share=enrico-documents";

    fn show(p: &str) -> String {
        display_path(Path::new(p), Path::new("/home/enrico")).full()
    }

    #[test]
    fn display_dir_shortens_home() {
        assert_eq!(show("/home/enrico/Docs"), "~/Docs");
        assert_eq!(show("/home/enrico/Pictures/2026"), "~/Pictures/2026");
        assert_eq!(show("/home/enrico"), "~");
        assert_eq!(show("/run/media/enrico/USB"), "/run/media/enrico/USB");
        assert_eq!(show("/home/enrico2/x"), "/home/enrico2/x");
        assert_eq!(show("/"), "/");
        assert_eq!(
            display_path(Path::new("/srv/x"), Path::new("/")).full(),
            "/srv/x"
        );
    }

    #[test]
    fn gvfs_smb_share_shown_as_address() {
        assert_eq!(
            show(&format!("{SHARE}/Pictures/photographs/2023")),
            "smb://nas01.local/enrico-documents/Pictures/photographs/2023"
        );
        assert_eq!(show(SHARE), "smb://nas01.local/enrico-documents");
        assert_eq!(
            show("/run/user/1000/gvfs/smb-share:domain=WG,server=nas,share=a%2Cb,user=me/x"),
            "smb://me@nas/a,b/x"
        );
    }

    #[test]
    fn gvfs_other_mounts_shown_as_address() {
        assert_eq!(
            show("/run/user/1000/gvfs/sftp:host=pi.local,port=2222,user=enrico/home/pics"),
            "sftp://enrico@pi.local:2222/home/pics"
        );
        assert_eq!(
            show(
                "/run/user/1000/gvfs/dav:host=cloud.example,ssl=true,prefix=%2Fremote.php%2Fdav/Photos"
            ),
            "davs://cloud.example/remote.php/dav/Photos"
        );
        assert_eq!(
            show("/run/user/1000/gvfs/nfs:host=nas,prefix=%2Fexport%2Fphotos/2024"),
            "nfs://nas/export/photos/2024"
        );
        assert_eq!(
            show("/run/user/1000/gvfs/ftp:host=ftp.example/pub"),
            "ftp://ftp.example/pub"
        );
    }

    #[test]
    fn unknown_or_partial_gvfs_paths_stay_as_they_are() {
        for p in [
            "/run/user/1000/gvfs/weird:thing=1/x",
            "/run/user/1000/gvfs/no-colon/x",
            "/run/user/1000/gvfs",
            "/run/user/abc/gvfs/smb-share:server=a,share=b",
            "/run/user/1000/gvfs/smb-share:share=b/x",
        ] {
            assert_eq!(show(p), p);
        }
    }

    #[test]
    fn candidates_drop_whole_folders_after_the_head() {
        let d = display_path(
            Path::new(&format!("{SHARE}/Pictures/photographs/2023/2023-11-22")),
            Path::new("/home/enrico"),
        );
        assert_eq!(
            d.candidates(),
            [
                "smb://nas01.local/enrico-documents/Pictures/photographs/2023/2023-11-22",
                "smb://nas01.local/…/Pictures/photographs/2023/2023-11-22",
                "smb://nas01.local/…/photographs/2023/2023-11-22",
                "smb://nas01.local/…/2023/2023-11-22",
            ]
        );
        let local = display_path(Path::new("/home/enrico/a/b/c"), Path::new("/home/enrico"));
        assert_eq!(local.candidates(), ["~/a/b/c", "~/…/b/c"]);
        let root = display_path(Path::new("/a/b/c"), Path::new("/home/enrico"));
        assert_eq!(root.candidates(), ["/a/b/c", "/…/b/c"]);
    }

    #[test]
    fn short_paths_have_one_candidate() {
        for p in ["/home/enrico", "/home/enrico/a", "/home/enrico/a/b", "/"] {
            let d = display_path(Path::new(p), Path::new("/home/enrico"));
            assert_eq!(d.candidates().len(), 1, "{p}");
        }
    }
}
