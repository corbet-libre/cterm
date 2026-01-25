//! Docker container/image picker dialog

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Dialog, Label, ListBox, ListBoxRow, Notebook, Orientation,
    ResponseType, ScrolledWindow, Window,
};

use cterm_app::docker::{self, ContainerInfo, DockerSelection, ImageInfo};

/// Show the Docker picker dialog
///
/// This dialog shows two tabs:
/// 1. Running Containers - for connecting with `docker exec`
/// 2. Images - for starting new containers with `docker run`
pub fn show_docker_picker<F>(parent: &impl IsA<Window>, callback: F)
where
    F: Fn(DockerSelection) + 'static,
{
    // Check if Docker is available first
    if let Err(e) = docker::check_docker_available() {
        show_docker_error_dialog(parent, &e.to_string());
        return;
    }

    let dialog = Dialog::builder()
        .title("Docker Terminal")
        .transient_for(parent)
        .modal(true)
        .default_width(550)
        .default_height(400)
        .build();

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Connect", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    // Create notebook with two tabs
    let notebook = Notebook::new();
    notebook.set_vexpand(true);
    content.append(&notebook);

    // Tab 1: Running Containers (exec)
    let containers_data: Rc<RefCell<Vec<ContainerInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let (containers_page, containers_list) = create_containers_page();
    notebook.append_page(
        &containers_page,
        Some(&Label::new(Some("Running Containers"))),
    );

    // Tab 2: Images (run)
    let images_data: Rc<RefCell<Vec<ImageInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let (images_page, images_list) = create_images_page();
    notebook.append_page(&images_page, Some(&Label::new(Some("Images"))));

    // Refresh button
    let refresh_btn = Button::with_label("Refresh");
    refresh_btn.set_halign(Align::End);

    let containers_list_ref = containers_list.clone();
    let images_list_ref = images_list.clone();
    let containers_data_ref = Rc::clone(&containers_data);
    let images_data_ref = Rc::clone(&images_data);
    refresh_btn.connect_clicked(move |_| {
        refresh_containers(&containers_list_ref, &containers_data_ref);
        refresh_images(&images_list_ref, &images_data_ref);
    });
    content.append(&refresh_btn);

    // Initial load
    refresh_containers(&containers_list, &containers_data);
    refresh_images(&images_list, &images_data);

    // Handle response
    let callback = Rc::new(callback);
    let callback_clone = Rc::clone(&callback);
    let notebook_ref = notebook.clone();
    let containers_list_ref = containers_list.clone();
    let images_list_ref = images_list.clone();
    let containers_data_ref = Rc::clone(&containers_data);
    let images_data_ref = Rc::clone(&images_data);

    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Ok {
            let current_page = notebook_ref.current_page().unwrap_or(0);

            let selection = if current_page == 0 {
                // Containers tab
                if let Some(row) = containers_list_ref.selected_row() {
                    let idx = row.index() as usize;
                    let data = containers_data_ref.borrow();
                    data.get(idx)
                        .map(|c| DockerSelection::ExecContainer(c.clone()))
                } else {
                    None
                }
            } else {
                // Images tab
                if let Some(row) = images_list_ref.selected_row() {
                    let idx = row.index() as usize;
                    let data = images_data_ref.borrow();
                    data.get(idx).map(|i| DockerSelection::RunImage(i.clone()))
                } else {
                    None
                }
            };

            if let Some(sel) = selection {
                callback_clone(sel);
            }
        }
        dialog.close();
    });

    dialog.present();
}

fn create_containers_page() -> (GtkBox, ListBox) {
    let page = GtkBox::new(Orientation::Vertical, 8);
    page.set_margin_top(8);
    page.set_margin_bottom(8);
    page.set_margin_start(8);
    page.set_margin_end(8);

    let info_label = Label::new(Some(
        "Select a running container to connect with docker exec:",
    ));
    info_label.set_halign(Align::Start);
    info_label.add_css_class("dim-label");
    page.append(&info_label);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);

    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::Single);
    list.add_css_class("boxed-list");
    scroll.set_child(Some(&list));

    page.append(&scroll);

    (page, list)
}

fn create_images_page() -> (GtkBox, ListBox) {
    let page = GtkBox::new(Orientation::Vertical, 8);
    page.set_margin_top(8);
    page.set_margin_bottom(8);
    page.set_margin_start(8);
    page.set_margin_end(8);

    let info_label = Label::new(Some("Select an image to run a new container:"));
    info_label.set_halign(Align::Start);
    info_label.add_css_class("dim-label");
    page.append(&info_label);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);

    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::Single);
    list.add_css_class("boxed-list");
    scroll.set_child(Some(&list));

    page.append(&scroll);

    (page, list)
}

fn refresh_containers(list: &ListBox, data: &Rc<RefCell<Vec<ContainerInfo>>>) {
    // Clear existing rows
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }

    match docker::list_containers() {
        Ok(containers) => {
            if containers.is_empty() {
                let row = create_empty_row("No running containers found");
                list.append(&row);
            } else {
                for container in &containers {
                    let row = create_container_row(container);
                    list.append(&row);
                }
            }
            *data.borrow_mut() = containers;
        }
        Err(e) => {
            let row = create_error_row(&e.to_string());
            list.append(&row);
            data.borrow_mut().clear();
        }
    }
}

fn refresh_images(list: &ListBox, data: &Rc<RefCell<Vec<ImageInfo>>>) {
    // Clear existing rows
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }

    match docker::list_images() {
        Ok(images) => {
            if images.is_empty() {
                let row = create_empty_row("No images found");
                list.append(&row);
            } else {
                for image in &images {
                    let row = create_image_row(image);
                    list.append(&row);
                }
            }
            *data.borrow_mut() = images;
        }
        Err(e) => {
            let row = create_error_row(&e.to_string());
            list.append(&row);
            data.borrow_mut().clear();
        }
    }
}

fn create_container_row(container: &ContainerInfo) -> ListBoxRow {
    let hbox = GtkBox::new(Orientation::Horizontal, 12);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    // Container name (primary)
    let name_label = Label::new(Some(&container.name));
    name_label.set_hexpand(true);
    name_label.set_halign(Align::Start);
    name_label.add_css_class("heading");

    // Image name (secondary)
    let image_label = Label::new(Some(&container.image));
    image_label.add_css_class("dim-label");

    // Status
    let status_label = Label::new(Some(&container.status));
    status_label.add_css_class("dim-label");

    hbox.append(&name_label);
    hbox.append(&image_label);
    hbox.append(&status_label);

    let row = ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

fn create_image_row(image: &ImageInfo) -> ListBoxRow {
    let hbox = GtkBox::new(Orientation::Horizontal, 12);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    // Repository:tag (primary)
    let repo_tag = format!("{}:{}", image.repository, image.tag);
    let name_label = Label::new(Some(&repo_tag));
    name_label.set_hexpand(true);
    name_label.set_halign(Align::Start);
    name_label.add_css_class("heading");

    // Size
    let size_label = Label::new(Some(&image.size));
    size_label.add_css_class("dim-label");

    hbox.append(&name_label);
    hbox.append(&size_label);

    let row = ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

fn create_empty_row(message: &str) -> ListBoxRow {
    let label = Label::new(Some(message));
    label.set_margin_top(16);
    label.set_margin_bottom(16);
    label.add_css_class("dim-label");

    let row = ListBoxRow::new();
    row.set_child(Some(&label));
    row.set_selectable(false);
    row
}

fn create_error_row(message: &str) -> ListBoxRow {
    let label = Label::new(Some(&format!("Error: {}", message)));
    label.set_margin_top(16);
    label.set_margin_bottom(16);
    label.add_css_class("error");

    let row = ListBoxRow::new();
    row.set_child(Some(&label));
    row.set_selectable(false);
    row
}

fn show_docker_error_dialog(parent: &impl IsA<Window>, message: &str) {
    let dialog = Dialog::builder()
        .title("Docker Not Available")
        .transient_for(parent)
        .modal(true)
        .build();

    dialog.add_button("OK", ResponseType::Ok);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let icon = Label::new(Some("⚠️"));
    icon.add_css_class("title-1");
    content.append(&icon);

    let msg_label = Label::new(Some(message));
    msg_label.set_wrap(true);
    msg_label.set_max_width_chars(50);
    content.append(&msg_label);

    let hint_label = Label::new(Some(
        "Please ensure Docker is installed and the Docker daemon is running.",
    ));
    hint_label.add_css_class("dim-label");
    hint_label.set_wrap(true);
    content.append(&hint_label);

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });

    dialog.present();
}
