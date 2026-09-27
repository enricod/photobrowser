use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

use crate::media::{Item, ItemId};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ItemObject {
        pub item: RefCell<Option<Item>>,
        pub thumbnail: RefCell<Option<gdk::Texture>>,
        pub error: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemObject {
        const NAME: &'static str = "PbItemObject";
        type Type = super::ItemObject;
    }

    impl ObjectImpl for ItemObject {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPS.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("stars").read_only().build(),
                    glib::ParamSpecBoolean::builder("rejected").read_only().build(),
                    glib::ParamSpecBoolean::builder("is-pair").read_only().build(),
                    glib::ParamSpecString::builder("name").read_only().build(),
                    glib::ParamSpecObject::builder::<gdk::Paintable>("thumbnail").read_only().build(),
                    glib::ParamSpecBoolean::builder("has-error").read_only().build(),
                    glib::ParamSpecString::builder("error").read_only().build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let item = self.item.borrow();
            let item = item.as_ref();
            match pspec.name() {
                "stars" => item
                    .map(|i| if i.rejected { String::new() } else { stars_text(i.rating) })
                    .unwrap_or_default()
                    .to_value(),
                "rejected" => item.is_some_and(|i| i.rejected).to_value(),
                "is-pair" => item.is_some_and(|i| i.is_pair()).to_value(),
                "name" => item.map(|i| i.file_names()).unwrap_or_default().to_value(),
                "thumbnail" => self
                    .thumbnail
                    .borrow()
                    .as_ref()
                    .map(|t| t.clone().upcast::<gdk::Paintable>())
                    .to_value(),
                "has-error" => self.error.borrow().is_some().to_value(),
                "error" => self.error.borrow().clone().to_value(),
                name => unreachable!("unknown property {name}"),
            }
        }
    }
}

glib::wrapper! {
    pub struct ItemObject(ObjectSubclass<imp::ItemObject>);
}

impl ItemObject {
    pub fn new(item: Item) -> ItemObject {
        let obj: ItemObject = glib::Object::new();
        obj.imp().item.replace(Some(item));
        obj
    }

    pub fn id(&self) -> ItemId {
        self.with_item(|i| i.id)
    }

    pub fn item(&self) -> Item {
        self.with_item(Item::clone)
    }

    pub fn with_item<R>(&self, f: impl FnOnce(&Item) -> R) -> R {
        f(self.imp().item.borrow().as_ref().expect("ItemObject without item"))
    }

    pub fn set_item(&self, item: Item) {
        self.imp().item.replace(Some(item));
        self.notify_item();
    }

    pub fn update_item(&self, f: impl FnOnce(&mut Item)) {
        f(self.imp().item.borrow_mut().as_mut().expect("ItemObject without item"));
        self.notify_item();
    }

    fn notify_item(&self) {
        for name in ["stars", "rejected", "is-pair", "name"] {
            self.notify(name);
        }
    }

    pub fn thumbnail(&self) -> Option<gdk::Texture> {
        self.imp().thumbnail.borrow().clone()
    }

    pub fn set_thumbnail(&self, texture: Option<gdk::Texture>) {
        self.imp().thumbnail.replace(texture);
        self.notify("thumbnail");
    }

    pub fn has_error(&self) -> bool {
        self.imp().error.borrow().is_some()
    }

    pub fn set_error(&self, error: Option<String>) {
        self.imp().error.replace(error);
        self.notify("has-error");
        self.notify("error");
    }
}

pub fn stars_text(rating: u8) -> String {
    let r = rating.min(5) as usize;
    format!("{}{}", "★".repeat(r), "☆".repeat(5 - r))
}

#[cfg(test)]
mod tests {
    use super::stars_text;

    #[test]
    fn renders_stars() {
        assert_eq!(stars_text(0), "☆☆☆☆☆");
        assert_eq!(stars_text(3), "★★★☆☆");
        assert_eq!(stars_text(9), "★★★★★");
    }
}
