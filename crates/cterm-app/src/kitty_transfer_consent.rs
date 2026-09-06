//! Scripted, headless consent driver for Kitty OSC 5113 transfers.
//!
//! The daemon-owned deny-by-default dialogs (GTK/Cocoa/Win32) cannot run
//! without a compositor, so this module captures the dialog-result plumbing
//! seam they all share: mapping one native dialog outcome onto the exact-token
//! [`TtyTransferManager`] calls. Frontends keep owning pixel presentation and
//! click handling; everything at and below the dialog result is covered here
//! with no window system.
//!
//! Final hop still needing a human-driven or UI-automation pass, per backend:
//! - GTK: `MessageDialog` presentation, focus, Deny-as-default, the 100 ms
//!   expiry/cancel poll, and prompt-gate serialization
//!   (`cterm-gtk/src/tty_file_transfer_prompt.rs`).
//! - Cocoa: `NSAlert` sheet ownership, the main-thread prompt registry,
//!   token-addressed cancellation, and completion-to-`await_tty_file_transfer_prompt`
//!   wiring (`cterm-cocoa/src/tty_file_transfer_prompt.rs`).
//! - Win32: the `DialogBoxIndirectParamW` template, default-button focus, and
//!   cancel-message routing (`cterm-win32/src/tty_file_transfer_prompt.rs`,
//!   `window.rs` tty transfer dialog hooks).
//!
//! End-to-end PTY status bytes for approve/deny/expire/cancel are additionally
//! covered at the daemon RPC layer by
//! `test_tty_file_transfer_consent_replay_and_relaunch_refusal` in
//! `crates/cterm-headless/tests/grpc_integration.rs`.

use crate::kitty_file_transfer::{
    TransferConsentAudit, TransferConsentDecision, TtyTransferAction, TtyTransferBypassKind,
    TtyTransferManager,
};

/// Status reported when the daemon-side expiry fires before a dialog resolves.
/// Matches the worker literal in `cterm-headless/src/session/tty_transfer.rs`.
pub const CONSENT_EXPIRED_STATUS: &str = "ETIMEDOUT:Transfer approval expired";
/// Status reported when the session closes before a dialog resolves.
/// Matches the worker shutdown literal in `cterm-headless/src/session/tty_transfer.rs`.
pub const CONSENT_CANCELLED_STATUS: &str = "ECANCELED:Terminal session closed";

/// What the native dialog layer reports for one consent request.
///
/// This is the compositor-free seam: frontends translate their toolkit result
/// (button click, timeout, lifecycle cancellation) into one of these variants
/// and hand it to [`apply_dialog_outcome`]. The dialog itself is the only part
/// that still needs UI automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// The user explicitly clicked approve before the deadline.
    Approved,
    /// The user explicitly denied, closed the dialog, or the dialog reported
    /// any result other than approval.
    Denied,
    /// The daemon-advertised deadline passed before approval.
    Expired,
    /// The terminal session closed (or the daemon shut down) while prompting.
    Cancelled,
}

impl DialogOutcome {
    /// Collapse a raw toolkit result into a fail-closed outcome.
    ///
    /// Cancellation and expiry are biased ahead of a simultaneous positive
    /// completion so consent can never become valid after its deadline or its
    /// session: `Some(true)` paired with `expired` or `cancelled` still denies.
    /// A lost completion callback (`None`) denies.
    pub fn from_dialog_result(approved: Option<bool>, expired: bool, cancelled: bool) -> Self {
        if cancelled {
            Self::Cancelled
        } else if expired {
            Self::Expired
        } else {
            match approved {
                Some(true) => Self::Approved,
                _ => Self::Denied,
            }
        }
    }
}

/// Apply one dialog outcome to the exact-token authorization state machine.
///
/// Returns the protocol actions for the daemon worker to dispatch. Stale or
/// already-consumed tokens resolve to no actions: the manager only honors the
/// exact still-pending `request_id`, so a late click after expiry, cancel, a
/// peer `cancel` command, or session-id reuse can never authorize a transfer.
///
/// Every resolved outcome logs one [`TransferConsentAudit`] row carrying the
/// request id, transfer id, direction, decision, and evaluated bypass kind.
/// Dialog outcomes never carry an authorizing token hash: no credential
/// authorized anything on this path (bypass approvals log their hash where
/// they are granted, in the bypass evaluation seam).
pub fn apply_dialog_outcome(
    manager: &mut TtyTransferManager,
    request_id: u64,
    outcome: DialogOutcome,
) -> Vec<TtyTransferAction> {
    // Capture session context before acting: denial removes the session, so a
    // post-hoc lookup would lose the transfer id and direction for the audit
    // row. The lookup is read-only and cannot affect authorization.
    let (transfer_id, direction) = manager
        .pending_request_info(request_id)
        .map(|(session_id, direction)| (Some(session_id), Some(direction)))
        .unwrap_or((None, None));
    let audit = |decision: TransferConsentDecision| {
        TransferConsentAudit {
            request_id: Some(request_id),
            transfer_id: transfer_id.as_deref(),
            direction,
            decision,
            bypass_kind: TtyTransferBypassKind::None,
            token_hash: None,
        }
        .line()
    };
    match outcome {
        DialogOutcome::Approved => {
            let actions = manager.approve(request_id);
            if actions.is_empty() {
                log::warn!(
                    "OSC 5113 approval for request {request_id} arrived after its token lapsed; staying denied"
                );
            } else {
                log::info!("{}", audit(TransferConsentDecision::Approved));
            }
            actions
        }
        DialogOutcome::Denied => {
            let actions = manager.deny(request_id);
            if actions.is_empty() {
                log::warn!(
                    "OSC 5113 denial for request {request_id} arrived after its token lapsed"
                );
            } else {
                log::info!("{}", audit(TransferConsentDecision::Denied));
            }
            actions
        }
        DialogOutcome::Expired => {
            let actions = manager.reject(request_id, CONSENT_EXPIRED_STATUS);
            log::info!("{}", audit(TransferConsentDecision::Expired));
            actions
        }
        DialogOutcome::Cancelled => {
            let actions = manager.reject(request_id, CONSENT_CANCELLED_STATUS);
            log::info!("{}", audit(TransferConsentDecision::Cancelled));
            actions
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_core::{FileTransferAction, FileTransferCommand};

    fn command(action: FileTransferAction, id: &str) -> FileTransferCommand {
        FileTransferCommand {
            action,
            id: id.into(),
            file_id: None,
            bypass: None,
            quiet: 0,
            mtime: None,
            permissions: None,
            size: None,
            name: None,
            status: None,
            parent: None,
            data: Vec::new(),
            compression: None,
            file_type: None,
            transmission_type: None,
        }
    }

    /// Start one send session and return its exact consent token.
    fn request_send(manager: &mut TtyTransferManager, session_id: &str) -> u64 {
        let actions = manager.handle(command(FileTransferAction::Send, session_id));
        let [TtyTransferAction::RequestApproval(request)] = actions.as_slice() else {
            panic!("expected one approval request, got {actions:?}");
        };
        assert!(manager.is_approval_pending(request.request_id));
        request.request_id
    }

    fn write_status(actions: &[TtyTransferAction]) -> String {
        let [TtyTransferAction::Write(bytes)] = actions else {
            panic!("expected one protocol write, got {actions:?}");
        };
        let body = bytes
            .strip_prefix(b"\x1b]5113;")
            .and_then(|body| body.strip_suffix(b"\x1b\\"))
            .expect("encoded OSC 5113 response");
        let mut fields = vec![&b"5113"[..]];
        fields.extend(body.split(|byte| *byte == b';'));
        cterm_core::parse_file_transfer_command(&fields)
            .expect("valid response")
            .status
            .expect("status response carries decoded status text")
    }

    #[test]
    fn dialog_result_precedence_stays_fail_closed() {
        use DialogOutcome::*;
        assert_eq!(
            DialogOutcome::from_dialog_result(Some(true), false, false),
            Approved
        );
        assert_eq!(
            DialogOutcome::from_dialog_result(Some(false), false, false),
            Denied
        );
        assert_eq!(
            DialogOutcome::from_dialog_result(None, false, false),
            Denied
        );
        // A click that lands exactly on expiry or session teardown must not win.
        assert_eq!(
            DialogOutcome::from_dialog_result(Some(true), true, false),
            Expired
        );
        assert_eq!(
            DialogOutcome::from_dialog_result(Some(true), false, true),
            Cancelled
        );
        assert_eq!(
            DialogOutcome::from_dialog_result(Some(true), true, true),
            Cancelled
        );
        assert_eq!(
            DialogOutcome::from_dialog_result(None, false, true),
            Cancelled
        );
    }

    #[test]
    fn scripted_approve_emits_ok_and_arms_the_session() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "approve");
        let actions = apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved);
        assert!(write_status(&actions).contains("OK"));
        assert!(manager.has_session("approve"));

        let mut data = command(FileTransferAction::Data, "approve");
        data.file_id = Some("f1".into());
        assert!(matches!(
            manager.handle(data).as_slice(),
            [TtyTransferAction::Execute(_)]
        ));
    }

    #[test]
    fn scripted_deny_reports_eperm_and_invalidates_the_token() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "deny");
        let actions = apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Denied);
        assert!(write_status(&actions).contains("EPERM"));
        assert!(!manager.is_approval_pending(request_id));

        // A repeated or replayed denial for the spent token is a silent no-op.
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Denied).is_empty());
        // So is a late approval: the token cannot be revived.
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
    }

    #[test]
    fn scripted_expiry_reports_etimedout_and_blocks_late_approval() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "expire");
        let actions = apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Expired);
        assert!(write_status(&actions).contains("ETIMEDOUT"));
        assert!(!manager.is_approval_pending(request_id));

        // The user clicking "Allow" after the deadline must not authorize.
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
    }

    #[test]
    fn scripted_cancel_reports_ecanceled_and_blocks_late_approval() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "teardown");
        let actions = apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Cancelled);
        assert!(write_status(&actions).contains("ECANCELED"));
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
    }

    #[test]
    fn peer_cancel_before_the_dialog_resolves_spends_the_token() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "peer-cancel");
        let cancelled = manager.handle(command(FileTransferAction::Cancel, "peer-cancel"));
        assert!(write_status(&cancelled).contains("CANCELED"));

        // The dialog resolving afterwards — either way — authorizes nothing.
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Denied).is_empty());
    }

    #[test]
    fn reconnect_replay_keeps_the_live_token_until_it_resolves() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "reconnect");

        // A fresh frontend subscribing after the request observes the same
        // still-pending exact token instead of minting a second prompt.
        assert!(manager.is_approval_pending(request_id));
        let actions = apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Denied);
        assert!(write_status(&actions).contains("EPERM"));

        // Restarting the session id mints a strictly new token; the spent one
        // stays dead, so a replayed approval cannot authorize the new session.
        let current = request_send(&mut manager, "reconnect");
        assert_ne!(request_id, current);
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
        let actions = apply_dialog_outcome(&mut manager, current, DialogOutcome::Approved);
        assert!(write_status(&actions).contains("OK"));
    }

    #[test]
    fn double_respond_resolves_exactly_once() {
        let mut manager = TtyTransferManager::new();
        let request_id = request_send(&mut manager, "double");
        assert!(
            !apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty()
        );
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Approved).is_empty());
        assert!(apply_dialog_outcome(&mut manager, request_id, DialogOutcome::Denied).is_empty());
    }

    #[test]
    fn scripted_receive_approval_fans_out_to_listing_completion() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Receive, "receive");
        start.size = Some(1);
        assert!(manager.handle(start).is_empty());

        let mut file = command(FileTransferAction::File, "receive");
        file.file_id = Some("f1".into());
        file.name = Some("~/report.txt".into());
        let actions = manager.handle(file);
        let [TtyTransferAction::RequestApproval(request)] = actions.as_slice() else {
            panic!("expected one approval request, got {actions:?}");
        };

        let approved =
            apply_dialog_outcome(&mut manager, request.request_id, DialogOutcome::Approved);
        assert!(write_status(&approved[..1]).contains("OK"));
        assert!(approved
            .iter()
            .any(|action| matches!(action, TtyTransferAction::Execute(_))));
        assert!(matches!(
            approved.last(),
            Some(TtyTransferAction::CompleteReceiveListing { session_id, .. })
            if session_id == "receive"
        ));
    }

    #[test]
    fn quiet_sessions_stay_silent_through_the_scripted_paths() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Send, "quiet");
        start.quiet = 2;
        let actions = manager.handle(start);
        let [TtyTransferAction::RequestApproval(request)] = actions.as_slice() else {
            panic!("expected one approval request, got {actions:?}");
        };
        assert!(
            apply_dialog_outcome(&mut manager, request.request_id, DialogOutcome::Denied)
                .is_empty()
        );
        assert!(!manager.is_approval_pending(request.request_id));
    }
}
