use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::gdk;

use super::grid::thumb_factory;
use super::item_object::{stars_text, ItemObject};
use super::state::{AppState, Zoom};
use crate::exif::ExifInfo;
use crate::media::{Item, ItemId};

/// Overlay text: file names, rating, EXIF summary.
pub fn info_text(item: &Item, exif: Option<&ExifInfo>) -> String {
    let status = if item.rejected {
        "Rejected".to_string()
    } else if item.rating == 0 {
        "Unrated".to_string()
    } else {
        stars_text(item.rating)
    };
    let mut lines = vec![item.file_names(), status];
    if let Some(summary) = exif.map(ExifInfo::summary).filter(|s| !s.is_empty()) {
        lines.push(summary);
    }
    lines.join("\n")
}

pub struct Viewer {
    pub root: gtk::Overlay,
    scroller: gtk::ScrolledWindow,
    picture: gtk::Picture,
    info: gtk::Label,
    strip_revealer: gtk::Revealer,
    zoom: Cell<Zoom>,
    current: RefCell<Option<ItemObject>>,
    drag_origin: Cell<(f64, f64)>,
}

impl Viewer {
    pub fn new(state: &Rc<AppState>) -> Rc<Viewer> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&picture)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .build();

        let strip = gtk::ListView::new(Some(state.selection.clone()), Some(thumb_factory(state, true)));
        strip.set_orientation(gtk::Orientation::Horizontal);
        strip.set_single_click_activate(true);
        let strip_scroller = gtk::ScrolledWindow::builder()
            .child(&strip)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(96)
            .build();
        let strip_revealer = gtk::Revealer::builder()
            .child(&strip_scroller)
            .reveal_child(true)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&scroller);
        column.append(&strip_revealer);

        let info = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .xalign(0.0)
            .visible(false)
            .build();
        info.add_css_class("pb-osd");

        let root = gtk::Overlay::new();
        root.set_child(Some(&column));
        root.add_overlay(&info);
        root.add_css_class("pb-viewer");

        let viewer = Rc::new(Viewer {
            root,
            scroller,
            picture,
            info,
            strip_revealer,
            zoom: Cell::new(Zoom::Fit),
            current: RefCell::new(None),
            drag_origin: Cell::new((0.0, 0.0)),
        });

        let click = gtk::GestureClick::new();
        let s = state.clone();
        click.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                s.toggle_zoom();
            }
        });
        viewer.scroller.add_controller(click);

        let drag = gtk::GestureDrag::new();
        let v = Rc::downgrade(&viewer);
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(v) = v.upgrade() {
                v.drag_origin
                    .set((v.scroller.hadjustment().value(), v.scroller.vadjustment().value()));
            }
        });
        let v = Rc::downgrade(&viewer);
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(v) = v.upgrade() {
                let (h, vv) = v.drag_origin.get();
                v.scroller.hadjustment().set_value(h - dx);
                v.scroller.vadjustment().set_value(vv - dy);
            }
        });
        viewer.scroller.add_controller(drag);

        let s = state.clone();
        strip.connect_activate(move |_, pos| s.show_at(pos));
        viewer
    }

    pub fn current(&self) -> Option<ItemObject> {
        self.current.borrow().clone()
    }

    pub fn current_id(&self) -> Option<ItemId> {
        self.current.borrow().as_ref().map(ItemObject::id)
    }

    /// Shows `obj` at fit-to-window; the caller supplies the first texture.
    pub fn set_current(&self, obj: &ItemObject, info: &str) {
        *self.current.borrow_mut() = Some(obj.clone());
        self.set_zoom(Zoom::Fit);
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.info.set_label(info);
    }

    pub fn set_texture(&self, texture: &gdk::Texture) {
        self.picture.set_paintable(Some(texture));
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom.get()
    }

    pub fn set_zoom(&self, zoom: Zoom) {
        self.zoom.set(zoom);
        let (policy, shrink) = match zoom {
            Zoom::Fit => (gtk::PolicyType::Never, true),
            Zoom::Actual => (gtk::PolicyType::Automatic, false),
        };
        self.scroller.set_policy(policy, policy);
        self.picture.set_can_shrink(shrink);
    }

    pub fn set_info(&self, text: &str) {
        self.info.set_label(text);
    }

    pub fn info_visible(&self) -> bool {
        self.info.is_visible()
    }

    pub fn toggle_info(&self) -> bool {
        let visible = !self.info.is_visible();
        self.info.set_visible(visible);
        visible
    }

    pub fn toggle_strip(&self) {
        self.strip_revealer
            .set_reveal_child(!self.strip_revealer.reveals_child());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{FileKind, MediaFile};
    use std::path::PathBuf;

    fn item(rating: u8, rejected: bool) -> Item {
        let mut i = Item::new(vec![MediaFile {
            path: PathBuf::from("/p/IMG_1.JPG"),
            kind: FileKind::Jpeg,
            size: 1,
            mtime_ns: 0,
        }]);
        i.rating = rating;
        i.rejected = rejected;
        i
    }

    #[test]
    fn info_shows_rating_state() {
        assert_eq!(info_text(&item(0, false), None), "IMG_1.JPG\nUnrated");
        assert_eq!(info_text(&item(3, false), None), "IMG_1.JPG\n★★★☆☆");
        assert_eq!(info_text(&item(3, true), None), "IMG_1.JPG\nRejected");
    }

    #[test]
    fn info_appends_exif_summary() {
        let exif = ExifInfo { iso: Some(200), ..Default::default() };
        assert_eq!(info_text(&item(1, false), Some(&exif)), "IMG_1.JPG\n★☆☆☆☆\nISO 200");
        assert_eq!(info_text(&item(1, false), Some(&ExifInfo::default())), "IMG_1.JPG\n★☆☆☆☆");
    }
}
