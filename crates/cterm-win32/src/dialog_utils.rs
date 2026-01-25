//! Utility functions for building complex Win32 dialogs
//!
//! Provides helpers for creating controls, getting/setting values, and managing
//! tab controls and list views.

use std::ptr;

use winapi::shared::minwindef::{LPARAM, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::commctrl::*;
use winapi::um::wingdi::{GetStockObject, DEFAULT_GUI_FONT};
use winapi::um::winuser::*;

/// Convert a Rust string to a null-terminated wide string
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Convert a wide string to a Rust String
pub fn from_wide(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Initialize common controls (call once at startup)
pub fn init_common_controls() {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES | ICC_UPDOWN_CLASS | ICC_STANDARD_CLASSES,
    };
    unsafe {
        InitCommonControlsEx(&icc);
    }
}

/// Set the default GUI font on a control
pub fn set_default_font(hwnd: HWND) {
    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT as i32);
        SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
    }
}

// ============================================================================
// Control Creation Helpers
// ============================================================================

/// Create a static label control
pub fn create_label(parent: HWND, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("STATIC");
    let text = to_wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create an edit control
pub fn create_edit(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("EDIT");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create an edit control with initial text
pub fn create_edit_with_text(
    parent: HWND,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    let hwnd = create_edit(parent, id, x, y, w, h);
    set_edit_text(hwnd, text);
    hwnd
}

/// Create a multiline edit control
pub fn create_multiline_edit(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("EDIT");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a button control
pub fn create_button(parent: HWND, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("BUTTON");
    let text = to_wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a default button control (responds to Enter key)
pub fn create_default_button(
    parent: HWND,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    let class = to_wide("BUTTON");
    let text = to_wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a checkbox control
pub fn create_checkbox(parent: HWND, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("BUTTON");
    let text = to_wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a combobox control (dropdown)
pub fn create_combobox(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("COMBOBOX");
    // Note: h should be the total height including dropdown list
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | CBS_DROPDOWNLIST | WS_VSCROLL,
            x,
            y,
            w,
            h + 200, // Extra height for dropdown
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create an editable combobox control
pub fn create_editable_combobox(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("COMBOBOX");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | CBS_DROPDOWN | WS_VSCROLL,
            x,
            y,
            w,
            h + 200, // Extra height for dropdown
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a spinner (up-down) control paired with an edit control
/// Returns (edit_hwnd, spinner_hwnd)
#[allow(clippy::too_many_arguments)]
pub fn create_spinner(
    parent: HWND,
    edit_id: i32,
    spinner_id: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    min: i32,
    max: i32,
    initial: i32,
) -> (HWND, HWND) {
    // Create edit control first
    let edit = create_edit(parent, edit_id, x, y, w - 20, h);

    // Set the edit control to accept only numbers
    unsafe {
        let style = GetWindowLongW(edit, GWL_STYLE) as u32;
        SetWindowLongW(edit, GWL_STYLE, (style | ES_NUMBER) as i32);
    }

    // Create up-down control
    let class = to_wide(UPDOWN_CLASSW);
    let spinner = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD
                | WS_VISIBLE
                | UDS_SETBUDDYINT
                | UDS_ALIGNRIGHT
                | UDS_ARROWKEYS
                | UDS_NOTHOUSANDS,
            x + w - 20,
            y,
            20,
            h,
            parent,
            spinner_id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };

    // Link spinner to edit control
    unsafe {
        SendMessageW(spinner, UDM_SETBUDDY, edit as WPARAM, 0);
        SendMessageW(spinner, UDM_SETRANGE32, min as WPARAM, max as LPARAM);
        SendMessageW(spinner, UDM_SETPOS32, 0, initial as LPARAM);
    }

    (edit, spinner)
}

/// Create a tab control
pub fn create_tab_control(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide(WC_TABCONTROLW);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a list view control
pub fn create_listview(parent: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide(WC_LISTVIEWW);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };

    // Enable full row select
    unsafe {
        SendMessageW(
            hwnd,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            LVS_EX_FULLROWSELECT as LPARAM,
        );
    }

    hwnd
}

/// Create a group box
pub fn create_groupbox(parent: HWND, id: i32, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let class = to_wide("BUTTON");
    let text = to_wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | BS_GROUPBOX,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    set_default_font(hwnd);
    hwnd
}

/// Create a trackbar (slider) control
#[allow(clippy::too_many_arguments)]
pub fn create_trackbar(
    parent: HWND,
    id: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    min: i32,
    max: i32,
) -> HWND {
    let class = to_wide(TRACKBAR_CLASSW);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | TBS_HORZ | TBS_AUTOTICKS,
            x,
            y,
            w,
            h,
            parent,
            id as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };

    // Set range
    unsafe {
        SendMessageW(
            hwnd,
            TBM_SETRANGE,
            1,
            MAKELPARAM(min as u16, max as u16) as LPARAM,
        );
    }

    hwnd
}

// ============================================================================
// Value Helpers
// ============================================================================

/// Get text from an edit control
pub fn get_edit_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) as usize };
    if len == 0 {
        return String::new();
    }

    let mut buffer = vec![0u16; len + 1];
    unsafe {
        GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
    }
    from_wide(&buffer)
}

/// Set text in an edit control
pub fn set_edit_text(hwnd: HWND, text: &str) {
    let wide = to_wide(text);
    unsafe {
        SetWindowTextW(hwnd, wide.as_ptr());
    }
}

/// Get checkbox state
pub fn get_checkbox_state(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
}

/// Set checkbox state
pub fn set_checkbox_state(hwnd: HWND, checked: bool) {
    let state = if checked { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(hwnd, BM_SETCHECK, state as WPARAM, 0);
    }
}

/// Get combobox selection index (-1 if no selection)
pub fn get_combobox_selection(hwnd: HWND) -> Option<i32> {
    let idx = unsafe { SendMessageW(hwnd, CB_GETCURSEL, 0, 0) } as i32;
    if idx >= 0 {
        Some(idx)
    } else {
        None
    }
}

/// Set combobox selection by index
pub fn set_combobox_selection(hwnd: HWND, index: i32) {
    unsafe {
        SendMessageW(hwnd, CB_SETCURSEL, index as WPARAM, 0);
    }
}

/// Add an item to a combobox
pub fn add_combobox_item(hwnd: HWND, text: &str) {
    let wide = to_wide(text);
    unsafe {
        SendMessageW(hwnd, CB_ADDSTRING, 0, wide.as_ptr() as LPARAM);
    }
}

/// Clear all items from a combobox
pub fn clear_combobox(hwnd: HWND) {
    unsafe {
        SendMessageW(hwnd, CB_RESETCONTENT, 0, 0);
    }
}

/// Get combobox text (for editable comboboxes)
pub fn get_combobox_text(hwnd: HWND) -> String {
    get_edit_text(hwnd)
}

/// Set combobox text (for editable comboboxes)
pub fn set_combobox_text(hwnd: HWND, text: &str) {
    let wide = to_wide(text);
    unsafe {
        SetWindowTextW(hwnd, wide.as_ptr());
    }
}

/// Get spinner (up-down) value
pub fn get_spinner_value(hwnd: HWND) -> i32 {
    unsafe { SendMessageW(hwnd, UDM_GETPOS32, 0, 0) as i32 }
}

/// Set spinner (up-down) value
pub fn set_spinner_value(hwnd: HWND, value: i32) {
    unsafe {
        SendMessageW(hwnd, UDM_SETPOS32, 0, value as LPARAM);
    }
}

/// Get trackbar position
pub fn get_trackbar_value(hwnd: HWND) -> i32 {
    unsafe { SendMessageW(hwnd, TBM_GETPOS, 0, 0) as i32 }
}

/// Set trackbar position
pub fn set_trackbar_value(hwnd: HWND, value: i32) {
    unsafe {
        SendMessageW(hwnd, TBM_SETPOS, 1, value as LPARAM);
    }
}

// ============================================================================
// Tab Control Helpers
// ============================================================================

/// Add a tab to a tab control
pub fn add_tab(tab_ctrl: HWND, index: i32, text: &str) {
    let wide = to_wide(text);
    let mut item = TCITEMW {
        mask: TCIF_TEXT,
        dwState: 0,
        dwStateMask: 0,
        pszText: wide.as_ptr() as *mut _,
        cchTextMax: 0,
        iImage: -1,
        lParam: 0,
    };
    unsafe {
        SendMessageW(
            tab_ctrl,
            TCM_INSERTITEMW,
            index as WPARAM,
            &mut item as *mut _ as LPARAM,
        );
    }
}

/// Get the selected tab index
pub fn get_selected_tab(tab_ctrl: HWND) -> i32 {
    unsafe { SendMessageW(tab_ctrl, TCM_GETCURSEL, 0, 0) as i32 }
}

/// Set the selected tab
pub fn set_selected_tab(tab_ctrl: HWND, index: i32) {
    unsafe {
        SendMessageW(tab_ctrl, TCM_SETCURSEL, index as WPARAM, 0);
    }
}

/// Get the display area rectangle for tab content
pub fn get_tab_display_rect(tab_ctrl: HWND) -> (i32, i32, i32, i32) {
    let mut rect = winapi::shared::windef::RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        GetClientRect(tab_ctrl, &mut rect);
        SendMessageW(tab_ctrl, TCM_ADJUSTRECT, 0, &mut rect as *mut _ as LPARAM);
    }
    (
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    )
}

// ============================================================================
// ListView Helpers
// ============================================================================

/// Add a column to a list view
pub fn add_listview_column(hwnd: HWND, index: i32, text: &str, width: i32) {
    let wide = to_wide(text);
    let mut col = LVCOLUMNW {
        mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
        fmt: LVCFMT_LEFT,
        cx: width,
        pszText: wide.as_ptr() as *mut _,
        cchTextMax: 0,
        iSubItem: 0,
        iImage: 0,
        iOrder: 0,
        cxMin: 0,
        cxDefault: 0,
        cxIdeal: 0,
    };
    unsafe {
        SendMessageW(
            hwnd,
            LVM_INSERTCOLUMNW,
            index as WPARAM,
            &mut col as *mut _ as LPARAM,
        );
    }
}

/// Add an item to a list view (returns the item index)
pub fn add_listview_item(hwnd: HWND, index: i32, text: &str) -> i32 {
    let wide = to_wide(text);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: index,
        iSubItem: 0,
        state: 0,
        stateMask: 0,
        pszText: wide.as_ptr() as *mut _,
        cchTextMax: 0,
        iImage: 0,
        lParam: 0,
        iIndent: 0,
        iGroupId: 0,
        cColumns: 0,
        puColumns: ptr::null_mut(),
        piColFmt: ptr::null_mut(),
        iGroup: 0,
    };
    unsafe { SendMessageW(hwnd, LVM_INSERTITEMW, 0, &mut item as *mut _ as LPARAM) as i32 }
}

/// Set a subitem text in a list view
pub fn set_listview_subitem(hwnd: HWND, item: i32, subitem: i32, text: &str) {
    let wide = to_wide(text);
    let mut lv_item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: item,
        iSubItem: subitem,
        state: 0,
        stateMask: 0,
        pszText: wide.as_ptr() as *mut _,
        cchTextMax: 0,
        iImage: 0,
        lParam: 0,
        iIndent: 0,
        iGroupId: 0,
        cColumns: 0,
        puColumns: ptr::null_mut(),
        piColFmt: ptr::null_mut(),
        iGroup: 0,
    };
    unsafe {
        SendMessageW(hwnd, LVM_SETITEMW, 0, &mut lv_item as *mut _ as LPARAM);
    }
}

/// Get the selected item index in a list view (-1 if none)
pub fn get_listview_selection(hwnd: HWND) -> Option<i32> {
    let idx = unsafe {
        SendMessageW(
            hwnd,
            LVM_GETNEXTITEM,
            -1_isize as WPARAM,
            LVNI_SELECTED as LPARAM,
        )
    } as i32;
    if idx >= 0 {
        Some(idx)
    } else {
        None
    }
}

/// Clear all items from a list view
pub fn clear_listview(hwnd: HWND) {
    unsafe {
        SendMessageW(hwnd, LVM_DELETEALLITEMS, 0, 0);
    }
}

/// Get the number of items in a list view
pub fn get_listview_item_count(hwnd: HWND) -> i32 {
    unsafe { SendMessageW(hwnd, LVM_GETITEMCOUNT, 0, 0) as i32 }
}

/// Select an item in a list view
pub fn select_listview_item(hwnd: HWND, index: i32) {
    let mut item = LVITEMW {
        mask: LVIF_STATE,
        iItem: index,
        iSubItem: 0,
        state: LVIS_SELECTED | LVIS_FOCUSED,
        stateMask: LVIS_SELECTED | LVIS_FOCUSED,
        pszText: ptr::null_mut(),
        cchTextMax: 0,
        iImage: 0,
        lParam: 0,
        iIndent: 0,
        iGroupId: 0,
        cColumns: 0,
        puColumns: ptr::null_mut(),
        piColFmt: ptr::null_mut(),
        iGroup: 0,
    };
    unsafe {
        SendMessageW(
            hwnd,
            LVM_SETITEMSTATE,
            index as WPARAM,
            &mut item as *mut _ as LPARAM,
        );
    }
}

// ============================================================================
// Dialog Helpers
// ============================================================================

/// Get a dialog item by ID
pub fn get_dialog_item(dialog: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(dialog, id) }
}

/// Enable or disable a control
pub fn enable_control(hwnd: HWND, enable: bool) {
    unsafe {
        EnableWindow(hwnd, if enable { 1 } else { 0 });
    }
}

/// Show or hide a control
pub fn show_control(hwnd: HWND, show: bool) {
    unsafe {
        ShowWindow(hwnd, if show { SW_SHOW } else { SW_HIDE });
    }
}

/// Create MAKELPARAM from two values
#[allow(non_snake_case)]
pub fn MAKELPARAM(lo: u16, hi: u16) -> u32 {
    ((hi as u32) << 16) | (lo as u32)
}

/// WC_TABCONTROL class name
pub const WC_TABCONTROLW: &str = "SysTabControl32";

/// WC_LISTVIEW class name
pub const WC_LISTVIEWW: &str = "SysListView32";

/// UPDOWN class name
pub const UPDOWN_CLASSW: &str = "msctls_updown32";

/// TRACKBAR class name
pub const TRACKBAR_CLASSW: &str = "msctls_trackbar32";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_wide() {
        let wide = to_wide("Hello");
        assert_eq!(wide.len(), 6); // "Hello" + null
        assert_eq!(wide[5], 0);
    }

    #[test]
    fn test_from_wide() {
        let wide = to_wide("Hello");
        let s = from_wide(&wide);
        assert_eq!(s, "Hello");
    }

    #[test]
    fn test_makelparam() {
        let lp = MAKELPARAM(0x1234, 0x5678);
        assert_eq!(lp, 0x56781234);
    }
}
