//! Log Viewer for GTK4
//!
//! Simple debug window showing application logs.

use gtk4::prelude::*;
use gtk4::{ScrolledWindow, TextView, Window, WindowType};

/// Show the log viewer window
pub fn show_log_viewer(parent: &impl IsA<Window>) {
    let window = Window::builder()
        .title("Debug Log")
        .transient_for(parent)
        .default_width(700)
        .default_height(500)
        .build();

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);

    let text_view = TextView::new();
    text_view.set_editable(false);
    text_view.set_cursor_visible(false);
    text_view.set_monospace(true);
    text_view.set_wrap_mode(gtk4::WrapMode::None);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    text_view.set_top_margin(8);
    text_view.set_bottom_margin(8);

    // Apply dark styling
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(
        r#"
        textview {
            background-color: #1e1e1e;
            color: #d4d4d4;
        }
        textview text {
            background-color: #1e1e1e;
            color: #d4d4d4;
        }
    "#,
    );
    text_view
        .style_context()
        .add_provider(&provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);

    // Get logs from the capture buffer
    let logs = cterm_app::log_capture::get_logs_formatted();
    let buffer = text_view.buffer();
    buffer.set_text(&logs);

    // Scroll to bottom
    let end_iter = buffer.end_iter();
    let mark = buffer.create_mark(None, &end_iter, false);
    text_view.scroll_to_mark(&mark, 0.0, false, 0.0, 0.0);

    scroll.set_child(Some(&text_view));
    window.set_child(Some(&scroll));
    window.present();
}
