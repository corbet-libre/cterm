//! Native Win32 drag-out sessions for Kitty OSC 72 drag-and-drop.
//!
//! Session building mirrors what GTK's provider serves (pre-sent bytes for
//! the offered MIME types in offer order, via the shared
//! [`servable_drag_payloads`] plan): each text payload becomes
//! `CF_UNICODETEXT` (+ `CF_TEXT` for ANSI readers) plus a named clipboard
//! format under its own MIME name, PNG bytes a named `image/png` format, and
//! every other MIME type a named format from
//! [`RegisterClipboardFormatW`][windows::Win32::System::DataExchange::RegisterClipboardFormatW].
//! Offered-but-unpresented types are skipped, never served empty; an
//! unservable offer fails closed through `os_drag_failed` so the application
//! keeps its registration.
//!
//! File manifests: where the retained `text/uri-list` names `file:` URLs
//! destined for Explorer, one staged temp file per entry (bounded by
//! [`MAX_SESSION_FILE_ENTRIES`]) is listed in a `CF_HDROP` (`DROPFILES`)
//! manifest. Staged bytes come only from the shared core's retained ingress
//! (`OsDataReady` for an approved `t=k` read); the terminal never opens
//! application-supplied local paths. A file asked for before its bytes arrive
//! is skipped in this drag's manifest while its `t=k` read is emitted for
//! next time — closing the prefetch timing needs on-host work (see below).
//!
//! Operation mapping: negotiated copy/move/either becomes the matching
//! `DROPEFFECT` mask; the terminal only *reports* the destination's pick
//! (`t=e:x=2:o=` on a move finish) and never deletes local data. Lifecycle:
//! `DoDragDrop` returning `DRAGDROP_S_DROP` reports `os_action_changed` /
//! `os_finished`, `DRAGDROP_S_CANCEL` (or any failure) reports
//! `os_finished(canceled=true)` or `os_abort`, and `CancelOsDrag` drops staged
//! files — the COM objects live only for the `DoDragDrop` call.
//!
//! Remote-read consent mirrors the rich-clipboard dialog semantics
//! (`rich_clipboard_prompt.rs`): modal **Allow Once** / **Always Allow** /
//! **Deny** with Deny as the focused default, exact-token (`request_id`)
//! display, a 120 s frontend-local expiry (OSC 72 advertises no deadline,
//! like OSC 5522), and lifecycle cancellation when the drag finishes/aborts
//! or the pane goes away (a late answer finds no live drag and is dropped).
//! Same-machine drags never prompt (core never emits `RequestApproval` for
//! them). The Win32 prompt is synchronous and parented to the main window
//! (like the TTY transfer prompt), unlike Cocoa's async sheet — the body text
//! and choice semantics match.
//!
//! Gesture arbitration mirrors GTK/Kitty: every primary press records a
//! pixel anchor; motion past [`SOURCE_DRAG_THRESHOLD_PX`] (5 px, Kitty's
//! `drag_threshold` default) while armed emits `t=o` once, in addition to
//! ongoing selection/mouse-tracking which is never suppressed.
//!
//! Runtime verification is impossible on this host (no Windows hardware —
//! only the msvc type-check target), so `DoDragDrop` takeover, `CF_HDROP`
//! delivery to Explorer, operation negotiation, staged-file cleanup timing,
//! and dialog interaction still need on-host confirmation; nothing here is
//! faked, only unrun.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use cterm_app::kitty_dnd::{DndLocation, DndOperation};
use cterm_app::kitty_dnd_source::{
    servable_drag_payloads, DndSource, DragPayloadKind, OsDragOffer, PresentedMime,
    RemoteReadApprovalRequest, ServablePayload,
};

/// Pixel distance past which a primary-button motion becomes a drag-source
/// gesture while an application is armed. Mirrors Kitty's `drag_threshold`
/// default (5 px); selection/mouse-tracking is never suppressed.
pub(crate) const SOURCE_DRAG_THRESHOLD_PX: f32 = 5.0;

/// Euclidean pixel distance between a press anchor and the current pointer.
pub(crate) fn source_gesture_distance(anchor: (f32, f32), current: (f32, f32)) -> f32 {
    let dx = current.0 - anchor.0;
    let dy = current.1 - anchor.1;
    dx.hypot(dy)
}

/// Whether a motion event should emit the source-side `t=o` gesture: armed
/// **and** past [`SOURCE_DRAG_THRESHOLD_PX`], exactly once per press (the
/// caller clears the anchor on emit). Compositor-free.
pub(crate) fn should_emit_source_gesture(
    armed: bool,
    anchor: Option<(f32, f32)>,
    current: (f32, f32),
) -> bool {
    if !armed {
        return false;
    }
    let Some(anchor) = anchor else {
        return false;
    };
    if !anchor.0.is_finite()
        || !anchor.1.is_finite()
        || !current.0.is_finite()
        || !current.1.is_finite()
    {
        return false;
    }
    source_gesture_distance(anchor, current) > SOURCE_DRAG_THRESHOLD_PX
}

/// `DndLocation` from client-space coordinates and cell dimensions. Mirrors
/// the GTK `gtk_dnd_location` mapping. Compositor-free.
pub(crate) fn dnd_location_for_point(
    x: f32,
    y: f32,
    cell_width: f32,
    cell_height: f32,
) -> DndLocation {
    fn coordinate(value: f32, cell: f32) -> i32 {
        if value.is_finite() && cell.is_finite() && cell > 0.0 {
            (value / cell).floor().clamp(0.0, i32::MAX as f32) as i32
        } else {
            0
        }
    }
    fn pixel(value: f32) -> i32 {
        if value.is_finite() {
            value.floor().clamp(0.0, i32::MAX as f32) as i32
        } else {
            0
        }
    }
    DndLocation {
        cell_x: coordinate(x, cell_width),
        cell_y: coordinate(y, cell_height),
        pixel_x: pixel(x),
        pixel_y: pixel(y),
    }
}

/// Map a negotiated source operation to the OLE `DoDragDrop` allowed-effects
/// mask. `Either` offers both so the destination picks; the terminal only
/// reports the pick. Compositor-free.
pub(crate) fn drag_effect_mask(operation: DndOperation) -> windows::Win32::System::Ole::DROPEFFECT {
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
    match operation {
        DndOperation::Copy => DROPEFFECT_COPY,
        DndOperation::Move => DROPEFFECT_MOVE,
        DndOperation::Either => DROPEFFECT_COPY | DROPEFFECT_MOVE,
        DndOperation::None => DROPEFFECT_NONE,
    }
}

/// Resolve the destination's final effect into the core calls the window must
/// make: an optional move hint plus whether the drag was canceled.
/// `NONE` (empty) means the drop did not complete; copy needs no action hint
/// (it is the default); anything non-empty without move completed without a
/// hint. Compositor-free.
pub(crate) fn resolve_drag_result(
    effect: windows::Win32::System::Ole::DROPEFFECT,
) -> (Option<DndOperation>, bool) {
    use windows::Win32::System::Ole::{DROPEFFECT_MOVE, DROPEFFECT_NONE};
    if effect == DROPEFFECT_NONE {
        (None, true)
    } else if (effect.0 & DROPEFFECT_MOVE.0) != 0 {
        (Some(DndOperation::Move), false)
    } else {
        (None, false)
    }
}

/// PTY frames for a finished OS drag, in order: the `x=2:o=` move hint when
/// the destination picked move, then the terminal `x=4:y` finish. Pure over
/// `&mut DndSource`; the window writes the frames. Unit-testable without OLE.
pub(crate) fn finish_os_drag_frames(
    source: &mut DndSource,
    effect: windows::Win32::System::Ole::DROPEFFECT,
) -> Vec<Vec<u8>> {
    use cterm_app::kitty_dnd_source::DndSourceAction;
    let (hint, canceled) = resolve_drag_result(effect);
    let mut frames = Vec::new();
    if let Some(action) = hint {
        for frame_action in source.os_action_changed(action) {
            if let DndSourceAction::Write(frame) = frame_action {
                frames.push(frame);
            }
        }
    }
    for frame_action in source.os_finished(canceled) {
        if let DndSourceAction::Write(frame) = frame_action {
            frames.push(frame);
        }
    }
    frames
}

/// Servable check shared with the window: the plan the session is built from.
/// Mirrors GTK's provider gate (presented offer bytes + a usable operation).
pub fn session_plan(offer: &OsDragOffer, presented: &[PresentedMime]) -> Vec<ServablePayload> {
    servable_drag_payloads(offer, presented)
}

/// Encode Rust text as `CF_UNICODETEXT` bytes (UTF-16LE + terminating NUL).
/// Compositor-free.
pub(crate) fn text_to_unicode_bytes(text: &str) -> Vec<u8> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Encode Rust text as `CF_TEXT` bytes (lossy ANSI + terminating NUL).
/// Compositor-free.
pub(crate) fn text_to_ansi_bytes(text: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = text
        .chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect();
    bytes.push(0);
    bytes
}

/// Build the `CF_HDROP` (`DROPFILES`) manifest for staged temp files.
/// Unicode (`fWide`) with a double-NUL-terminated file list.
/// Compositor-free.
pub(crate) fn build_hdrop_bytes(paths: &[PathBuf]) -> Option<Vec<u8>> {
    use windows::Win32::UI::Shell::DROPFILES;
    if paths.is_empty() {
        return None;
    }
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        let text = path.to_str()?;
        units.extend(text.encode_utf16());
        units.push(0);
    }
    units.push(0);
    let header_size = std::mem::size_of::<DROPFILES>() as u32;
    let mut bytes = Vec::with_capacity(header_size as usize + units.len() * 2);
    let header = DROPFILES {
        pFiles: header_size,
        pt: windows::Win32::Foundation::POINT { x: 0, y: 0 },
        fNC: windows::core::BOOL(0),
        fWide: windows::core::BOOL(1),
    };
    let header_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            (&header as *const DROPFILES).cast::<u8>(),
            std::mem::size_of::<DROPFILES>(),
        )
    };
    bytes.extend_from_slice(header_bytes);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Some(bytes)
}

/// Stage retained file bytes to per-drag temp files for the `CF_HDROP`
/// manifest. Returns the staged paths in entry order. Best-effort cleanup
/// happens via [`cleanup_staged_files`]; a drop target may still be reading
/// when `DoDragDrop` returns, so callers keep the files until the drag
/// finishes and treat leftover files as a documented on-host verification
/// item. Runtime-only (touches the filesystem).
pub(crate) fn stage_uri_list_files(files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<PathBuf>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("cterm-dnd-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut paths = Vec::with_capacity(files.len());
    for (index, (name, bytes)) in files.iter().enumerate() {
        let safe = sanitize_file_name(name);
        let path = dir.join(format!("{index:03}-{safe}"));
        std::fs::write(&path, bytes)?;
        paths.push(path);
    }
    Ok(paths)
}

/// Best-effort removal of staged temp files and their parent directory.
pub(crate) fn cleanup_staged_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
    if let Some(dir) = paths.first().and_then(|p| p.parent()) {
        let _ = std::fs::remove_dir(dir);
    }
}

fn sanitize_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(128));
    for c in name.chars().take(128) {
        if c == '/' || c == '\\' || c == ':' || c == '\0' || c.is_control() {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        return "dropped-file".to_string();
    }
    out.trim_start_matches('.').to_string()
}

/// Button ids for the remote-read consent dialog. They mirror the clipboard
/// adapter's numbering so reviewers can compare the two dialogs side by side.
pub(crate) const DND_REMOTE_ALLOW_ONCE: i32 = 5530;
pub(crate) const DND_REMOTE_ALLOW_ALWAYS: i32 = 5531;
pub(crate) const DND_REMOTE_DENY: i32 = 5532;

/// Frontend-local bound for one remote-read consent dialog.
///
/// OSC 72 advertises no expiry, so an unanswered dialog must not linger only
/// to authorize a read the user long forgot about. The drag finishing or
/// aborting cancels the dialog sooner through pane teardown (a late answer
/// finds no live drag and is dropped).
pub const DND_REMOTE_PROMPT_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// Maximum characters of untrusted entry text shown in a dialog.
const MAX_DISPLAY_CHARS: usize = 160;

/// The user's answer to one remote-read consent dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndRemoteChoice {
    /// Read exactly this entry; remember nothing.
    AllowOnce,
    /// Remember surface-scoped remote reads, then read this entry.
    AllowAlways,
    /// Abort the drag fail-closed.
    Deny,
}

/// Outcome of the remote-read consent dialog: anything but the two allow
/// buttons — including close/Escape — denies fail-closed, exactly like the
/// clipboard adapter. Compositor-free.
pub(crate) fn resolve_dnd_remote_choice(choice: i32) -> DndRemoteChoice {
    match choice {
        DND_REMOTE_ALLOW_ONCE => DndRemoteChoice::AllowOnce,
        DND_REMOTE_ALLOW_ALWAYS => DndRemoteChoice::AllowAlways,
        _ => DndRemoteChoice::Deny,
    }
}

/// Title and body for one consent dialog. Pure (no Win32 call), so the exact
/// wording is unit-testable. Shows the exact token (`request_id`), the entry
/// reference, the surface-scoped `Always allow` meaning, the same-machine
/// exemption, and drag-lifecycle expiry; no file bytes are shown (they
/// arrive only after approval).
pub fn format_dnd_remote_prompt(request: &RemoteReadApprovalRequest) -> (String, String) {
    let title = "Allow remote file read?".to_string();
    let body = format!(
        "A terminal application wants to read a remote file entry (entry {}, part {} of {}, request #{}).\r\n\r\n\
        \"Allow once\" reads exactly this entry. \"Always allow\" remembers remote reads for this terminal surface only (same-machine drags never prompt); it clears when the surface closes.\r\n\r\n\
        The approval expires when the drag finishes or is cancelled (or after 120 s); closing this dialog denies the read.",
        sanitize_display(&request.entry.entry.to_string(), MAX_DISPLAY_CHARS),
        sanitize_display(&request.entry.num.to_string(), MAX_DISPLAY_CHARS),
        sanitize_display(&request.entry.parent.to_string(), MAX_DISPLAY_CHARS),
        request.request_id
    );
    (title, body)
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

// ---------------------------------------------------------------------------
// OLE session objects (runtime-only; type-checked on the msvc target)
// ---------------------------------------------------------------------------

/// One clipboard format the drag data object serves.
#[derive(Debug, Clone)]
pub(crate) struct DragClipboardFormat {
    /// `CLIPBOARD_FORMAT` value (`CF_*` or a registered custom format).
    pub cf_format: u32,
    /// Raw bytes served via `TYMED_HGLOBAL`.
    pub data: Vec<u8>,
}

/// Collect the clipboard formats for already-servable payloads plus an
/// optional `CF_HDROP` manifest. Custom MIME formats are registered lazily;
/// registration failure skips that format fail-closed (the standard formats
/// still serve). Runtime-only (calls `RegisterClipboardFormatW`).
pub(crate) fn drag_clipboard_formats(
    payloads: &[ServablePayload],
    hdrop: Option<Vec<u8>>,
) -> Vec<DragClipboardFormat> {
    use windows::Win32::System::Ole::{CF_HDROP, CF_TEXT, CF_UNICODETEXT};
    let mut formats = Vec::new();
    for payload in payloads {
        match payload.kind {
            DragPayloadKind::Text | DragPayloadKind::UriList => {
                let text = String::from_utf8_lossy(&payload.data).into_owned();
                formats.push(DragClipboardFormat {
                    cf_format: CF_UNICODETEXT.0 as u32,
                    data: text_to_unicode_bytes(&text),
                });
                // `CF_TEXT` is best-effort for ANSI-only targets; a duplicate
                // registration simply overwrites the previous entry below.
                upsert_format(
                    &mut formats,
                    DragClipboardFormat {
                        cf_format: CF_TEXT.0 as u32,
                        data: text_to_ansi_bytes(&text),
                    },
                );
                if let Some(custom) = register_mime_format(&payload.mime_type) {
                    upsert_format(
                        &mut formats,
                        DragClipboardFormat {
                            cf_format: custom,
                            data: payload.data.clone(),
                        },
                    );
                }
            }
            DragPayloadKind::Png => {
                if let Some(custom) = register_mime_format("image/png") {
                    upsert_format(
                        &mut formats,
                        DragClipboardFormat {
                            cf_format: custom,
                            data: payload.data.clone(),
                        },
                    );
                }
            }
            DragPayloadKind::Other => {
                if let Some(custom) = register_mime_format(&payload.mime_type) {
                    upsert_format(
                        &mut formats,
                        DragClipboardFormat {
                            cf_format: custom,
                            data: payload.data.clone(),
                        },
                    );
                }
            }
        }
    }
    if let Some(bytes) = hdrop {
        upsert_format(
            &mut formats,
            DragClipboardFormat {
                cf_format: CF_HDROP.0 as u32,
                data: bytes,
            },
        );
    }
    formats
}

fn upsert_format(formats: &mut Vec<DragClipboardFormat>, format: DragClipboardFormat) {
    if let Some(existing) = formats.iter_mut().find(|f| f.cf_format == format.cf_format) {
        *existing = format;
    } else {
        formats.push(format);
    }
}

fn register_mime_format(mime: &str) -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    let wide: Vec<u16> = mime.encode_utf16().chain(std::iter::once(0)).collect();
    let id = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
    (id != 0).then_some(id)
}

thread_local! {
    static PENDING_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
    static DIALOG_RESULT: RefCell<Option<i32>> = const { RefCell::new(None) };
}

const IDC_PROMPT_DETAILS: i32 = 5533;
const EXPIRY_TIMER_ID: usize = 0x5530;

/// Show a deny-by-default modal dialog for one remote read and return the
/// user's choice. Runs its own nested message loop on the calling (UI) thread
/// with `hwnd` as parent; the caller must not hold pane borrows. Close,
/// timeout, and dialog creation failure all return [`DndRemoteChoice::Deny`].
pub(crate) fn show_prompt(
    parent: windows::Win32::Foundation::HWND,
    request: &RemoteReadApprovalRequest,
) -> DndRemoteChoice {
    use winapi::shared::windef::HWND as WinapiHwnd;
    use winapi::um::winuser::DialogBoxIndirectParamW;
    let (title, body) = format_dnd_remote_prompt(request);
    PENDING_MESSAGE.with(|message| {
        *message.borrow_mut() = Some(body);
    });
    DIALOG_RESULT.with(|result| {
        *result.borrow_mut() = None;
    });
    let template = build_dialog_template(&title);
    unsafe {
        DialogBoxIndirectParamW(
            std::ptr::null_mut(),
            template.as_ptr() as *const winapi::um::winuser::DLGTEMPLATE,
            parent.0 as WinapiHwnd,
            Some(dialog_proc),
            0,
        );
    }
    let result = DIALOG_RESULT.with(|result| result.borrow_mut().take());
    PENDING_MESSAGE.with(|message| {
        *message.borrow_mut() = None;
    });
    match result {
        Some(choice) => resolve_dnd_remote_choice(choice),
        None => DndRemoteChoice::Deny,
    }
}

fn build_dialog_template(title: &str) -> Vec<u8> {
    use winapi::um::winuser::{
        DS_CENTER, DS_MODALFRAME, DS_SETFONT, WS_CAPTION, WS_POPUP, WS_SYSMENU,
    };
    let mut template = Vec::new();
    let style = DS_MODALFRAME | DS_CENTER | WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_SETFONT;
    let ex_style = 0u32;
    template.extend_from_slice(&style.to_le_bytes());
    template.extend_from_slice(&ex_style.to_le_bytes());
    template.extend_from_slice(&0u16.to_le_bytes());
    template.extend_from_slice(&0i16.to_le_bytes());
    template.extend_from_slice(&0i16.to_le_bytes());
    template.extend_from_slice(&420i16.to_le_bytes());
    template.extend_from_slice(&200i16.to_le_bytes());
    template.extend_from_slice(&0u16.to_le_bytes());
    template.extend_from_slice(&0u16.to_le_bytes());
    append_wide(&mut template, title);
    while !template.len().is_multiple_of(2) {
        template.push(0);
    }
    template.extend_from_slice(&9u16.to_le_bytes());
    append_wide(&mut template, "Segoe UI");
    template
}

fn append_wide(buffer: &mut Vec<u8>, value: &str) {
    use crate::dialog_utils::to_wide;
    for code_unit in to_wide(value) {
        buffer.extend_from_slice(&code_unit.to_le_bytes());
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: winapi::shared::windef::HWND,
    message: u32,
    wparam: usize,
    _lparam: isize,
) -> isize {
    use winapi::um::winuser::{
        EndDialog, KillTimer, SendMessageW, SetFocus, SetTimer, DM_SETDEFID, EM_SETREADONLY,
        IDCANCEL, WM_CLOSE, WM_COMMAND, WM_INITDIALOG, WM_TIMER,
    };
    match message {
        WM_INITDIALOG => initialize_dialog(hwnd),
        WM_COMMAND => {
            match (wparam & 0xffff) as i32 {
                DND_REMOTE_ALLOW_ONCE | DND_REMOTE_ALLOW_ALWAYS | DND_REMOTE_DENY => {
                    DIALOG_RESULT.with(|result| {
                        *result.borrow_mut() = Some((wparam & 0xffff) as i32);
                    });
                    EndDialog(hwnd, (wparam & 0xffff) as isize);
                }
                x if x == IDCANCEL => {
                    EndDialog(hwnd, DND_REMOTE_DENY as isize);
                }
                _ => return 0,
            };
            1
        }
        WM_TIMER => {
            KillTimer(hwnd, EXPIRY_TIMER_ID);
            EndDialog(hwnd, DND_REMOTE_DENY as isize);
            1
        }
        WM_CLOSE => {
            EndDialog(hwnd, DND_REMOTE_DENY as isize);
            1
        }
        _ => {
            let _ = (
                DM_SETDEFID,
                EM_SETREADONLY,
                SendMessageW,
                SetFocus,
                SetTimer,
            );
            0
        }
    }
}

unsafe fn initialize_dialog(hwnd: winapi::shared::windef::HWND) -> isize {
    use winapi::shared::windef::RECT;
    use winapi::um::winuser::{GetClientRect, EM_SETREADONLY as _EM_RO};
    use winapi::um::winuser::{SendMessageW, SetFocus, SetTimer, DM_SETDEFID, EM_SETREADONLY};
    let _ = _EM_RO;
    let message = PENDING_MESSAGE.with(|message| message.borrow().clone());
    let Some(message) = message else {
        winapi::um::winuser::EndDialog(hwnd, DND_REMOTE_DENY as isize);
        return 0;
    };
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let margin = 16;
    let button_width = 96;
    let button_height = 28;
    let button_gap = 10;
    let button_y = height - margin - button_height;
    crate::dialog_utils::create_label(
        hwnd,
        0,
        "Review this terminal file request",
        margin,
        margin,
        width - margin * 2,
        24,
    );
    let details = crate::dialog_utils::create_multiline_edit(
        hwnd,
        IDC_PROMPT_DETAILS,
        margin,
        margin + 30,
        width - margin * 2,
        button_y - margin * 2 - 30,
    );
    crate::dialog_utils::set_edit_text(details, &message);
    SendMessageW(details, EM_SETREADONLY as u32, 1, 0);
    crate::dialog_utils::create_button(
        hwnd,
        DND_REMOTE_ALLOW_ONCE,
        "Allow Once",
        width - margin - button_width * 3 - button_gap * 2,
        button_y,
        button_width,
        button_height,
    );
    crate::dialog_utils::create_button(
        hwnd,
        DND_REMOTE_ALLOW_ALWAYS,
        "Always Allow",
        width - margin - button_width * 2 - button_gap,
        button_y,
        button_width,
        button_height,
    );
    let deny = crate::dialog_utils::create_default_button(
        hwnd,
        DND_REMOTE_DENY,
        "Deny",
        width - margin - button_width,
        button_y,
        button_width,
        button_height,
    );
    SendMessageW(hwnd, DM_SETDEFID, DND_REMOTE_DENY as usize, 0);
    SetFocus(deny);
    SetTimer(
        hwnd,
        EXPIRY_TIMER_ID,
        DND_REMOTE_PROMPT_TTL.as_millis() as u32,
        None,
    );
    0
}

// ---------------------------------------------------------------------------
// OLE data object + drop source (DoDragDrop session)
// ---------------------------------------------------------------------------

use windows::core::{implement, Ref, Result as WinResult};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HGLOBAL, S_OK,
};
use windows::Win32::System::Com::{
    IDataObject, IDataObject_Impl, IEnumFORMATETC, DVASPECT_CONTENT, FORMATETC, STGMEDIUM,
    TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{IDropSource, IDropSource_Impl, ReleaseStgMedium, DROPEFFECT};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;

/// OLE data object serving the negotiated drag formats via `TYMED_HGLOBAL`.
#[implement(IDataObject)]
struct CtermDragData {
    formats: Vec<DragClipboardFormat>,
}

impl CtermDragData {
    fn new(formats: Vec<DragClipboardFormat>) -> Self {
        Self { formats }
    }

    fn find(&self, format: &FORMATETC) -> Option<&DragClipboardFormat> {
        if format.dwAspect != DVASPECT_CONTENT.0 || format.lindex != -1 {
            return None;
        }
        if format.tymed != TYMED_HGLOBAL.0 as u32 {
            return None;
        }
        let cf = format.cfFormat as u32;
        self.formats.iter().find(|f| f.cf_format == cf)
    }

    fn hglobal_for(data: &[u8]) -> WinResult<STGMEDIUM> {
        use windows::Win32::System::Com::STGMEDIUM_0;
        let len = data.len().max(1);
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, len)? };
        let locked = unsafe { GlobalLock(handle) };
        if locked.is_null() {
            unsafe {
                let _ = windows::Win32::Foundation::GlobalFree(Some(handle));
            }
            return Err(windows::core::Error::from_win32());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), locked as *mut u8, data.len());
            let _ = GlobalUnlock(handle);
        }
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 {
                hGlobal: HGLOBAL(handle.0 as _),
            },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for CtermDragData_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        let format = unsafe { pformatetcin.read() };
        let Some(entry) = self.find(&format) else {
            return Err(windows::core::Error::from(
                windows::Win32::Foundation::DV_E_FORMATETC,
            ));
        };
        CtermDragData::hglobal_for(&entry.data)
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> WinResult<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
        let format = unsafe { pformatetc.read() };
        if self.find(&format).is_some() {
            S_OK
        } else {
            windows::Win32::Foundation::DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> windows::core::HRESULT {
        windows::Win32::Foundation::DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::core::BOOL,
    ) -> WinResult<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        use windows::Win32::System::Com::DATADIR_GET;
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(windows::core::Error::from(
                windows::Win32::Foundation::E_NOTIMPL,
            ));
        }
        // Enumerator is a documented gap: drop targets that probe formats via
        // `EnumFormatEtc` instead of `QueryGetData`/`GetData` will see
        // `E_NOTIMPL` and may refuse the drag. Explorer and most targets use
        // `GetData`, which is fully served; a full `IEnumFORMATETC` needs
        // on-host verification.
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<'_, windows::Win32::System::Com::IAdviseSink>,
    ) -> WinResult<u32> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED,
        ))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_NOCONNECTION,
        ))
    }

    fn EnumDAdvise(&self) -> WinResult<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_NOCONNECTION,
        ))
    }
}

/// OLE drop source driving the `DoDragDrop` modal loop.
#[implement(IDropSource)]
struct CtermDropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for CtermDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON};
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        let buttons = grfkeystate.0;
        let dragging = (buttons & (MK_LBUTTON.0 | MK_RBUTTON.0)) != 0;
        if dragging {
            S_OK
        } else {
            DRAGDROP_S_DROP
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Run the blocking OLE drag loop for already-built formats. Returns the
/// performed effect (`DROPEFFECT_NONE` when the drag was canceled or the loop
/// could not start). Must be called on the STA UI thread without pane
/// borrows held (`DoDragDrop` pumps messages and may re-enter the window).
pub(crate) fn run_drag_loop(formats: Vec<DragClipboardFormat>, allowed: DROPEFFECT) -> DROPEFFECT {
    use windows::Win32::System::Ole::{DoDragDrop, DROPEFFECT_NONE};
    if formats.is_empty() || allowed == DROPEFFECT_NONE {
        return DROPEFFECT_NONE;
    }
    let data: IDataObject = CtermDragData::new(formats).into();
    let source: IDropSource = CtermDropSource.into();
    let mut performed = DROPEFFECT_NONE;
    let hr = unsafe { DoDragDrop(&data, &source, allowed, &mut performed) };
    if hr == DRAGDROP_S_DROP || hr == DRAGDROP_S_CANCEL || hr.is_ok() {
        performed
    } else {
        log::warn!("Kitty DND DoDragDrop failed: {hr:?}");
        DROPEFFECT_NONE
    }
}

/// Release helper for `GetData` callers (mirrors the drop-target guard).
#[allow(dead_code)]
pub(crate) fn release_medium(medium: &mut STGMEDIUM) {
    unsafe { ReleaseStgMedium(medium) };
}

/// Keep the linker from discarding the format registry when only tests run.
#[allow(dead_code)]
pub(crate) fn clipboard_format_count(formats: &[DragClipboardFormat]) -> usize {
    formats.len()
}

#[allow(dead_code)]
pub(crate) fn staged_file_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_app::kitty_dnd::DndOperation as Op;
    use cterm_app::kitty_dnd_source::{PresentedMime, RemoteEntryRef};
    use cterm_core::{DndCommand, DndCommandType};

    fn approval_request() -> RemoteReadApprovalRequest {
        RemoteReadApprovalRequest {
            request_id: 42,
            entry: RemoteEntryRef {
                entry: 3,
                num: 1,
                parent: 0,
            },
            client_id: 7,
        }
    }

    #[test]
    fn gesture_arbitration_is_thresholded_and_armed() {
        let anchor = Some((10.0f32, 10.0f32));
        assert!(should_emit_source_gesture(true, anchor, (16.0, 10.0)));
        assert!(should_emit_source_gesture(true, anchor, (10.0, 16.0)));
        assert!(!should_emit_source_gesture(true, anchor, (12.0, 10.0)));
        assert!(!should_emit_source_gesture(true, anchor, (10.0, 10.0)));
        assert!(!should_emit_source_gesture(false, anchor, (100.0, 100.0)));
        assert!(!should_emit_source_gesture(true, None, (100.0, 100.0)));
        assert!(!should_emit_source_gesture(true, anchor, (f32::NAN, 10.0)));
        assert!(!should_emit_source_gesture(
            true,
            Some((f32::INFINITY, 10.0)),
            (100.0, 100.0)
        ));
        assert!((source_gesture_distance((0.0, 0.0), (3.0, 4.0)) - 5.0).abs() < 1e-6);
        assert_eq!(SOURCE_DRAG_THRESHOLD_PX, 5.0);
    }

    #[test]
    fn effect_mask_maps_copy_move_either() {
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
        assert_eq!(drag_effect_mask(Op::Copy), DROPEFFECT_COPY);
        assert_eq!(drag_effect_mask(Op::Move), DROPEFFECT_MOVE);
        assert_eq!(
            drag_effect_mask(Op::Either),
            DROPEFFECT_COPY | DROPEFFECT_MOVE
        );
        assert_eq!(drag_effect_mask(Op::None), DROPEFFECT_NONE);
    }

    #[test]
    fn drag_result_maps_cancel_move_and_copy() {
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
        assert_eq!(resolve_drag_result(DROPEFFECT_NONE), (None, true));
        assert_eq!(
            resolve_drag_result(DROPEFFECT_MOVE),
            (Some(Op::Move), false)
        );
        assert_eq!(resolve_drag_result(DROPEFFECT_COPY), (None, false));
        assert_eq!(
            resolve_drag_result(DROPEFFECT_COPY | DROPEFFECT_MOVE),
            (Some(Op::Move), false)
        );
    }

    #[test]
    fn finish_frames_report_move_hint_then_terminal_finish() {
        fn command(command_type: DndCommandType) -> DndCommand {
            DndCommand {
                command_type,
                more: false,
                client_id: 0,
                operation: 0,
                cell_x: 0,
                cell_y: 0,
                has_cell_y: false,
                pixel_x: 0,
                pixel_y: 0,
                payload: Vec::new(),
            }
        }

        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE};
        let mut source = DndSource::default();
        let mut enable = command(DndCommandType::OfferDrag);
        enable.cell_x = 1;
        assert!(source.handle_command(enable).is_empty());
        assert!(source
            .user_gesture(DndLocation::default())
            .iter()
            .any(|action| matches!(
                action,
                cterm_app::kitty_dnd_source::DndSourceAction::Write(_)
            )));
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 3;
        offer.payload = b"text/plain".to_vec();
        assert!(source.handle_command(offer).is_empty());
        assert!(!source.os_drag_started().is_empty());
        assert!(source.is_dragging());

        let frames = finish_os_drag_frames(&mut source, DROPEFFECT_MOVE);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].windows(4).any(|part| part == b":o=2"));
        assert!(frames[1].windows(7).any(|part| part == b"x=4:y=0"));

        let mut source = DndSource::default();
        let mut enable = command(DndCommandType::OfferDrag);
        enable.cell_x = 1;
        assert!(source.handle_command(enable).is_empty());
        assert!(!source.user_gesture(DndLocation::default()).is_empty());
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 1;
        offer.payload = b"text/plain".to_vec();
        assert!(source.handle_command(offer).is_empty());
        assert!(!source.os_drag_started().is_empty());
        let frames = finish_os_drag_frames(&mut source, DROPEFFECT_NONE);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].windows(7).any(|part| part == b"x=4:y=1"));

        let mut source = DndSource::default();
        let mut enable = command(DndCommandType::OfferDrag);
        enable.cell_x = 1;
        assert!(source.handle_command(enable).is_empty());
        assert!(!source.user_gesture(DndLocation::default()).is_empty());
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 1;
        offer.payload = b"text/plain".to_vec();
        assert!(source.handle_command(offer).is_empty());
        assert!(!source.os_drag_started().is_empty());
        let frames = finish_os_drag_frames(&mut source, DROPEFFECT_COPY);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].windows(7).any(|part| part == b"x=4:y=0"));
    }

    #[test]
    fn locations_are_bounded_and_cell_relative() {
        assert_eq!(
            dnd_location_for_point(20.5, 35.9, 8.0, 16.0),
            DndLocation {
                cell_x: 2,
                cell_y: 2,
                pixel_x: 20,
                pixel_y: 35,
            }
        );
        assert_eq!(
            dnd_location_for_point(f32::NAN, -5.0, 8.0, 16.0),
            DndLocation::default()
        );
        assert_eq!(
            dnd_location_for_point(20.0, 35.0, 0.0, 16.0),
            DndLocation {
                cell_x: 0,
                cell_y: 2,
                pixel_x: 20,
                pixel_y: 35,
            }
        );
    }

    #[test]
    fn remote_prompt_shows_exact_token_scope_and_lifecycle() {
        let (title, body) = format_dnd_remote_prompt(&approval_request());
        assert_eq!(title, "Allow remote file read?");
        assert!(
            body.contains("42"),
            "body must show the exact token: {body}"
        );
        assert!(body.contains("entry 3"), "body must show the entry: {body}");
        assert!(
            body.contains("Always allow"),
            "body must state the surface-scoped opt-in: {body}"
        );
        assert!(
            body.contains("same-machine") || body.contains("Same-machine"),
            "body must note same-machine drags never prompt: {body}"
        );
        assert!(
            body.contains("expires") || body.contains("Expires"),
            "body must state drag-lifecycle expiry: {body}"
        );
        assert!(
            body.contains("denies") || body.contains("Denies") || body.contains("Deny"),
            "body must state close-means-deny: {body}"
        );
        assert_eq!(
            resolve_dnd_remote_choice(DND_REMOTE_ALLOW_ONCE),
            DndRemoteChoice::AllowOnce
        );
        assert_eq!(
            resolve_dnd_remote_choice(DND_REMOTE_ALLOW_ALWAYS),
            DndRemoteChoice::AllowAlways
        );
        assert_eq!(
            resolve_dnd_remote_choice(DND_REMOTE_DENY),
            DndRemoteChoice::Deny
        );
        assert_eq!(resolve_dnd_remote_choice(99), DndRemoteChoice::Deny);
    }

    #[test]
    fn session_plan_serves_only_presented_offer_mimes() {
        let offer = OsDragOffer {
            mime_types: vec!["text/plain".to_string(), "text/uri-list".to_string()],
            operation: DndOperation::Either,
            location: DndLocation::default(),
            has_image: false,
        };
        let presented = vec![PresentedMime {
            mime_type: "text/plain".to_string(),
            data: b"hi".to_vec(),
        }];
        let plan = session_plan(&offer, &presented);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].mime_type, "text/plain");
        assert!((drag_effect_mask(offer.operation).0 & 1) != 0);
    }

    #[test]
    fn unicode_and_ansi_text_keep_nul_termination() {
        let unicode = text_to_unicode_bytes("hi");
        assert_eq!(unicode, vec![b'h', 0, b'i', 0, 0, 0]);
        let ansi = text_to_ansi_bytes("héllo");
        assert_eq!(ansi.last(), Some(&0));
        assert!(ansi.contains(&b'?'));
    }

    #[test]
    fn hdrop_manifest_is_unicode_double_nul_terminated() {
        let paths = vec![PathBuf::from(r"C:\tmp\a.txt")];
        let bytes = build_hdrop_bytes(&paths).expect("manifest builds");
        assert!(bytes.len() > 20);
        assert_eq!(bytes[bytes.len() - 4..], [0, 0, 0, 0]);
        assert!(build_hdrop_bytes(&[]).is_none());
    }
}
