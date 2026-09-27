use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use super::item_object::ItemObject;
use super::state::AppState;

fn list_item_object(obj: &glib::Object) -> Option<ItemObject> {
    obj.downcast_ref::<gtk::ListItem>()?
        .item()
        .and_downcast::<ItemObject>()
}

/// Cell factory shared by the grid (`compact = false`) and the filmstrip (`compact = true`).
pub fn thumb_factory(state: &Rc<AppState>, compact: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .build();
        picture.add_css_class(if compact { "pb-strip-thumb" } else { "pb-thumb" });

        let broken = gtk::Image::from_icon_name("image-missing-symbolic");
        broken.set_pixel_size(48);
        broken.set_halign(gtk::Align::Center);
        broken.set_valign(gtk::Align::Center);

        let badge = gtk::Label::new(Some("RAW+JPG"));
        badge.add_css_class("pb-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);

        let reject = gtk::Label::new(Some("✕"));
        reject.add_css_class("pb-reject");
        reject.set_halign(gtk::Align::Start);
        reject.set_valign(gtk::Align::Start);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&picture));
        overlay.add_overlay(&broken);
        overlay.add_overlay(&badge);
        overlay.add_overlay(&reject);

        let stars = gtk::Label::new(None);
        stars.add_css_class("pb-stars");
        stars.set_visible(!compact);

        let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
        cell.add_css_class("pb-cell");
        cell.append(&overlay);
        cell.append(&stars);

        let item = li.property_expression("item");
        item.chain_property::<ItemObject>("thumbnail").bind(&picture, "paintable", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("has-error").bind(&broken, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("error").bind(&broken, "tooltip-text", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("is-pair").bind(&badge, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("rejected").bind(&reject, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("name").bind(&cell, "tooltip-text", gtk::Widget::NONE);
        if !compact {
            item.chain_property::<ItemObject>("stars").bind(&stars, "label", gtk::Widget::NONE);
        }
        li.set_child(Some(&cell));
    });

    let s = state.clone();
    factory.connect_bind(move |_, obj| {
        if let Some(item) = list_item_object(obj) {
            s.bind_cell(&item);
        }
    });
    let s = state.clone();
    factory.connect_unbind(move |_, obj| {
        if let Some(item) = list_item_object(obj) {
            s.unbind_cell(&item);
        }
    });
    factory
}

pub fn build(state: &Rc<AppState>) -> gtk::GridView {
    let grid = gtk::GridView::new(Some(state.selection.clone()), Some(thumb_factory(state, false)));
    grid.set_max_columns(64);
    grid.set_enable_rubberband(true);
    grid
}
