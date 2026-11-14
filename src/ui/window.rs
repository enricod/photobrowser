use std::cell::Cell;
use std::path::PathBuf;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use super::pathbar::PathBar;
use super::state::{AppState, Widgets};
use super::viewer::Viewer;
use super::{filterbar, grid, keys, prefs, style};

pub fn build(app: &adw::Application, start_folder: Option<PathBuf>) {
    let (tx, rx) = async_channel::unbounded();
    let state = AppState::new(tx, rx.clone());

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
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .build();
    header.pack_end(&menu_button);
    let filters = filterbar::build(&state);
    header.set_title_widget(Some(&filters));

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
    let viewer = Viewer::new(&state);
    stack.add_named(&viewer.root, Some("viewer"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let path_bar = PathBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&path_bar.root);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let _ = state.widgets.set(Widgets {
        window: window.clone(),
        toasts,
        stack,
        grid,
        counter,
        filters,
        viewer,
        path_bar,
    });

    let s = state.clone();
    window.connect_close_request(move |_| s.close_requested());

    let s = state.clone();
    let preferences = gio::ActionEntry::builder("preferences")
        .activate(move |_: &adw::ApplicationWindow, _, _| prefs::show(&s))
        .build();
    window.add_action_entries([preferences]);

    for button in [&open_button, &empty_button] {
        let s = state.clone();
        button.connect_clicked(move |_| s.choose_folder());
    }
    let s = state.clone();
    state.selection.connect_items_changed(move |_, _, _, _| {
        s.update_counter();
        s.queue_viewer_sync();
    });

    let s = state.clone();
    state.selection.connect_selection_changed(move |_, _, _| {
        s.unpin();
        s.follow_selection();
    });

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    let scroll_accum = Cell::new(0.0_f64);
    scroll.connect_scroll(move |ctl, _dx, dy| {
        if !ctl
            .current_event_state()
            .contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            scroll_accum.set(0.0);
            return glib::Propagation::Proceed;
        }
        if dy == 0.0 {
            return glib::Propagation::Stop;
        }
        // Touchpads deliver many small deltas per gesture; wheels deliver ±1.0 per notch.
        // Accumulate and step once per whole unit crossed, in either direction.
        let mut acc = scroll_accum.get() + dy;
        while acc <= -1.0 {
            s.zoom_thumbs(1);
            acc += 1.0;
        }
        while acc >= 1.0 {
            s.zoom_thumbs(-1);
            acc -= 1.0;
        }
        scroll_accum.set(acc);
        glib::Propagation::Stop
    });
    grid_scroller.add_controller(scroll);

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
    state.show_startup_warnings();
}
