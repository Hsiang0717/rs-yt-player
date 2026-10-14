use std::sync::{Arc, Mutex};

use crate::library::LibraryStore;
use crate::queue::PlayQueue;
use crate::ui_models::{apply_queue_expanded, queue_items_model};
use crate::AppWindow;
use slint::ComponentHandle;

pub fn bind_window_controls(
    app: &AppWindow,
    main_hwnd: Arc<Mutex<Option<windows::Win32::Foundation::HWND>>>,
) {
    let hwnd_for_drag = Arc::clone(&main_hwnd);
    app.on_start_window_drag(move || {
        if let Some(hwnd) = *hwnd_for_drag.lock().unwrap() {
            crate::win32_window::start_window_drag(hwnd);
        }
    });

    let hwnd_for_min = Arc::clone(&main_hwnd);
    app.on_minimize_window(move || {
        if let Some(hwnd) = *hwnd_for_min.lock().unwrap() {
            crate::win32_window::minimize_to_tray(hwnd);
        }
    });

    app.on_close_window(move || {
        let _ = slint::quit_event_loop();
    });
}

pub fn bind_navigation_controls(app: &AppWindow, queue: PlayQueue, library: LibraryStore) {
    let ui_weak = app.as_weak();
    let queue_for_toggle = queue;
    let library_for_toggle = library;
    let last_toggle = Arc::new(Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)));
    app.on_toggle_queue(move || {
        let mut last = last_toggle.lock().unwrap();
        if last.elapsed() < std::time::Duration::from_millis(220) {
            return;
        }
        *last = std::time::Instant::now();
        if let Some(ui) = ui_weak.upgrade() {
            let expanded = !ui.get_queue_expanded();
            apply_queue_expanded(&ui, expanded);
            if expanded {
                ui.set_queue_items(queue_items_model(&queue_for_toggle, &library_for_toggle));
            }
        }
    });

    let ui_weak_tab = app.as_weak();
    app.on_select_panel_tab(move |tab| {
        if let Some(ui) = ui_weak_tab.upgrade() {
            ui.set_panel_tab(tab);
            ui.set_panel_message(slint::SharedString::default());
        }
    });
}
