//! Docker picker dialog for selecting containers or images
//!
//! Provides a dialog to browse running containers and available images,
//! allowing users to connect to a container or start a new one from an image.

use std::cell::RefCell;
use std::ptr;

use winapi::shared::basetsd::INT_PTR;
use winapi::shared::minwindef::{LPARAM, UINT, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::commctrl::*;
use winapi::um::winuser::*;

use crate::dialog_utils::*;
use cterm_app::docker::{self, ContainerInfo, DockerSelection, ImageInfo};

// Control IDs
const IDC_TABS: i32 = 1001;
const IDC_LISTVIEW: i32 = 1002;
const IDC_REFRESH: i32 = 1003;
const IDC_STATUS: i32 = 1004;

// Tab indices
const TAB_CONTAINERS: i32 = 0;
const TAB_IMAGES: i32 = 1;

/// Dialog state
struct DialogState {
    containers: Vec<ContainerInfo>,
    images: Vec<ImageInfo>,
    current_tab: i32,
    docker_available: bool,
    error_message: Option<String>,
}

// Thread-local storage for dialog state
thread_local! {
    static DIALOG_STATE: RefCell<Option<DialogState>> = const { RefCell::new(None) };
    static DIALOG_RESULT: RefCell<Option<DockerSelection>> = const { RefCell::new(None) };
}

/// Show the Docker picker dialog
///
/// Returns the user's selection, or None if cancelled or Docker unavailable.
pub fn show_docker_picker(parent: HWND) -> Option<DockerSelection> {
    // Initialize state
    let mut state = DialogState {
        containers: Vec::new(),
        images: Vec::new(),
        current_tab: TAB_CONTAINERS,
        docker_available: false,
        error_message: None,
    };

    // Check Docker availability and load data
    match docker::check_docker_available() {
        Ok(()) => {
            state.docker_available = true;
            // Load containers and images
            match docker::list_containers() {
                Ok(c) => state.containers = c,
                Err(e) => state.error_message = Some(format!("Failed to list containers: {}", e)),
            }
            match docker::list_images() {
                Ok(i) => state.images = i,
                Err(e) => {
                    if state.error_message.is_none() {
                        state.error_message = Some(format!("Failed to list images: {}", e));
                    }
                }
            }
        }
        Err(e) => {
            state.error_message = Some(e.to_string());
        }
    }

    DIALOG_STATE.with(|s| {
        *s.borrow_mut() = Some(state);
    });
    DIALOG_RESULT.with(|r| {
        *r.borrow_mut() = None;
    });

    // Build and show dialog
    let template = build_dialog_template();
    let ret = unsafe {
        DialogBoxIndirectParamW(
            ptr::null_mut(),
            template.as_ptr() as *const DLGTEMPLATE,
            parent,
            Some(dialog_proc),
            0,
        )
    };

    // Clean up state
    DIALOG_STATE.with(|s| {
        *s.borrow_mut() = None;
    });

    if ret == IDOK as isize {
        DIALOG_RESULT.with(|r| r.borrow().clone())
    } else {
        None
    }
}

/// Build the dialog template
fn build_dialog_template() -> Vec<u8> {
    let mut template = Vec::new();

    // Dialog dimensions
    let width: i16 = 320; // Dialog units (roughly 500 pixels)
    let height: i16 = 240; // Dialog units (roughly 400 pixels)

    // DLGTEMPLATE structure
    let style = DS_MODALFRAME | DS_CENTER | WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_SETFONT;
    let ex_style = 0u32;
    let c_dit = 0u16; // We create controls in WM_INITDIALOG
    let x = 0i16;
    let y = 0i16;

    template.extend_from_slice(&style.to_le_bytes());
    template.extend_from_slice(&ex_style.to_le_bytes());
    template.extend_from_slice(&c_dit.to_le_bytes());
    template.extend_from_slice(&x.to_le_bytes());
    template.extend_from_slice(&y.to_le_bytes());
    template.extend_from_slice(&width.to_le_bytes());
    template.extend_from_slice(&height.to_le_bytes());

    // Menu (none)
    template.extend_from_slice(&[0u8, 0]);
    // Class (use default)
    template.extend_from_slice(&[0u8, 0]);
    // Title
    let title = to_wide("Docker");
    for c in &title {
        template.extend_from_slice(&c.to_le_bytes());
    }

    // Font (for DS_SETFONT)
    align_to_word(&mut template);
    template.extend_from_slice(&9u16.to_le_bytes());
    let font = to_wide("Segoe UI");
    for c in &font {
        template.extend_from_slice(&c.to_le_bytes());
    }

    template
}

fn align_to_word(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(2) {
        v.push(0);
    }
}

/// Dialog procedure
unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> INT_PTR {
    match msg {
        WM_INITDIALOG => {
            init_dialog(hwnd);
            1
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u16;
            handle_command(hwnd, id, code);
            1
        }
        WM_NOTIFY => {
            let nmhdr = lparam as *const NMHDR;
            if !nmhdr.is_null() {
                handle_notify(hwnd, &*nmhdr);
            }
            0
        }
        WM_CLOSE => {
            EndDialog(hwnd, IDCANCEL as isize);
            1
        }
        _ => 0,
    }
}

/// Initialize the dialog
unsafe fn init_dialog(hwnd: HWND) {
    // Get dialog client rect
    let mut rect = std::mem::zeroed();
    GetClientRect(hwnd, &mut rect);
    let dlg_width = rect.right - rect.left;
    let dlg_height = rect.bottom - rect.top;

    // Margins
    let margin = 10;
    let button_height = 25;
    let button_width = 80;
    let tab_height = 28;

    // Create tab control at top
    let tab_ctrl = create_tab_control(
        hwnd,
        IDC_TABS,
        margin,
        margin,
        dlg_width - margin * 2 - button_width - 10,
        tab_height,
    );
    add_tab(tab_ctrl, TAB_CONTAINERS, "Containers");
    add_tab(tab_ctrl, TAB_IMAGES, "Images");

    // Create Refresh button next to tabs
    create_button(
        hwnd,
        IDC_REFRESH,
        "Refresh",
        dlg_width - margin - button_width,
        margin,
        button_width,
        button_height,
    );

    // Create ListView below tabs
    let list_top = margin + tab_height + 5;
    let list_height = dlg_height - list_top - button_height - margin * 2 - 5;
    let listview = create_listview(
        hwnd,
        IDC_LISTVIEW,
        margin,
        list_top,
        dlg_width - margin * 2,
        list_height,
    );

    // Set up columns for containers view
    setup_container_columns(listview, dlg_width - margin * 2 - 4);

    // Create status label (for errors)
    let status_y = list_top + list_height + 5;
    create_label(
        hwnd,
        IDC_STATUS,
        "",
        margin,
        status_y,
        dlg_width - margin * 2,
        20,
    );

    // Create Cancel and Connect buttons at bottom
    let btn_y = dlg_height - button_height - margin;
    create_button(
        hwnd,
        IDCANCEL,
        "Cancel",
        dlg_width - margin - button_width * 2 - 10,
        btn_y,
        button_width,
        button_height,
    );
    create_default_button(
        hwnd,
        IDOK,
        "Connect",
        dlg_width - margin - button_width,
        btn_y,
        button_width,
        button_height,
    );

    // Populate data
    DIALOG_STATE.with(|state| {
        if let Some(ref state) = *state.borrow() {
            if let Some(ref err) = state.error_message {
                let status = get_dialog_item(hwnd, IDC_STATUS);
                set_edit_text(status, err);
            }
            populate_containers(listview, &state.containers);
        }
    });

    // Disable Connect if no items
    update_connect_button(hwnd);
}

/// Set up columns for container view
fn setup_container_columns(listview: HWND, total_width: i32) {
    clear_listview(listview);
    // Remove existing columns first by setting count to 0
    while unsafe { SendMessageW(listview, LVM_DELETECOLUMN, 0, 0) } != 0 {}

    add_listview_column(listview, 0, "Name", (total_width * 40) / 100);
    add_listview_column(listview, 1, "Image", (total_width * 30) / 100);
    add_listview_column(listview, 2, "Status", (total_width * 30) / 100);
}

/// Set up columns for image view
fn setup_image_columns(listview: HWND, total_width: i32) {
    clear_listview(listview);
    // Remove existing columns first
    while unsafe { SendMessageW(listview, LVM_DELETECOLUMN, 0, 0) } != 0 {}

    add_listview_column(listview, 0, "Repository:Tag", (total_width * 70) / 100);
    add_listview_column(listview, 1, "Size", (total_width * 30) / 100);
}

/// Populate the list view with containers
fn populate_containers(listview: HWND, containers: &[ContainerInfo]) {
    clear_listview(listview);
    for (i, container) in containers.iter().enumerate() {
        let idx = add_listview_item(listview, i as i32, &container.name);
        set_listview_subitem(listview, idx, 1, &container.image);
        set_listview_subitem(listview, idx, 2, &container.status);
    }
    if !containers.is_empty() {
        select_listview_item(listview, 0);
    }
}

/// Populate the list view with images
fn populate_images(listview: HWND, images: &[ImageInfo]) {
    clear_listview(listview);
    for (i, image) in images.iter().enumerate() {
        let name = if image.tag == "<none>" {
            image.repository.clone()
        } else {
            format!("{}:{}", image.repository, image.tag)
        };
        let idx = add_listview_item(listview, i as i32, &name);
        set_listview_subitem(listview, idx, 1, &image.size);
    }
    if !images.is_empty() {
        select_listview_item(listview, 0);
    }
}

/// Handle WM_COMMAND
unsafe fn handle_command(hwnd: HWND, id: i32, _code: u16) {
    match id {
        IDOK => {
            // Get selected item and create result
            if try_connect(hwnd) {
                EndDialog(hwnd, IDOK as isize);
            }
        }
        IDCANCEL => {
            EndDialog(hwnd, IDCANCEL as isize);
        }
        IDC_REFRESH => {
            refresh_data(hwnd);
        }
        _ => {}
    }
}

/// Handle WM_NOTIFY
unsafe fn handle_notify(hwnd: HWND, nmhdr: &NMHDR) {
    match nmhdr.code {
        TCN_SELCHANGE if nmhdr.idFrom == IDC_TABS as usize => {
            // Tab changed
            let tab_ctrl = get_dialog_item(hwnd, IDC_TABS);
            let new_tab = get_selected_tab(tab_ctrl);

            DIALOG_STATE.with(|state| {
                if let Some(ref mut state) = *state.borrow_mut() {
                    state.current_tab = new_tab;
                }
            });

            let listview = get_dialog_item(hwnd, IDC_LISTVIEW);
            let mut rect = std::mem::zeroed();
            GetClientRect(hwnd, &mut rect);
            let dlg_width = rect.right - rect.left;
            let list_width = dlg_width - 20 - 4; // margin * 2 - scrollbar

            if new_tab == TAB_CONTAINERS {
                setup_container_columns(listview, list_width);
                DIALOG_STATE.with(|state| {
                    if let Some(ref state) = *state.borrow() {
                        populate_containers(listview, &state.containers);
                    }
                });
            } else {
                setup_image_columns(listview, list_width);
                DIALOG_STATE.with(|state| {
                    if let Some(ref state) = *state.borrow() {
                        populate_images(listview, &state.images);
                    }
                });
            }
            update_connect_button(hwnd);
        }
        NM_DBLCLK if nmhdr.idFrom == IDC_LISTVIEW as usize => {
            // Double-click on list view - treat as Connect
            if try_connect(hwnd) {
                EndDialog(hwnd, IDOK as isize);
            }
        }
        LVN_ITEMCHANGED if nmhdr.idFrom == IDC_LISTVIEW as usize => {
            update_connect_button(hwnd);
        }
        _ => {}
    }
}

/// Refresh Docker data
unsafe fn refresh_data(hwnd: HWND) {
    DIALOG_STATE.with(|state| {
        if let Some(ref mut state) = *state.borrow_mut() {
            state.error_message = None;

            match docker::check_docker_available() {
                Ok(()) => {
                    state.docker_available = true;
                    match docker::list_containers() {
                        Ok(c) => state.containers = c,
                        Err(e) => {
                            state.error_message = Some(format!("Failed to list containers: {}", e))
                        }
                    }
                    match docker::list_images() {
                        Ok(i) => state.images = i,
                        Err(e) => {
                            if state.error_message.is_none() {
                                state.error_message = Some(format!("Failed to list images: {}", e));
                            }
                        }
                    }
                }
                Err(e) => {
                    state.docker_available = false;
                    state.error_message = Some(e.to_string());
                }
            }
        }
    });

    // Update UI
    let status = get_dialog_item(hwnd, IDC_STATUS);
    DIALOG_STATE.with(|state| {
        if let Some(ref state) = *state.borrow() {
            if let Some(ref err) = state.error_message {
                set_edit_text(status, err);
            } else {
                set_edit_text(status, "");
            }
        }
    });

    // Refresh list view
    let listview = get_dialog_item(hwnd, IDC_LISTVIEW);
    let mut rect = std::mem::zeroed();
    GetClientRect(hwnd, &mut rect);
    let dlg_width = rect.right - rect.left;
    let list_width = dlg_width - 20 - 4;

    DIALOG_STATE.with(|state| {
        if let Some(ref state) = *state.borrow() {
            if state.current_tab == TAB_CONTAINERS {
                setup_container_columns(listview, list_width);
                populate_containers(listview, &state.containers);
            } else {
                setup_image_columns(listview, list_width);
                populate_images(listview, &state.images);
            }
        }
    });

    update_connect_button(hwnd);
}

/// Try to connect and set the result
fn try_connect(hwnd: HWND) -> bool {
    let listview = get_dialog_item(hwnd, IDC_LISTVIEW);

    if let Some(idx) = get_listview_selection(listview) {
        DIALOG_STATE.with(|state| {
            if let Some(ref state) = *state.borrow() {
                let selection = if state.current_tab == TAB_CONTAINERS {
                    state
                        .containers
                        .get(idx as usize)
                        .cloned()
                        .map(DockerSelection::ExecContainer)
                } else {
                    state
                        .images
                        .get(idx as usize)
                        .cloned()
                        .map(DockerSelection::RunImage)
                };

                if selection.is_some() {
                    DIALOG_RESULT.with(|r| {
                        *r.borrow_mut() = selection;
                    });
                }
            }
        });

        DIALOG_RESULT.with(|r| r.borrow().is_some())
    } else {
        false
    }
}

/// Update the Connect button state
fn update_connect_button(hwnd: HWND) {
    let listview = get_dialog_item(hwnd, IDC_LISTVIEW);
    let connect_btn = get_dialog_item(hwnd, IDOK);
    let has_selection = get_listview_selection(listview).is_some();
    enable_control(connect_btn, has_selection);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_selection_debug() {
        let container = ContainerInfo {
            id: "abc123".to_string(),
            name: "test-container".to_string(),
            image: "ubuntu:latest".to_string(),
            status: "Up 2 hours".to_string(),
        };
        let selection = DockerSelection::ExecContainer(container);
        let debug_str = format!("{:?}", selection);
        assert!(debug_str.contains("ExecContainer"));
    }
}
