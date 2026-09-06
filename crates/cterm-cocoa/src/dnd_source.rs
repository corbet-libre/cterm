//! Native macOS drag-out sessions for Kitty OSC 72 drag-and-drop.
//!
//! Session building mirrors what GTK's provider serves (pre-sent bytes for
//! the offered MIME types in offer order, via the shared
//! [`servable_drag_payloads`] plan): each text payload becomes an
//! `NSPasteboardItem` string (plus `NSPasteboardTypeString` for
//! `text/plain`), PNG bytes a PNG item, and every other MIME type an item
//! under its own name. Offered-but-unpresented types are skipped, never
//! served empty; an unservable offer fails closed through `os_drag_failed`
//! so the application keeps its registration.
//!
//! File promises: where the retained `text/uri-list` names `file:` URLs
//! destined for Finder, one `NSFilePromiseProvider` per entry (bounded by
//! [`MAX_SESSION_FILE_ENTRIES`]) promises the file. Promise bytes come only
//! from the shared core's retained ingress (`OsDataReady` for an approved
//! `t=k` read); the terminal never opens application-supplied local paths.
//! A promise asked for before its bytes arrive emits the `t=k` read for next
//! time and fails that promise with an `NSError` instead of blocking the
//! drop — closing the prefetch timing needs on-host work (see below).
//!
//! Operation mapping: negotiated copy/move/either becomes the matching
//! `NSDragOperation` mask; the terminal only *reports* the destination's
//! pick (`t=e:x=2:o=` on a move finish) and never deletes local data.
//! Lifecycle: session creation confirms `os_drag_started` (`t=E;OK`,
//! idempotent like GTK's `drag-begin` arm), `endedAtPoint:operation:`
//! reports `os_action_changed`/`os_finished`, and `CancelOsDrag` drops our
//! retained session refs — AppKit owns the live session and ends it via the
//! delegate, whose callbacks fail closed outside `Dragging`.
//!
//! Remote-read consent mirrors the rich-clipboard sheet semantics
//! (`rich_clipboard_prompt.rs`): modal **Allow Once** / **Always Allow** /
//! **Deny** with Deny as the first (Return-key default) button, exact-token
//! (`request_id`) display, a 120 s frontend-local expiry (OSC 72 advertises
//! no deadline, like OSC 5522), and lifecycle cancellation when the drag
//! finishes/aborts or the view goes away. Same-machine drags never prompt
//! (core never emits `RequestApproval` for them).
//!
//! Gesture arbitration mirrors GTK/Kitty: every primary press records a
//! pixel anchor; motion past [`SOURCE_DRAG_THRESHOLD_PX`] (5 px, Kitty's
//! `drag_threshold` default) while armed emits `t=o` once, in addition to
//! ongoing text selection which is never suppressed.
//!
//! Runtime verification is impossible on this host (no macOS hardware — only
//! the Apple type-check targets), so session takeover, promise delivery,
//! operation-mask negotiation with Finder, and sheet interaction still need
//! on-host confirmation; nothing here is faked, only unrun.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSFilePromiseProvider, NSFilePromiseProviderDelegate, NSModalResponseCancel,
    NSPasteboardItem, NSPasteboardType, NSPasteboardWriting, NSView, NSWindow,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSError, NSObject, NSObjectProtocol, NSOperationQueue,
    NSPoint, NSRect, NSSize, NSString, NSUInteger, NSURL,
};
use tokio::sync::{oneshot, watch};

use cterm_app::kitty_dnd::{DndLocation, DndOperation};
use cterm_app::kitty_dnd_source::{
    servable_drag_payloads, DndSource, DragPayloadKind, OsDragOffer, PresentedMime, RemoteEntryRef,
    RemoteReadApprovalRequest, ServablePayload,
};

use crate::terminal_view::TerminalView;

/// Pixel distance past which a primary-button motion becomes a drag-source
/// gesture while an application is armed. Mirrors Kitty's `drag_threshold`
/// default (5 px); selection is never suppressed — the caller keeps
/// extending it and the gesture is emitted *in addition*.
pub(crate) const SOURCE_DRAG_THRESHOLD_PX: f64 = 5.0;

/// Euclidean pixel distance between a press anchor and the current pointer.
pub(crate) fn source_gesture_distance(anchor: (f64, f64), current: (f64, f64)) -> f64 {
    let dx = current.0 - anchor.0;
    let dy = current.1 - anchor.1;
    dx.hypot(dy)
}

/// Whether a motion event should emit the source-side `t=o` gesture: armed
/// **and** past [`SOURCE_DRAG_THRESHOLD_PX`], exactly once per press (the
/// caller clears the anchor on emit). Compositor-free.
pub(crate) fn should_emit_source_gesture(
    armed: bool,
    anchor: Option<(f64, f64)>,
    current: (f64, f64),
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

/// Map a negotiated source operation to the `NSDraggingSession` source mask.
/// `Either` offers both so the destination picks; the terminal only reports
/// the pick. Compositor-free.
pub(crate) fn drag_operation_mask(operation: DndOperation) -> NSDragOperation {
    match operation {
        DndOperation::Copy => NSDragOperation::Copy,
        DndOperation::Move => NSDragOperation::Move,
        DndOperation::Either => NSDragOperation::Copy | NSDragOperation::Move,
        DndOperation::None => NSDragOperation::None,
    }
}

/// Resolve the destination's final operation into the core calls the view
/// must make: an optional move hint plus whether the drag was canceled.
/// `None` (empty mask) means the drop did not complete; copy needs no
/// action hint (it is the default); anything non-empty without move
/// completed without a hint. Compositor-free.
pub(crate) fn resolve_drag_result(operation: NSDragOperation) -> (Option<DndOperation>, bool) {
    if operation.is_empty() {
        (None, true)
    } else if operation.contains(NSDragOperation::Move) {
        (Some(DndOperation::Move), false)
    } else {
        (None, false)
    }
}

/// PTY frames for a finished OS drag, in order: the `x=2:o=` move hint when
/// the destination picked move, then the terminal `x=4:y` finish. Pure over
/// `&mut DndSource`; the view writes the frames. Unit-testable without AppKit.
pub(crate) fn finish_os_drag_frames(
    source: &mut DndSource,
    operation: NSDragOperation,
) -> Vec<Vec<u8>> {
    use cterm_app::kitty_dnd_source::DndSourceAction;
    let (hint, canceled) = resolve_drag_result(operation);
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

/// `DndLocation` from view-space coordinates and cell dimensions. Mirrors the
/// GTK `gtk_dnd_location` mapping. Compositor-free.
pub(crate) fn dnd_location_for_point(
    x: f64,
    y: f64,
    cell_width: f64,
    cell_height: f64,
) -> DndLocation {
    fn coordinate(value: f64, cell: f64) -> i32 {
        if value.is_finite() && cell.is_finite() && cell > 0.0 {
            (value / cell).floor().clamp(0.0, i32::MAX as f64) as i32
        } else {
            0
        }
    }
    fn pixel(value: f64) -> i32 {
        if value.is_finite() {
            value.floor().clamp(0.0, i32::MAX as f64) as i32
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

/// Button ids for the remote-read consent sheet. They mirror the clipboard
/// adapter's numbering so reviewers can compare the two dialogs side by side.
pub(crate) const DND_REMOTE_ALLOW_ONCE: u16 = 0;
pub(crate) const DND_REMOTE_ALLOW_ALWAYS: u16 = 1;
pub(crate) const DND_REMOTE_DENY: u16 = 2;

/// Frontend-local bound for one remote-read consent sheet.
///
/// OSC 72 advertises no expiry, so an unanswered sheet must not linger only
/// to authorize a read the user long forgot about. The drag finishing or
/// aborting cancels the sheet sooner through the lifecycle sender.
pub const DND_REMOTE_PROMPT_TTL: Duration = Duration::from_secs(120);

/// Maximum characters of untrusted entry text shown in a sheet.
const MAX_DISPLAY_CHARS: usize = 160;

/// The user's answer to one remote-read consent sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndRemoteChoice {
    /// Read exactly this entry; remember nothing.
    AllowOnce,
    /// Remember surface-scoped remote reads, then read this entry.
    AllowAlways,
    /// Abort the drag fail-closed.
    Deny,
}

/// How one consent sheet resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptOutcome {
    pub choice: DndRemoteChoice,
    /// The caller must close an outstanding AppKit sheet for timeout,
    /// lifecycle cancellation, or a lost completion callback.
    pub dismiss: bool,
}

/// Outcome of the remote-read consent prompt: anything but the two allow
/// buttons — including close/Escape — denies fail-closed, exactly like the
/// clipboard adapter. Compositor-free.
pub(crate) fn resolve_dnd_remote_choice(choice: u16) -> DndRemoteChoice {
    match choice {
        DND_REMOTE_ALLOW_ONCE => DndRemoteChoice::AllowOnce,
        DND_REMOTE_ALLOW_ALWAYS => DndRemoteChoice::AllowAlways,
        _ => DndRemoteChoice::Deny,
    }
}

/// Title and body for one consent sheet. Pure (no AppKit call), so the exact
/// wording is unit-testable. Shows the exact token (`request_id`), the entry
/// reference, the surface-scoped `Always allow` meaning, the same-machine
/// exemption, and drag-lifecycle expiry; no file bytes are shown (they
/// arrive only after approval).
pub fn format_dnd_remote_prompt(request: &RemoteReadApprovalRequest) -> (String, String) {
    let title = "Allow remote file read?".to_string();
    let body = format!(
        "A terminal application wants to read a remote file entry (entry {}, part {} of {}, request #{}).\n\n\
        \"Allow once\" reads exactly this entry. \"Always allow\" remembers remote reads for this terminal surface only (same-machine drags never prompt); it clears when the surface closes.\n\n\
        The approval expires when the drag finishes or is cancelled (or after 120 s); closing this dialog denies the read.",
        sanitize_display_text(&request.entry.entry.to_string(), MAX_DISPLAY_CHARS),
        sanitize_display_text(&request.entry.num.to_string(), MAX_DISPLAY_CHARS),
        sanitize_display_text(&request.entry.parent.to_string(), MAX_DISPLAY_CHARS),
        request.request_id
    );
    (title, body)
}

static NEXT_PROMPT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct ActivePrompt {
    alert: Retained<NSAlert>,
    parent: Retained<NSWindow>,
    cancelled: Arc<AtomicBool>,
}

thread_local! {
    /// AppKit objects never cross threads. Background cancellation dispatches
    /// a token to the main queue and this main-thread registry owns the sheet.
    static ACTIVE_PROMPTS: RefCell<HashMap<u64, ActivePrompt>> = RefCell::new(HashMap::new());
}

/// Allocate a process-local token used only to address one owned AppKit sheet.
pub fn next_dnd_remote_prompt_token() -> u64 {
    let token = NEXT_PROMPT_TOKEN.fetch_add(1, Ordering::Relaxed);
    if token == 0 {
        NEXT_PROMPT_TOKEN.fetch_add(1, Ordering::Relaxed)
    } else {
        token
    }
}

/// Begin one fail-closed, main-thread-owned consent sheet without blocking
/// the AppKit event loop. Deny is the first (Return-key default) button.
pub fn begin_dnd_remote_prompt(
    mtm: MainThreadMarker,
    token: u64,
    request: &RemoteReadApprovalRequest,
    parent: &NSWindow,
    cancelled: Arc<AtomicBool>,
    result_tx: oneshot::Sender<DndRemoteChoice>,
) {
    if cancelled.load(Ordering::Acquire) {
        let _ = result_tx.send(DndRemoteChoice::Deny);
        return;
    }
    let (title, body) = format_dnd_remote_prompt(request);

    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(&title));
    alert.setInformativeText(&NSString::from_str(&body));

    alert.addButtonWithTitle(&NSString::from_str("Deny"));
    alert.addButtonWithTitle(&NSString::from_str("Allow Once"));
    alert.addButtonWithTitle(&NSString::from_str("Always Allow"));

    let completion_sender = RefCell::new(Some(result_tx));
    let completion_cancelled = Arc::clone(&cancelled);
    let completion_alert = alert.clone();
    let completion = RcBlock::new(move |response: objc2_app_kit::NSModalResponse| {
        // Match AppKit's ownership pattern used by the OSC 5113 and rich
        // clipboard prompts: retain the NSAlert through its completion
        // callback even after the registry releases its cancellation handle.
        let _keep_alert_alive = &completion_alert;
        ACTIVE_PROMPTS.with(|prompts| {
            prompts.borrow_mut().remove(&token);
        });
        let choice = if completion_cancelled.load(Ordering::Acquire)
            || response == objc2_app_kit::NSAlertFirstButtonReturn
        {
            DndRemoteChoice::Deny
        } else if response == objc2_app_kit::NSAlertSecondButtonReturn {
            DndRemoteChoice::AllowOnce
        } else if response == objc2_app_kit::NSAlertThirdButtonReturn {
            DndRemoteChoice::AllowAlways
        } else {
            DndRemoteChoice::Deny
        };
        if let Some(sender) = completion_sender.borrow_mut().take() {
            let _ = sender.send(choice);
        }
    });

    ACTIVE_PROMPTS.with(|prompts| {
        prompts.borrow_mut().insert(
            token,
            ActivePrompt {
                alert: alert.clone(),
                parent: parent.retain(),
                cancelled,
            },
        );
    });
    alert.beginSheetModalForWindow_completionHandler(parent, Some(&completion));
}

/// End one pending sheet as a denial. Must be called on AppKit's main thread.
pub fn cancel_dnd_remote_prompt(_mtm: MainThreadMarker, token: u64) {
    let active = ACTIVE_PROMPTS.with(|prompts| prompts.borrow_mut().remove(&token));
    let Some(active) = active else {
        return;
    };

    active.cancelled.store(true, Ordering::Release);
    let sheet = active.alert.window();
    // `sheets` includes queued sheets, so this also removes a prompt that was
    // waiting behind another document-modal sheet and prevents stale display.
    active
        .parent
        .endSheet_returnCode(&sheet, NSModalResponseCancel);
    sheet.orderOut(None);
}

/// Await a sheet result, the frontend-local expiry, or drag/view lifecycle
/// cancellation. Cancellation and expiry are biased ahead of a simultaneous
/// positive completion so consent can never become valid after its deadline.
pub async fn await_dnd_remote_prompt(
    result_rx: oneshot::Receiver<DndRemoteChoice>,
    mut lifecycle_cancel: watch::Receiver<bool>,
    expires_at: tokio::time::Instant,
) -> PromptOutcome {
    if *lifecycle_cancel.borrow() {
        return PromptOutcome {
            choice: DndRemoteChoice::Deny,
            dismiss: true,
        };
    }

    tokio::select! {
        biased;
        _ = lifecycle_cancel.changed() => PromptOutcome {
            choice: DndRemoteChoice::Deny,
            dismiss: true,
        },
        _ = tokio::time::sleep_until(expires_at) => PromptOutcome {
            choice: DndRemoteChoice::Deny,
            dismiss: true,
        },
        result = result_rx => match result {
            Ok(choice) => PromptOutcome { choice, dismiss: false },
            Err(_) => PromptOutcome {
                choice: DndRemoteChoice::Deny,
                dismiss: true,
            },
        },
    }
}

fn sanitize_display_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut output = String::with_capacity(value.len().min(max_chars));
    for character in chars.by_ref().take(max_chars) {
        if character.is_control() || is_directional_control(character) {
            output.push('�');
        } else {
            output.push(character);
        }
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

fn is_directional_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// One promised file: the `t=k` entry reference the delegate resolves
/// against the view's retained ingress plus the sanitized leaf name derived
/// (via the shared [`uri_list_file_entries`]) from the retained URL text.
struct PromiseFile {
    entry: RemoteEntryRef,
    name: String,
}

pub(crate) struct DndDragSourceIvars {
    view: RefCell<Option<Retained<TerminalView>>>,
    mask: Cell<NSUInteger>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CtermDndDragSource"]
    #[ivars = DndDragSourceIvars]
    pub(crate) struct DndDragSource;

    unsafe impl NSObjectProtocol for DndDragSource {}

    unsafe impl NSDraggingSource for DndDragSource {
        #[allow(non_snake_case)]
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn draggingSession_sourceOperationMaskForDraggingContext(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation(self.ivars().mask.get())
        }

        #[allow(non_snake_case)]
        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn draggingSession_endedAtPoint_operation(
            &self,
            _session: &NSDraggingSession,
            _screen_point: NSPoint,
            operation: NSDragOperation,
        ) {
            // The OS session ended: report finish to the application and
            // release the view pin so the pane can tear down. Callbacks are
            // fail-closed outside `Dragging` (a late event after an abort
            // writes nothing).
            if let Some(view) = self.ivars().view.borrow().as_ref().map(Retained::clone) {
                view.dnd_drag_ended(operation);
            }
            self.ivars().view.borrow_mut().take();
        }
    }
);

impl DndDragSource {
    pub(crate) fn with_view(
        mtm: MainThreadMarker,
        view: &TerminalView,
        mask: NSDragOperation,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(DndDragSourceIvars {
            view: RefCell::new(Some(view.retain())),
            mask: Cell::new(mask.0),
        });
        unsafe { msg_send![super(this), init] }
    }
}

pub(crate) struct DndFilePromiseDelegateIvars {
    view: RefCell<Option<Retained<TerminalView>>>,
    files: RefCell<HashMap<usize, PromiseFile>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CtermDndFilePromiseDelegate"]
    #[ivars = DndFilePromiseDelegateIvars]
    pub(crate) struct DndFilePromiseDelegate;

    unsafe impl NSObjectProtocol for DndFilePromiseDelegate {}

    unsafe impl NSFilePromiseProviderDelegate for DndFilePromiseDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn filePromiseProvider_fileNameForType(
            &self,
            file_promise_provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            // Names are registered at session build time from sanitized
            // uri-list leaves; anything unknown falls back to a fixed leaf so
            // a promise can never address an attacker-chosen path.
            let key = file_promise_provider as *const NSFilePromiseProvider as usize;
            let name = self
                .ivars()
                .files
                .borrow()
                .get(&key)
                .map(|file| file.name.clone());
            NSString::from_str(name.as_deref().unwrap_or("dropped-file"))
        }

        #[allow(non_snake_case)]
        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn filePromiseProvider_writePromiseToURL_completionHandler(
            &self,
            file_promise_provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let result = self.write_promise(file_promise_provider, url);
            match result {
                Ok(()) => completion_handler.call((ptr::null_mut(),)),
                Err(error) => {
                    log::warn!("Kitty DND file promise failed: {error}");
                    let ns_error = unsafe {
                        NSError::errorWithDomain_code_userInfo(
                            objc2_foundation::NSCocoaErrorDomain,
                            512,
                            None,
                        )
                    };
                    let raw = Retained::into_raw(ns_error);
                    completion_handler.call((raw,));
                }
            }
        }

        #[allow(non_snake_case)]
        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operationQueueForFilePromiseProvider(
            &self,
            _file_promise_provider: &NSFilePromiseProvider,
        ) -> Retained<NSOperationQueue> {
            // Fulfillment writes retained bytes to disk; keep it off the
            // AppKit main thread.
            NSOperationQueue::new()
        }
    }
);

impl DndFilePromiseDelegate {
    pub(crate) fn with_view(mtm: MainThreadMarker, view: &TerminalView) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(DndFilePromiseDelegateIvars {
            view: RefCell::new(Some(view.retain())),
            files: RefCell::new(HashMap::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Build one `NSFilePromiseProvider` for a derived uri-list entry and
    /// register its bytes identity for drop-time resolution. The delegate
    /// resolves bytes from the view's retained ingress when Finder asks.
    pub(crate) fn make_provider(
        &self,
        entry: RemoteEntryRef,
        file_name: &str,
    ) -> Option<Retained<NSFilePromiseProvider>> {
        let mtm = MainThreadMarker::new()?;
        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            &NSString::from_str("public.data"),
            ProtocolObject::from_ref(self),
        );
        self.ivars().files.borrow_mut().insert(
            &*provider as *const NSFilePromiseProvider as usize,
            PromiseFile {
                entry,
                name: file_name.to_string(),
            },
        );
        let _ = mtm;
        Some(provider)
    }

    fn write_promise(
        &self,
        file_promise_provider: &NSFilePromiseProvider,
        url: &NSURL,
    ) -> Result<(), String> {
        let view = self
            .ivars()
            .view
            .borrow()
            .as_ref()
            .map(Retained::clone)
            .ok_or_else(|| "drag view is gone".to_string())?;
        let key = file_promise_provider as *const NSFilePromiseProvider as usize;
        let file = self
            .ivars()
            .files
            .borrow()
            .get(&key)
            .map(|file| (file.entry, file.name.clone()))
            .ok_or_else(|| "promise is not registered".to_string())?;
        let (entry, file_name) = file;
        let Some(bytes) = view.dnd_promise_bytes(entry) else {
            // Fail closed without blocking the drop: ask core for the entry
            // (surfaces the consent prompt / `t=k` write when the drag is
            // still live; a no-op otherwise) so a follow-up drag succeeds,
            // and fail this promise now.
            view.dnd_request_promise_entry(entry);
            return Err(format!("no retained bytes for entry {}", entry.entry));
        };
        let destination = url
            .URLByAppendingPathComponent(&NSString::from_str(&file_name))
            .ok_or_else(|| "promise destination has no path".to_string())?;
        let path_ns = destination
            .path()
            .ok_or_else(|| "promise destination has no path".to_string())?;
        let path = path_ns.to_string();
        std::fs::write(&path, &bytes).map_err(|error| format!("write {path}: {error}"))?;
        Ok(())
    }
}

/// Build one dragging item for a servable payload. `text/plain` is
/// additionally declared as `NSPasteboardTypeString` so targets that only
/// read the string type still accept the drop. Returns `None` when the
/// payload cannot be represented (fail-closed: the caller skips it).
pub(crate) fn dragging_item_for_payload(
    payload: &ServablePayload,
) -> Option<Retained<NSDraggingItem>> {
    let _mtm = MainThreadMarker::new()?;
    let pasteboard_item = NSPasteboardItem::new();
    match payload.kind {
        DragPayloadKind::Text | DragPayloadKind::UriList => {
            let text = NSString::from_str(&String::from_utf8_lossy(&payload.data));
            let pasteboard_type: Retained<NSPasteboardType> =
                NSString::from_str(&payload.mime_type);
            if !pasteboard_item.setString_forType(&text, &pasteboard_type) {
                return None;
            }
            if payload.mime_type == "text/plain" {
                let string_type = unsafe { objc2_app_kit::NSPasteboardTypeString };
                let _ = pasteboard_item.setString_forType(&text, string_type);
            }
        }
        DragPayloadKind::Png => {
            let bytes = if payload.data.is_empty() {
                ptr::null()
            } else {
                payload.data.as_ptr() as *const _
            };
            let data = unsafe { NSData::dataWithBytes_length(bytes, payload.data.len()) };
            let png_type = unsafe { objc2_app_kit::NSPasteboardTypePNG };
            if !pasteboard_item.setData_forType(&data, png_type) {
                return None;
            }
        }
        DragPayloadKind::Other => {
            let bytes = if payload.data.is_empty() {
                ptr::null()
            } else {
                payload.data.as_ptr() as *const _
            };
            let data = unsafe { NSData::dataWithBytes_length(bytes, payload.data.len()) };
            let pasteboard_type: Retained<NSPasteboardType> =
                NSString::from_str(&payload.mime_type);
            if !pasteboard_item.setData_forType(&data, &pasteboard_type) {
                return None;
            }
        }
    }
    Some(NSDraggingItem::initWithPasteboardWriter(
        NSDraggingItem::alloc(),
        ProtocolObject::from_ref(&*pasteboard_item),
    ))
}

/// Wrap one file-promise provider as a dragging item for the session.
pub(crate) fn dragging_item_for_provider(
    provider: &NSFilePromiseProvider,
) -> Option<Retained<NSDraggingItem>> {
    let _mtm = MainThreadMarker::new()?;
    Some(NSDraggingItem::initWithPasteboardWriter(
        NSDraggingItem::alloc(),
        ProtocolObject::from_ref(provider),
    ))
}

/// Start the live `NSDraggingSession` for already-built items. Must be
/// called on AppKit's main thread; `event` is the stashed gesture event that
/// proves a genuine user drag. Returns `None` (fail-closed) when the mask is
/// empty, there are no items, or the session cannot start — the caller then
/// reports `os_drag_failed` so the application keeps its registration
/// instead of hearing an `OK` the frontend cannot honor.
pub(crate) fn begin_os_drag_session(
    view: &NSView,
    event: &objc2_app_kit::NSEvent,
    drag_source: &DndDragSource,
    mask: NSDragOperation,
    items: &[Retained<NSDraggingItem>],
) -> Option<Retained<NSDraggingSession>> {
    if mask.is_empty() || items.is_empty() {
        return None;
    }
    let array = NSArray::from_retained_slice(items);
    let source = ProtocolObject::from_ref(drag_source);
    Some(view.beginDraggingSessionWithItems_event_source(&array, event, source))
}

/// Servable check shared with the view: the plan the session is built from.
/// Mirrors GTK's provider gate (presented offer bytes + a usable operation).
pub fn session_plan(offer: &OsDragOffer, presented: &[PresentedMime]) -> Vec<ServablePayload> {
    servable_drag_payloads(offer, presented)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_app::kitty_dnd_source::PresentedMime;

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
        let anchor = Some((10.0, 10.0));
        assert!(should_emit_source_gesture(true, anchor, (16.0, 10.0)));
        assert!(should_emit_source_gesture(true, anchor, (10.0, 16.0)));
        assert!(!should_emit_source_gesture(true, anchor, (12.0, 10.0)));
        assert!(!should_emit_source_gesture(true, anchor, (10.0, 10.0)));
        assert!(!should_emit_source_gesture(false, anchor, (100.0, 100.0)));
        assert!(!should_emit_source_gesture(true, None, (100.0, 100.0)));
        assert!(!should_emit_source_gesture(true, anchor, (f64::NAN, 10.0)));
        assert!(!should_emit_source_gesture(
            true,
            Some((f64::INFINITY, 10.0)),
            (100.0, 100.0)
        ));
        assert!((source_gesture_distance((0.0, 0.0), (3.0, 4.0)) - 5.0).abs() < 1e-9);
        assert_eq!(SOURCE_DRAG_THRESHOLD_PX, 5.0);
    }

    #[test]
    fn operation_mask_maps_copy_move_either() {
        // Raw values are stable AppKit constants (Copy=1, Move=16).
        assert_eq!(drag_operation_mask(DndOperation::Copy).0, 1);
        assert_eq!(drag_operation_mask(DndOperation::Move).0, 16);
        assert_eq!(drag_operation_mask(DndOperation::Either).0, 17);
        assert!(drag_operation_mask(DndOperation::None).is_empty());
    }

    #[test]
    fn drag_result_maps_cancel_move_and_copy() {
        assert_eq!(resolve_drag_result(NSDragOperation(0)), (None, true));
        assert_eq!(
            resolve_drag_result(NSDragOperation::Move),
            (Some(DndOperation::Move), false)
        );
        assert_eq!(resolve_drag_result(NSDragOperation::Copy), (None, false));
        assert_eq!(
            resolve_drag_result(NSDragOperation::Copy | NSDragOperation::Move),
            (Some(DndOperation::Move), false)
        );
    }

    #[test]
    fn finish_frames_report_move_hint_then_terminal_finish() {
        use cterm_app::kitty_dnd::{DndLocation, DndOperation as Op};
        use cterm_core::{DndCommand, DndCommandType};

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

        // Move destination: hint first, then the terminal finish.
        let frames = finish_os_drag_frames(&mut source, NSDragOperation::Move);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].windows(4).any(|part| part == b":o=2"));
        assert!(frames[1].windows(7).any(|part| part == b"x=4:y=0"));

        // Canceled drop: single finish frame with `y=1`, registration kept.
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
        let frames = finish_os_drag_frames(&mut source, NSDragOperation(0));
        assert_eq!(frames.len(), 1);
        assert!(frames[0].windows(7).any(|part| part == b"x=4:y=1"));

        // Copy destination: no hint, just the finish.
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
        let frames = finish_os_drag_frames(&mut source, NSDragOperation::Copy);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].windows(7).any(|part| part == b"x=4:y=0"));

        let _ = Op::None;
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
            dnd_location_for_point(f64::NAN, -5.0, 8.0, 16.0),
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
        assert!(drag_operation_mask(offer.operation).contains(NSDragOperation::Copy));
    }

    #[tokio::test]
    async fn lifecycle_cancellation_denies_and_requests_sheet_dismissal() {
        let (_result_tx, result_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        cancel_tx.send_replace(true);

        assert_eq!(
            await_dnd_remote_prompt(
                result_rx,
                cancel_rx,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await,
            PromptOutcome {
                choice: DndRemoteChoice::Deny,
                dismiss: true,
            }
        );
    }

    #[tokio::test]
    async fn expiry_auto_denies_and_requests_sheet_dismissal() {
        let (_result_tx, result_rx) = oneshot::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        assert_eq!(
            await_dnd_remote_prompt(result_rx, cancel_rx, tokio::time::Instant::now()).await,
            PromptOutcome {
                choice: DndRemoteChoice::Deny,
                dismiss: true,
            }
        );
    }

    #[tokio::test]
    async fn explicit_completion_is_the_only_approval_path() {
        let (result_tx, result_rx) = oneshot::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        result_tx.send(DndRemoteChoice::AllowAlways).unwrap();

        assert_eq!(
            await_dnd_remote_prompt(
                result_rx,
                cancel_rx,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await,
            PromptOutcome {
                choice: DndRemoteChoice::AllowAlways,
                dismiss: false,
            }
        );
    }
}
