//! cterm - A high-performance terminal emulator
//!
//! Main entry point for the GTK4 application.

mod app;
mod dialogs;
mod menu;
mod tab_bar;
mod terminal_widget;
mod window;

use gtk4::prelude::*;
use gtk4::{glib, Application};

fn main() -> glib::ExitCode {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting cterm");

    // Create the GTK application
    let app = Application::builder()
        .application_id("com.cterm.terminal")
        .build();

    // Connect to the activate signal
    app.connect_activate(|app| {
        app::build_ui(app);
    });

    // Run the application
    app.run()
}
