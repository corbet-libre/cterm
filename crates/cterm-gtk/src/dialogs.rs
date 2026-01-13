//! Dialog windows for menu actions

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, ComboBoxText, Dialog, Entry, Grid, Label,
    Orientation, ResponseType, ScrolledWindow, SpinButton, Switch, Window,
};

use cterm_app::config::{Config, CursorStyleConfig, NewTabPosition, TabBarPosition, TabBarVisibility};

/// Show the "Set Title" dialog
pub fn show_set_title_dialog<F>(parent: &impl IsA<Window>, current_title: &str, callback: F)
where
    F: Fn(String) + 'static,
{
    let dialog = Dialog::builder()
        .title("Set Tab Title")
        .transient_for(parent)
        .modal(true)
        .build();

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("OK", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let label = Label::new(Some("Tab title:"));
    label.set_halign(Align::Start);
    content.append(&label);

    let entry = Entry::new();
    entry.set_text(current_title);
    entry.set_hexpand(true);
    content.append(&entry);

    let entry_clone = entry.clone();
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Ok {
            let title = entry_clone.text().to_string();
            callback(title);
        }
        dialog.close();
    });

    dialog.present();
}

/// Show the "Set Color" dialog
pub fn show_set_color_dialog<F>(parent: &impl IsA<Window>, callback: F)
where
    F: Fn(Option<String>) + 'static,
{
    let dialog = Dialog::builder()
        .title("Set Tab Color")
        .transient_for(parent)
        .modal(true)
        .build();

    dialog.add_button("Clear", ResponseType::Reject);
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("OK", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let label = Label::new(Some("Select a color for this tab:"));
    label.set_halign(Align::Start);
    content.append(&label);

    // Preset color buttons
    let colors_box = GtkBox::new(Orientation::Horizontal, 8);
    let colors = [
        ("#e74c3c", "Red"),
        ("#e67e22", "Orange"),
        ("#f1c40f", "Yellow"),
        ("#2ecc71", "Green"),
        ("#3498db", "Blue"),
        ("#9b59b6", "Purple"),
        ("#1abc9c", "Teal"),
        ("#95a5a6", "Gray"),
    ];

    let selected_color: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    for (color, name) in colors {
        let btn = Button::new();
        btn.set_tooltip_text(Some(name));
        btn.set_size_request(32, 32);
        // Apply color via CSS
        let css = format!(
            "button {{ background: {}; min-width: 32px; min-height: 32px; }}",
            color
        );
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(&css);
        btn.style_context()
            .add_provider(&provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);

        let color_str = color.to_string();
        let selected = Rc::clone(&selected_color);
        btn.connect_clicked(move |_| {
            *selected.borrow_mut() = Some(color_str.clone());
        });

        colors_box.append(&btn);
    }
    content.append(&colors_box);

    let selected_for_response = Rc::clone(&selected_color);
    dialog.connect_response(move |dialog, response| {
        match response {
            ResponseType::Ok => {
                let color = selected_for_response.borrow().clone();
                callback(color);
            }
            ResponseType::Reject => {
                callback(None);
            }
            _ => {}
        }
        dialog.close();
    });

    dialog.present();
}

/// Show the "Find" dialog
pub fn show_find_dialog<F>(parent: &impl IsA<Window>, callback: F)
where
    F: Fn(String, bool, bool) + 'static,
{
    let dialog = Dialog::builder()
        .title("Find in Terminal")
        .transient_for(parent)
        .modal(true)
        .build();

    dialog.add_button("Close", ResponseType::Close);
    dialog.add_button("Find Next", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let grid = Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);

    let search_label = Label::new(Some("Search:"));
    search_label.set_halign(Align::End);
    grid.attach(&search_label, 0, 0, 1, 1);

    let search_entry = Entry::new();
    search_entry.set_hexpand(true);
    grid.attach(&search_entry, 1, 0, 2, 1);

    let case_check = gtk4::CheckButton::with_label("Case sensitive");
    grid.attach(&case_check, 1, 1, 1, 1);

    let regex_check = gtk4::CheckButton::with_label("Regular expression");
    grid.attach(&regex_check, 2, 1, 1, 1);

    content.append(&grid);

    let entry_clone = search_entry.clone();
    let case_clone = case_check.clone();
    let regex_clone = regex_check.clone();

    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Ok {
            let text = entry_clone.text().to_string();
            let case_sensitive = case_clone.is_active();
            let regex = regex_clone.is_active();
            callback(text, case_sensitive, regex);
        } else {
            dialog.close();
        }
    });

    dialog.present();
}

/// Show the About dialog
pub fn show_about_dialog(parent: &impl IsA<Window>) {
    let about = gtk4::AboutDialog::builder()
        .transient_for(parent)
        .modal(true)
        .program_name("cterm")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A modern terminal emulator built with Rust and GTK4")
        .website("https://github.com/KarpelesLab/cterm")
        .website_label("GitHub Repository")
        .license_type(gtk4::License::MitX11)
        .authors(vec!["KarpelesLab"])
        .build();

    about.present();
}

/// Show the Preferences dialog
pub fn show_preferences_dialog(parent: &impl IsA<Window>, config: &Config, on_save: impl Fn(Config) + 'static) {
    let dialog = Dialog::builder()
        .title("Preferences")
        .transient_for(parent)
        .modal(true)
        .default_width(500)
        .default_height(400)
        .build();

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Apply", ResponseType::Apply);
    dialog.add_button("OK", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(0);

    // Create notebook for preference categories
    let notebook = gtk4::Notebook::new();
    notebook.set_vexpand(true);
    content.append(&notebook);

    // General tab
    let general_page = create_general_preferences(config);
    notebook.append_page(&general_page, Some(&Label::new(Some("General"))));

    // Appearance tab
    let appearance_page = create_appearance_preferences(config);
    notebook.append_page(&appearance_page, Some(&Label::new(Some("Appearance"))));

    // Tabs tab
    let tabs_page = create_tabs_preferences(config);
    notebook.append_page(&tabs_page, Some(&Label::new(Some("Tabs"))));

    // Shortcuts tab
    let shortcuts_page = create_shortcuts_preferences(config);
    notebook.append_page(&shortcuts_page, Some(&Label::new(Some("Shortcuts"))));

    let config = config.clone();
    dialog.connect_response(move |dialog, response| {
        match response {
            ResponseType::Ok | ResponseType::Apply => {
                // Collect settings and save
                // For now, just close - full implementation would read all widgets
                let new_config = config.clone();
                on_save(new_config);
                if response == ResponseType::Ok {
                    dialog.close();
                }
            }
            _ => {
                dialog.close();
            }
        }
    });

    dialog.present();
}

fn create_general_preferences(config: &Config) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);

    let grid = Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);

    // Scrollback lines
    let scrollback_label = Label::new(Some("Scrollback lines:"));
    scrollback_label.set_halign(Align::End);
    grid.attach(&scrollback_label, 0, 0, 1, 1);

    let scrollback_spin = SpinButton::with_range(0.0, 100000.0, 1000.0);
    scrollback_spin.set_value(config.general.scrollback_lines as f64);
    grid.attach(&scrollback_spin, 1, 0, 1, 1);

    // Confirm close
    let confirm_label = Label::new(Some("Confirm close with running processes:"));
    confirm_label.set_halign(Align::End);
    grid.attach(&confirm_label, 0, 1, 1, 1);

    let confirm_switch = Switch::new();
    confirm_switch.set_active(config.general.confirm_close_with_running);
    confirm_switch.set_halign(Align::Start);
    grid.attach(&confirm_switch, 1, 1, 1, 1);

    // Copy on select
    let copy_select_label = Label::new(Some("Copy on select:"));
    copy_select_label.set_halign(Align::End);
    grid.attach(&copy_select_label, 0, 2, 1, 1);

    let copy_select_switch = Switch::new();
    copy_select_switch.set_active(config.general.copy_on_select);
    copy_select_switch.set_halign(Align::Start);
    grid.attach(&copy_select_switch, 1, 2, 1, 1);

    page.append(&grid);
    page
}

fn create_appearance_preferences(config: &Config) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);

    let grid = Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);

    // Theme
    let theme_label = Label::new(Some("Theme:"));
    theme_label.set_halign(Align::End);
    grid.attach(&theme_label, 0, 0, 1, 1);

    let theme_combo = ComboBoxText::new();
    theme_combo.append(Some("dark"), "Default Dark");
    theme_combo.append(Some("light"), "Default Light");
    theme_combo.append(Some("tokyo_night"), "Tokyo Night");
    theme_combo.append(Some("dracula"), "Dracula");
    theme_combo.append(Some("nord"), "Nord");
    theme_combo.set_active_id(Some(&config.appearance.theme));
    grid.attach(&theme_combo, 1, 0, 1, 1);

    // Font family
    let font_label = Label::new(Some("Font:"));
    font_label.set_halign(Align::End);
    grid.attach(&font_label, 0, 1, 1, 1);

    let font_entry = Entry::new();
    font_entry.set_text(&config.appearance.font.family);
    font_entry.set_hexpand(true);
    grid.attach(&font_entry, 1, 1, 1, 1);

    // Font size
    let size_label = Label::new(Some("Font size:"));
    size_label.set_halign(Align::End);
    grid.attach(&size_label, 0, 2, 1, 1);

    let size_spin = SpinButton::with_range(6.0, 72.0, 1.0);
    size_spin.set_value(config.appearance.font.size);
    grid.attach(&size_spin, 1, 2, 1, 1);

    // Cursor style
    let cursor_label = Label::new(Some("Cursor style:"));
    cursor_label.set_halign(Align::End);
    grid.attach(&cursor_label, 0, 3, 1, 1);

    let cursor_combo = ComboBoxText::new();
    cursor_combo.append(Some("block"), "Block");
    cursor_combo.append(Some("underline"), "Underline");
    cursor_combo.append(Some("bar"), "Bar");
    let cursor_id = match config.appearance.cursor_style {
        CursorStyleConfig::Block => "block",
        CursorStyleConfig::Underline => "underline",
        CursorStyleConfig::Bar => "bar",
    };
    cursor_combo.set_active_id(Some(cursor_id));
    grid.attach(&cursor_combo, 1, 3, 1, 1);

    // Cursor blink
    let blink_label = Label::new(Some("Cursor blink:"));
    blink_label.set_halign(Align::End);
    grid.attach(&blink_label, 0, 4, 1, 1);

    let blink_switch = Switch::new();
    blink_switch.set_active(config.appearance.cursor_blink);
    blink_switch.set_halign(Align::Start);
    grid.attach(&blink_switch, 1, 4, 1, 1);

    // Opacity
    let opacity_label = Label::new(Some("Opacity:"));
    opacity_label.set_halign(Align::End);
    grid.attach(&opacity_label, 0, 5, 1, 1);

    let opacity_scale = gtk4::Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.1);
    opacity_scale.set_value(config.appearance.opacity);
    opacity_scale.set_hexpand(true);
    grid.attach(&opacity_scale, 1, 5, 1, 1);

    // Bold is bright
    let bold_label = Label::new(Some("Bold text uses bright colors:"));
    bold_label.set_halign(Align::End);
    grid.attach(&bold_label, 0, 6, 1, 1);

    let bold_switch = Switch::new();
    bold_switch.set_active(config.appearance.bold_is_bright);
    bold_switch.set_halign(Align::Start);
    grid.attach(&bold_switch, 1, 6, 1, 1);

    page.append(&grid);
    page
}

fn create_tabs_preferences(config: &Config) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);

    let grid = Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);

    // Show tab bar
    let show_label = Label::new(Some("Show tab bar:"));
    show_label.set_halign(Align::End);
    grid.attach(&show_label, 0, 0, 1, 1);

    let show_combo = ComboBoxText::new();
    show_combo.append(Some("always"), "Always");
    show_combo.append(Some("multiple"), "When multiple tabs");
    show_combo.append(Some("never"), "Never");
    let show_id = match config.tabs.show_tab_bar {
        TabBarVisibility::Always => "always",
        TabBarVisibility::Multiple => "multiple",
        TabBarVisibility::Never => "never",
    };
    show_combo.set_active_id(Some(show_id));
    grid.attach(&show_combo, 1, 0, 1, 1);

    // Tab bar position
    let position_label = Label::new(Some("Tab bar position:"));
    position_label.set_halign(Align::End);
    grid.attach(&position_label, 0, 1, 1, 1);

    let position_combo = ComboBoxText::new();
    position_combo.append(Some("top"), "Top");
    position_combo.append(Some("bottom"), "Bottom");
    let position_id = match config.tabs.tab_bar_position {
        TabBarPosition::Top => "top",
        TabBarPosition::Bottom => "bottom",
    };
    position_combo.set_active_id(Some(position_id));
    grid.attach(&position_combo, 1, 1, 1, 1);

    // New tab position
    let new_label = Label::new(Some("New tab position:"));
    new_label.set_halign(Align::End);
    grid.attach(&new_label, 0, 2, 1, 1);

    let new_combo = ComboBoxText::new();
    new_combo.append(Some("end"), "At end");
    new_combo.append(Some("after_current"), "After current");
    let new_id = match config.tabs.new_tab_position {
        NewTabPosition::End => "end",
        NewTabPosition::AfterCurrent => "after_current",
    };
    new_combo.set_active_id(Some(new_id));
    grid.attach(&new_combo, 1, 2, 1, 1);

    // Show close button
    let close_label = Label::new(Some("Show close button:"));
    close_label.set_halign(Align::End);
    grid.attach(&close_label, 0, 3, 1, 1);

    let close_switch = Switch::new();
    close_switch.set_active(config.tabs.show_close_button);
    close_switch.set_halign(Align::Start);
    grid.attach(&close_switch, 1, 3, 1, 1);

    page.append(&grid);
    page
}

fn create_shortcuts_preferences(config: &Config) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 12);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    page.set_margin_start(12);
    page.set_margin_end(12);

    let label = Label::new(Some("Keyboard Shortcuts"));
    label.set_halign(Align::Start);
    label.add_css_class("heading");
    page.append(&label);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);

    let grid = Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);

    let shortcuts = [
        ("New Tab", &config.shortcuts.new_tab),
        ("Close Tab", &config.shortcuts.close_tab),
        ("Next Tab", &config.shortcuts.next_tab),
        ("Previous Tab", &config.shortcuts.prev_tab),
        ("New Window", &config.shortcuts.new_window),
        ("Close Window", &config.shortcuts.close_window),
        ("Copy", &config.shortcuts.copy),
        ("Paste", &config.shortcuts.paste),
        ("Select All", &config.shortcuts.select_all),
        ("Zoom In", &config.shortcuts.zoom_in),
        ("Zoom Out", &config.shortcuts.zoom_out),
        ("Zoom Reset", &config.shortcuts.zoom_reset),
        ("Find", &config.shortcuts.find),
        ("Reset", &config.shortcuts.reset),
    ];

    for (i, (name, shortcut)) in shortcuts.iter().enumerate() {
        let name_label = Label::new(Some(name));
        name_label.set_halign(Align::End);
        grid.attach(&name_label, 0, i as i32, 1, 1);

        let shortcut_entry = Entry::new();
        shortcut_entry.set_text(shortcut);
        shortcut_entry.set_hexpand(true);
        grid.attach(&shortcut_entry, 1, i as i32, 1, 1);
    }

    scroll.set_child(Some(&grid));
    page.append(&scroll);
    page
}
