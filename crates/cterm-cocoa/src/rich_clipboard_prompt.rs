//! Native macOS consent for Kitty OSC 5522 rich-clipboard reads.
//!
//! Reads that core policy did not pre-authorize (no exact-token consent, no
//! live paste-event token; see `cterm-core/src/kitty_clipboard.rs`) arrive
//! here instead of failing closed with `EPERM`. This module shows a
//! deny-by-default AppKit sheet mirroring the GTK adapter's semantics
//! (`terminal_widget.rs`): **Allow Once** answers exactly one read,
//! **Always Allow** (offered only when the request presented a credential)
//! remembers the application's exact `password`+`name` token via
//! `Screen::authorize_rich_clipboard` for future reads, and **Deny** (the
//! Return-key default, first button) fails the read with `EPERM`. Window
//! close, timeout, pane teardown, and any unexpected response all deny.
//!
//! Exact-token display: the sheet shows the requesting application's
//! human-readable name (`credential.name`, sanitized — it arrives in
//! terminal output and is untrusted) plus the requested MIME scope. The
//! password half of the token is never displayed; "Always Allow" stores the
//! exact presented pair, never a prefix.
//!
//! Expiry: OSC 5522 carries no daemon-advertised deadline (unlike OSC 5113's
//! `expires_in_ms`), so this frontend bounds every sheet at
//! [`RICH_CLIPBOARD_PROMPT_TTL`] (120 s, matching core's paste-token TTL
//! convention). The authoritative seam stays core's bounded pending-read
//! queue: answers for evicted or unknown reads are dropped by
//! `take_pending_rich_read` matching, never misrouted.
//!
//! Lifecycle: each sheet is addressed by a process-local token and dismissed
//! as a denial when its pane/reader goes away (shared watch channel, same as
//! the OSC 5113 flow) or when the view is deallocated.
//!
//! Runtime verification is impossible on this host (no macOS hardware — only
//! the Apple type-check targets), so the sheet behavior below still needs
//! on-host confirmation; nothing here is faked, only unrun.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::Message;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSModalResponseCancel, NSWindow};
use objc2_foundation::{MainThreadMarker, NSString};
use tokio::sync::{oneshot, watch};

use cterm_core::RichReadRequest;

/// Frontend-local bound for one clipboard consent sheet.
///
/// OSC 5522 advertises no expiry, so an unanswered sheet must not linger
/// indefinitely only to authorize a read the user long forgot about.
pub const RICH_CLIPBOARD_PROMPT_TTL: Duration = Duration::from_secs(120);

/// Maximum characters of untrusted application/scope text shown in a sheet.
const MAX_DISPLAY_CHARS: usize = 160;

static NEXT_PROMPT_TOKEN: AtomicU64 = AtomicU64::new(1);

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

/// The user's answer to one clipboard consent sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichClipboardChoice {
    /// Answer exactly this read; remember nothing.
    AllowOnce,
    /// Remember the exact presented token, then answer this read.
    AllowAlways,
    /// Fail the read with `EPERM`.
    Deny,
}

/// How one consent sheet resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptOutcome {
    pub choice: RichClipboardChoice,
    /// The caller must close an outstanding AppKit sheet for timeout,
    /// lifecycle cancellation, or a lost completion callback.
    pub dismiss: bool,
}

/// Allocate a process-local token used only to address one owned AppKit sheet.
pub fn next_rich_clipboard_prompt_token() -> u64 {
    let token = NEXT_PROMPT_TOKEN.fetch_add(1, Ordering::Relaxed);
    if token == 0 {
        NEXT_PROMPT_TOKEN.fetch_add(1, Ordering::Relaxed)
    } else {
        token
    }
}

/// Title and body for one consent sheet. Pure (no AppKit call), so the exact
/// wording is unit-testable. Mirrors the GTK adapter's text: application
/// name plus MIME scope, with list-vs-data phrasing.
pub fn format_rich_clipboard_prompt(request: &RichReadRequest) -> (String, String) {
    let app = request
        .app_name
        .as_deref()
        .map(|name| sanitize_display_text(name, MAX_DISPLAY_CHARS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "A terminal application".to_string());
    let scope: String = sanitize_display_text(&request.mimes.join(", "), MAX_DISPLAY_CHARS);
    let body = if request.list_only {
        format!("{app} wants to list the available clipboard types.")
    } else if scope.is_empty() {
        format!("{app} wants to read the system clipboard.")
    } else {
        format!("{app} wants to read the system clipboard ({scope}).")
    };
    let body = if request.credential.is_some() {
        format!(
            "{body}\n\n\"Always allow\" remembers this exact application token for future reads."
        )
    } else {
        body
    };
    ("Allow clipboard access?".to_string(), body)
}

/// Begin one fail-closed, main-thread-owned consent sheet without blocking
/// the AppKit event loop.
///
/// Deny is the first (Return-key default) button, so pressing Return,
/// closing the sheet, or receiving an unexpected response can never grant
/// clipboard access. "Always Allow" is offered only when the request
/// presented a credential worth remembering.
pub fn begin_rich_clipboard_prompt(
    mtm: MainThreadMarker,
    token: u64,
    request: &RichReadRequest,
    parent: &NSWindow,
    cancelled: Arc<AtomicBool>,
    result_tx: oneshot::Sender<RichClipboardChoice>,
) {
    if cancelled.load(Ordering::Acquire) {
        let _ = result_tx.send(RichClipboardChoice::Deny);
        return;
    }
    let (title, body) = format_rich_clipboard_prompt(request);
    let offer_always = request.credential.is_some();

    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(&title));
    alert.setInformativeText(&NSString::from_str(&body));

    alert.addButtonWithTitle(&NSString::from_str("Deny"));
    alert.addButtonWithTitle(&NSString::from_str("Allow Once"));
    if offer_always {
        alert.addButtonWithTitle(&NSString::from_str("Always Allow"));
    }

    let completion_sender = RefCell::new(Some(result_tx));
    let completion_cancelled = Arc::clone(&cancelled);
    let completion_alert = alert.clone();
    let completion = RcBlock::new(move |response: objc2_app_kit::NSModalResponse| {
        // Match AppKit's ownership pattern used by the OSC 5113 prompts:
        // retain the NSAlert through its completion callback even after the
        // registry releases its cancellation handle.
        let _keep_alert_alive = &completion_alert;
        ACTIVE_PROMPTS.with(|prompts| {
            prompts.borrow_mut().remove(&token);
        });
        let choice = if completion_cancelled.load(Ordering::Acquire)
            || response == objc2_app_kit::NSAlertFirstButtonReturn
        {
            RichClipboardChoice::Deny
        } else if response == objc2_app_kit::NSAlertSecondButtonReturn {
            RichClipboardChoice::AllowOnce
        } else if offer_always && response == objc2_app_kit::NSAlertThirdButtonReturn {
            RichClipboardChoice::AllowAlways
        } else {
            RichClipboardChoice::Deny
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
pub fn cancel_rich_clipboard_prompt(_mtm: MainThreadMarker, token: u64) {
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

/// Await a sheet result, the frontend-local expiry, or pane/reader
/// cancellation. Cancellation and expiry are biased ahead of a simultaneous
/// positive completion so consent can never become valid after its deadline.
pub async fn await_rich_clipboard_prompt(
    result_rx: oneshot::Receiver<RichClipboardChoice>,
    mut lifecycle_cancel: watch::Receiver<bool>,
    expires_at: tokio::time::Instant,
) -> PromptOutcome {
    if *lifecycle_cancel.borrow() {
        return PromptOutcome {
            choice: RichClipboardChoice::Deny,
            dismiss: true,
        };
    }

    tokio::select! {
        biased;
        _ = lifecycle_cancel.changed() => PromptOutcome {
            choice: RichClipboardChoice::Deny,
            dismiss: true,
        },
        _ = tokio::time::sleep_until(expires_at) => PromptOutcome {
            choice: RichClipboardChoice::Deny,
            dismiss: true,
        },
        result = result_rx => match result {
            Ok(choice) => PromptOutcome { choice, dismiss: false },
            Err(_) => PromptOutcome {
                choice: RichClipboardChoice::Deny,
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
        request.mimes = vec!["x/".to_string() + &"y".repeat(400)];
        let (_, body) = format_rich_clipboard_prompt(&request);
        assert!(!body.contains('\u{1b}'));
        assert!(!body.contains('\u{7}'));
        assert!(!body.contains('\u{202e}'));
        assert!(body.contains('…'));
    }

    #[tokio::test]
    async fn lifecycle_cancellation_denies_and_requests_sheet_dismissal() {
        let (_result_tx, result_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        cancel_tx.send_replace(true);

        assert_eq!(
            await_rich_clipboard_prompt(
                result_rx,
                cancel_rx,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await,
            PromptOutcome {
                choice: RichClipboardChoice::Deny,
                dismiss: true,
            }
        );
    }

    #[tokio::test]
    async fn expiry_auto_denies_and_requests_sheet_dismissal() {
        let (_result_tx, result_rx) = oneshot::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        assert_eq!(
            await_rich_clipboard_prompt(result_rx, cancel_rx, tokio::time::Instant::now()).await,
            PromptOutcome {
                choice: RichClipboardChoice::Deny,
                dismiss: true,
            }
        );
    }

    #[tokio::test]
    async fn explicit_completion_is_the_only_approval_path() {
        let (result_tx, result_rx) = oneshot::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        result_tx.send(RichClipboardChoice::AllowAlways).unwrap();

        assert_eq!(
            await_rich_clipboard_prompt(
                result_rx,
                cancel_rx,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await,
            PromptOutcome {
                choice: RichClipboardChoice::AllowAlways,
                dismiss: false,
            }
        );
    }
}
