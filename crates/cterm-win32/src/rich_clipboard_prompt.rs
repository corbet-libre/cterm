//! Native Win32 consent for Kitty OSC 5522 rich-clipboard reads.
//!
//! Reads that core policy did not pre-authorize (no exact-token consent, no
//! live paste-event token; see `cterm-core/src/kitty_clipboard.rs`) arrive
//! here instead of failing closed with `EPERM`. This module shows a
//! deny-by-default modal dialog mirroring the GTK adapter's semantics
//! (`terminal_widget.rs`): **Allow Once** answers exactly one read,
//! **Always Allow** (offered only when the request presented a credential)
//! remembers the application's exact `password`+`name` token via
//! `Screen::authorize_rich_clipboard` for future reads, and **Deny** (the
//! focused default) fails the read with `EPERM`. Dialog close, timeout, and
//! any unexpected result all deny.
//!
//! Exact-token display: the dialog shows the requesting application's
//! human-readable name (`credential.name`, sanitized — it arrives in
//! terminal output and is untrusted) plus the requested MIME scope. The
//! password half of the token is never displayed; "Always Allow" stores the
//! exact presented pair, never a prefix.
//!
//! Expiry: OSC 5522 carries no daemon-advertised deadline (unlike OSC 5113's
//! `expires_in_ms`), so the dialog runs a Win32 timer that ends it as a
//! denial after [`RICH_CLIPBOARD_PROMPT_TTL`] (120 s, matching core's
//! paste-token TTL convention). The authoritative seam stays core's bounded
//! pending-read queue: answers for evicted or unknown reads are dropped by
//! `take_pending_rich_read` matching, never misrouted.
//!
//! Threading and lifecycle: the dialog runs its own modal message loop on
//! the calling worker thread (PTY reader or daemon stream task), like the
//! SSH secret dialogs — never on the UI thread and never under the terminal
//! lock. The dialog is parentless and topmost because a reader thread owns
//! no window; that choice, like the three-button layout, still needs
//! on-host confirmation. A pane torn down while its dialog is open cannot
//! preempt the modal; the late answer then finds no live terminal (or no
//! matching pending read) and is dropped, so consent can never outlive its
//! pane. Runtime verification is impossible on this host (no Windows
//! hardware — only the msvc type-check target); nothing here is faked,
//! only unrun.

use std::cell::RefCell;
use std::ptr;
use std::time::Duration;

use winapi::shared::basetsd::INT_PTR;
use winapi::shared::minwindef::{LPARAM, UINT, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::winuser::*;

use crate::dialog_utils::{
    create_button, create_default_button, create_label, create_multiline_edit, set_edit_text,
    to_wide,
};
use cterm_core::RichReadRequest;

/// Frontend-local bound for one clipboard consent dialog (see module docs).
pub const RICH_CLIPBOARD_PROMPT_TTL: Duration = Duration::from_secs(120);

/// Maximum characters of untrusted application/scope text shown in a dialog.
const MAX_DISPLAY_CHARS: usize = 160;
/// Maximum MIME entries listed in the scope line.
const MAX_DISPLAY_MIMES: usize = 8;

const IDC_PROMPT_DETAILS: i32 = 5522;
const IDC_ALLOW_ONCE: i32 = 5523;
const IDC_ALLOW_ALWAYS: i32 = 5524;
const IDC_DENY: i32 = 5525;
const EXPIRY_TIMER_ID: usize = 0x5522;

thread_local! {
    static PENDING_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
    static PENDING_OFFER_ALWAYS: RefCell<bool> = const { RefCell::new(false) };
    static DIALOG_RESULT: RefCell<Option<i32>> = const { RefCell::new(None) };
}

/// The user's answer to one clipboard consent dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichClipboardChoice {
    /// Answer exactly this read; remember nothing.
    AllowOnce,
    /// Remember the exact presented token, then answer this read.
    AllowAlways,
    /// Fail the read with `EPERM`.
    Deny,
}

/// Title and body for one consent dialog. Pure (no Win32 call), so the exact
/// wording is unit-testable. Mirrors the GTK adapter's text: application
/// name plus MIME scope, with list-vs-data phrasing.
pub fn format_rich_clipboard_prompt(request: &RichReadRequest) -> (String, String) {
    let app = request
        .app_name
        .as_deref()
        .map(|name| sanitize_display(name, MAX_DISPLAY_CHARS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "A terminal application".to_string());
    let shown: Vec<String> = request
        .mimes
        .iter()
        .take(MAX_DISPLAY_MIMES)
        .map(|mime| sanitize_display(mime, MAX_DISPLAY_CHARS))
        .collect();
    let mut scope = shown.join(", ");
    if request.mimes.len() > MAX_DISPLAY_MIMES {
        scope.push_str(&format!(
            ", … and {} more",
            request.mimes.len() - MAX_DISPLAY_MIMES
        ));
    }
    let body = if request.list_only {
        format!("{app} wants to list the available clipboard types.")
    } else if scope.is_empty() {
        format!("{app} wants to read the system clipboard.")
    } else {
        format!("{app} wants to read the system clipboard ({scope}).")
    };
    let body = if request.credential.is_some() {
        format!(
            "{body}\r\n\r\n\"Always allow\" remembers this exact application token for future reads."
        )
    } else {
        body
    };
    ("Allow clipboard access?".to_string(), body)
}

/// Show a deny-by-default modal dialog for one read and return the user's
/// choice. Runs its own nested message loop on the calling (worker) thread;
/// the caller must not hold the terminal lock. Close, timeout, and dialog
/// creation failure all return [`RichClipboardChoice::Deny`].
pub fn show_prompt(request: &RichReadRequest) -> RichClipboardChoice {
    let (title, body) = format_rich_clipboard_prompt(request);
    PENDING_MESSAGE.with(|message| {
        *message.borrow_mut() = Some(body);
    });
    PENDING_OFFER_ALWAYS.with(|offer| {
        *offer.borrow_mut() = request.credential.is_some();
    });
    DIALOG_RESULT.with(|result| {
        *result.borrow_mut() = None;
    });

    let template = build_dialog_template(&title);
    unsafe {
        DialogBoxIndirectParamW(
            ptr::null_mut(),
            template.as_ptr() as *const DLGTEMPLATE,
            ptr::null_mut(),
            Some(dialog_proc),
            0,
        );
    }

    let result = DIALOG_RESULT.with(|result| result.borrow_mut().take());
    PENDING_MESSAGE.with(|message| {
        *message.borrow_mut() = None;
    });
    match result {
        Some(choice) if choice == IDC_ALLOW_ONCE => RichClipboardChoice::AllowOnce,
        Some(choice) if choice == IDC_ALLOW_ALWAYS => RichClipboardChoice::AllowAlways,
        _ => RichClipboardChoice::Deny,
    }
}

fn build_dialog_template(title: &str) -> Vec<u8> {
    let mut template = Vec::new();
    let style = DS_MODALFRAME | DS_CENTER | WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_SETFONT;
    // Topmost compensates for the dialog being parentless (it runs on a
    // reader thread that owns no window); see the module docs.
    let ex_style = WS_EX_TOPMOST;
    template.extend_from_slice(&style.to_le_bytes());
    template.extend_from_slice(&ex_style.to_le_bytes());
    template.extend_from_slice(&0u16.to_le_bytes()); // controls are created in WM_INITDIALOG
    template.extend_from_slice(&0i16.to_le_bytes());
    template.extend_from_slice(&0i16.to_le_bytes());
    template.extend_from_slice(&420i16.to_le_bytes());
    template.extend_from_slice(&200i16.to_le_bytes());
    template.extend_from_slice(&0u16.to_le_bytes()); // no menu
    template.extend_from_slice(&0u16.to_le_bytes()); // default dialog class
    append_wide(&mut template, title);
    while !template.len().is_multiple_of(2) {
        template.push(0);
    }
    template.extend_from_slice(&9u16.to_le_bytes());
    append_wide(&mut template, "Segoe UI");
    template
}

fn append_wide(buffer: &mut Vec<u8>, value: &str) {
    for code_unit in to_wide(value) {
        buffer.extend_from_slice(&code_unit.to_le_bytes());
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    message: UINT,
    wparam: WPARAM,
    _lparam: LPARAM,
) -> INT_PTR {
    match message {
        WM_INITDIALOG => initialize_dialog(hwnd),
        WM_COMMAND => {
            match (wparam & 0xffff) as i32 {
                IDC_ALLOW_ONCE | IDC_ALLOW_ALWAYS | IDC_DENY => {
                    DIALOG_RESULT.with(|result| {
                        *result.borrow_mut() = Some((wparam & 0xffff) as i32);
                    });
                    EndDialog(hwnd, (wparam & 0xffff) as isize);
                }
                IDCANCEL => {
                    EndDialog(hwnd, IDC_DENY as isize);
                }
                _ => return 0,
            };
            1
        }
        WM_TIMER => {
            KillTimer(hwnd, EXPIRY_TIMER_ID);
            EndDialog(hwnd, IDC_DENY as isize);
            1
        }
        WM_CLOSE => {
            EndDialog(hwnd, IDC_DENY as isize);
            1
        }
        _ => 0,
    }
}

unsafe fn initialize_dialog(hwnd: HWND) -> INT_PTR {
    let (message, offer_always) = PENDING_MESSAGE.with(|message| {
        (
            message.borrow().clone(),
            PENDING_OFFER_ALWAYS.with(|offer| *offer.borrow()),
        )
    });
    let Some(message) = message else {
        EndDialog(hwnd, IDC_DENY as isize);
        return 0;
    };

    let mut client: winapi::shared::windef::RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let margin = 16;
    let button_width = 96;
    let button_height = 28;
    let button_gap = 10;
    let button_y = height - margin - button_height;

    create_label(
        hwnd,
        0,
        "Review this terminal clipboard request",
        margin,
        margin,
        width - margin * 2,
        24,
    );
    let details = create_multiline_edit(
        hwnd,
        IDC_PROMPT_DETAILS,
        margin,
        margin + 30,
        width - margin * 2,
        button_y - margin * 2 - 30,
    );
    set_edit_text(details, &message);
    SendMessageW(details, EM_SETREADONLY as UINT, 1, 0);

    // Deny is the focused default, so pressing Enter cannot approve the read.
    if offer_always {
        create_button(
            hwnd,
            IDC_ALLOW_ONCE,
            "Allow Once",
            width - margin - button_width * 3 - button_gap * 2,
            button_y,
            button_width,
            button_height,
        );
        create_button(
            hwnd,
            IDC_ALLOW_ALWAYS,
            "Always Allow",
            width - margin - button_width * 2 - button_gap,
            button_y,
            button_width,
            button_height,
        );
    } else {
        create_button(
            hwnd,
            IDC_ALLOW_ONCE,
            "Allow Once",
            width - margin - button_width * 2 - button_gap,
            button_y,
            button_width,
            button_height,
        );
    }
    let deny = create_default_button(
        hwnd,
        IDC_DENY,
        "Deny",
        width - margin - button_width,
        button_y,
        button_width,
        button_height,
    );
    SendMessageW(hwnd, DM_SETDEFID, IDC_DENY as WPARAM, 0);
    SetFocus(deny);
    SetTimer(
        hwnd,
        EXPIRY_TIMER_ID,
        RICH_CLIPBOARD_PROMPT_TTL.as_millis() as u32,
        None,
    );
    0
}

fn sanitize_display(value: &str, max_chars: usize) -> String {
    let mut sanitized = String::new();
    let mut truncated = false;
    for (index, character) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        if character.is_control() || is_unsafe_format_control(character) {
            sanitized.push('�');
        } else {
            sanitized.push(character);
        }
    }
    if truncated {
        sanitized.push('…');
    }
    if sanitized.is_empty() {
        sanitized.push_str("(unnamed)");
    }
    sanitized
}

fn is_unsafe_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200b}'
            | '\u{200c}'
            | '\u{200d}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'
            | '\u{202b}'
            | '\u{202c}'
            | '\u{202d}'
            | '\u{202e}'
            | '\u{2060}'
            | '\u{2061}'
            | '\u{2062}'
            | '\u{2063}'
            | '\u{2064}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
            | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_core::{RichClipboardLocation, RichCredential};

    fn read_request() -> RichReadRequest {
        RichReadRequest {
            id: String::new(),
            location: RichClipboardLocation::Clipboard,
            mimes: vec!["text/plain".to_string()],
            list_only: false,
            credential: Some(RichCredential {
                password: "secret-token".to_string(),
                name: "My App".to_string(),
            }),
            pre_authorized: false,
            app_name: Some("My App".to_string()),
        }
    }

    #[test]
    fn prompt_names_app_scope_and_token_policy() {
        let (title, body) = format_rich_clipboard_prompt(&read_request());
        assert_eq!(title, "Allow clipboard access?");
        assert!(body.contains("My App wants to read the system clipboard (text/plain)."));
        assert!(body.contains("Always allow"));
        assert!(body.contains("exact application token"));
        // The secret half of the token is never displayed.
        assert!(!body.contains("secret-token"));
    }

    #[test]
    fn listing_prompt_uses_list_phrasing() {
        let mut request = read_request();
        request.list_only = true;
        request.mimes.clear();
        let (_, body) = format_rich_clipboard_prompt(&request);
        assert!(body.contains("wants to list the available clipboard types."));
    }

    #[test]
    fn anonymous_app_falls_back_and_hides_always_allow_note() {
        let mut request = read_request();
        request.app_name = None;
        request.credential = None;
        let (_, body) = format_rich_clipboard_prompt(&request);
        assert!(body.contains("A terminal application wants to read"));
        assert!(!body.contains("Always allow"));
    }

    #[test]
    fn control_characters_are_neutralized_and_scope_is_bounded() {
        let mut request = read_request();
        request.app_name = Some("evil\u{1b}]0;spoof\u{7}\u{202e}".to_string());
        request.mimes = (0..12).map(|index| format!("type/x{index}")).collect();
        let (_, body) = format_rich_clipboard_prompt(&request);
        assert!(!body.contains('\u{1b}'));
        assert!(!body.contains('\u{7}'));
        assert!(!body.contains('\u{202e}'));
        assert!(body.contains("and 4 more"));
    }
}
