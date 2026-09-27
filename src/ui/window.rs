use std::path::PathBuf;

use adw::prelude::*;
use gtk::glib;

use super::state::{AppState, Widgets};
use super::{grid, keys, style};

pub fn build(app: &adw::Application, start_folder: Option<PathBuf>) {
    let (tx, rx) = async_channel::unbounded();
    let state = AppState::new(tx);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Photobrowser")
        .default_width(1400)
        .default_height(900)
        .build();

    let header = adw::HeaderBar::new();
    let open_button = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Open Folder (Ctrl+O)")
        .build();
    header.pack_start(&open_button);
    let counter = gtk::Label::new(None);
    counter.add_css_class("dim-label");
    counter.add_css_class("numeric");
    header.pack_end(&counter);

    let grid = grid::build(&state);
    let grid_scroller = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let empty_button = gtk::Button::builder()
        .label("Open Folder…")
        .halign(gtk::Align::Center)
        .build();
    empty_button.add_css_class("pill");
    empty_button.add_css_class("suggested-action");
    let empty = adw::StatusPage::builder()
        .icon_name("folder-pictures-symbolic")
        .title("No Folder Open")
        .description("Choose a folder of photos to start culling")
        .child(&empty_button)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&grid_scroller, Some("grid"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let _ = state.widgets.set(Widgets { window: window.clone(), toasts, stack, grid, counter });

    for button in [&open_button, &empty_button] {
        let s = state.clone();
        button.connect_clicked(move |_| s.choose_folder());
    }
    let s = state.clone();
    state.selection.connect_items_changed(move |_, _, _, _| s.update_counter());

    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    key_controller.connect_key_pressed(move |_, key, _, mods| keys::handle(&s, key, mods));
    window.add_controller(key_controller);

    style::set_thumb_size(state.config.borrow().thumb_display_size);

    let s = state.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            s.handle(msg);
        }
    });
    let disk = state.disk.clone();
    std::thread::spawn(move || {
        let _ = disk.cleanup();
    });

    let last = state.config.borrow().last_folder.clone();
    match start_folder.or(last).filter(|p| p.is_dir()) {
        Some(folder) => state.open_folder(folder),
        None => state.widgets().stack.set_visible_child_name("empty"),
    }
    window.present();
}
