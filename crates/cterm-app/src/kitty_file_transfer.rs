//! Authorization state machine for Kitty OSC 5113 file transfers.
//!
//! Filesystem work is deliberately represented as an action. Native frontends
//! must obtain user approval before passing an authorized command to a future
//! filesystem executor.

use std::collections::HashMap;

use cterm_core::{FileTransferAction, FileTransferCommand, MAX_FILE_TRANSFER_PATH_BYTES};

use crate::config::{TransferConfig, TransferConsentMode};
use crate::transfer_bypass::{hash_bypass_token, verify_bypass_credential};

const MAX_ACTIVE_SESSIONS: usize = 16;
const MAX_RECEIVE_PATHS: usize = 256;

/// Direction from the perspective of the process using the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtyTransferDirection {
    /// The process sends files to the computer running cterm.
    Send,
    /// The process requests files from the computer running cterm.
    Receive,
}

/// A native consent prompt which has not granted any filesystem access yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtyTransferApprovalRequest {
    /// Monotonic token; frontends must return this, not only the session id.
    pub request_id: u64,
    pub session_id: String,
    pub direction: TtyTransferDirection,
    /// Requested local paths for receive sessions; send sessions have none yet.
    pub paths: Vec<String>,
}

/// A validated command from a session that received explicit user consent.
///
/// The fields are private so raw parser output cannot be passed to the
/// filesystem executor without crossing the authorization state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedTtyTransferCommand {
    direction: TtyTransferDirection,
    quiet: u8,
    command: FileTransferCommand,
}

impl AuthorizedTtyTransferCommand {
    pub fn direction(&self) -> TtyTransferDirection {
        self.direction
    }

    pub fn quiet(&self) -> u8 {
        self.quiet
    }

    pub fn command(&self) -> &FileTransferCommand {
        &self.command
    }

    pub(crate) fn into_parts(self) -> (TtyTransferDirection, u8, FileTransferCommand) {
        (self.direction, self.quiet, self.command)
    }
}

/// Requested authorization-bypass condition for one transfer command.
///
/// The policy proposal lives in `docs/transfer-bypass-policy.md`. Only the
/// `PasswordCredential` condition is implemented, and only behind an explicit
/// operator opt-in (see [`evaluate_transfer_bypass_with_policy`]); every
/// other combination still prompts. The wire `pw` bypass credential decoded
/// from untrusted PTY bytes can never authorize a transfer on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtyTransferBypassKind {
    /// No bypass requested: always prompt, ignoring any wire credential.
    /// This is the default.
    #[default]
    None,
    /// Evaluate the wire `pw` credential against the configured password.
    /// Still prompts unless the direction was explicitly allowed *and* a
    /// password is configured *and* the credential verifies.
    PasswordCredential,
    /// Decided + implemented: a session the local user pre-authorized on
    /// the same TTY (`cterm transfer --allow-once`). The daemon consumes
    /// the binding at session-start evaluation; the policy seam itself
    /// still prompts for this kind (an explicit direction deny refuses),
    /// since the consumed pre-auth arrives as `Allow` from the worker.
    SameTtyPreauthorized,
    /// Proposed: loopback automation where the peer is the local machine.
    /// Declined: always prompts (an explicit direction deny still refuses).
    LoopbackAutomation,
}

/// Outcome of evaluating a bypass request.
///
/// `Prompt` stays the default for every input. `Allow` additionally requires
/// an explicit direction opt-in *and* a verified credential, and `Deny`
/// requires an explicit direction deny (see
/// [`evaluate_transfer_bypass_with_policy`]). Nothing is inferred from
/// defaults: an unset config evaluates exactly like the legacy
/// [`evaluate_transfer_bypass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassDecision {
    /// Show the native deny-by-default consent dialog.
    Prompt,
    /// Skip the dialog: the direction was explicitly allowed and the wire
    /// bypass credential verified against the configured password.
    Allow,
    /// Refuse without prompting: the direction was explicitly denied.
    Deny,
}

/// Evaluate a bypass request without weakening enforcement.
///
/// Always returns [`BypassDecision::Prompt`], including for commands that
/// carry a wire `pw` bypass credential and for every proposed
/// [`TtyTransferBypassKind`]. The regression tests below lock this in: any
/// future policy landing must extend this function and its decision enum
/// deliberately, never by relaxing the codec or the state machine.
pub fn evaluate_transfer_bypass(
    command: &FileTransferCommand,
    kind: TtyTransferBypassKind,
) -> BypassDecision {
    let _ = (command, kind);
    BypassDecision::Prompt
}

/// Resolved inputs for one bypass evaluation: operator config plus the
/// terminal-side secrets needed for verification.
///
/// Build one per evaluation from the loaded [`TransferConfig`]; there is no
/// global policy object, so every evaluation states what it ran under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferBypassPolicy<'a> {
    /// Operator config (password opt-in plus per-direction modes).
    pub config: &'a TransferConfig,
    /// This terminal's X25519 private key for the `kitty-1` envelope arm, if
    /// the daemon holds one. `None` fails that arm closed while leaving the
    /// `sha256` arm usable.
    pub decryption_key: Option<&'a [u8; 32]>,
    /// Current time in nanoseconds since the Unix epoch, for the envelope
    /// freshness window (see `transfer_bypass::current_time_ns`).
    pub now_ns: u64,
}

/// Consent decisions, shared by the bypass seam and the dialog outcomes so
/// every transfer logs one audit row in one format (see
/// [`TransferConsentAudit::line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferConsentDecision {
    /// Evaluation deferred to the human dialog.
    Prompt,
    /// Bypass auto-approved (explicit opt-in plus verified credential).
    Allow,
    /// Hard-refused by direction policy, no dialog shown.
    Deny,
    /// The user explicitly approved in the dialog.
    Approved,
    /// The user explicitly denied (or dismissed) the dialog.
    Denied,
    /// The daemon-advertised deadline passed before approval.
    Expired,
    /// The terminal session closed while prompting.
    Cancelled,
}

impl TransferConsentDecision {
    /// The stable log vocabulary for this decision.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

impl From<BypassDecision> for TransferConsentDecision {
    fn from(decision: BypassDecision) -> Self {
        match decision {
            BypassDecision::Prompt => Self::Prompt,
            BypassDecision::Allow => Self::Allow,
            BypassDecision::Deny => Self::Deny,
        }
    }
}

/// One audit row for a transfer consent decision.
///
/// The authorizing token is recorded only as a hex hash (see
/// `transfer_bypass::hash_bypass_token`); raw credentials and passwords never
/// appear in logs. The wire session id is codec-bounded to safe characters,
/// so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferConsentAudit<'a> {
    /// Terminal-minted approval token, if one was assigned yet (`None` while
    /// evaluating, before the session starts).
    pub request_id: Option<u64>,
    /// Wire session id, if the session is (still) known.
    pub transfer_id: Option<&'a str>,
    /// Transfer direction, if known.
    pub direction: Option<TtyTransferDirection>,
    /// The decision this row records.
    pub decision: TransferConsentDecision,
    /// The bypass condition that was evaluated (dialog outcomes report the
    /// kind the request was evaluated under, usually `None`).
    pub bypass_kind: TtyTransferBypassKind,
    /// Hex hash of the wire credential that authorized this decision, if any.
    /// `None` whenever no credential authorized anything — including failed
    /// verification attempts.
    pub token_hash: Option<&'a str>,
}

impl TransferConsentAudit<'_> {
    /// Render the row as a single greppable log line.
    pub fn line(&self) -> String {
        format!(
            "OSC 5113 transfer decision={} request_id={} transfer_id={} direction={} bypass={:?} token_hash={}",
            self.decision.as_str(),
            self.request_id
                .map_or_else(|| "-".to_string(), |id| id.to_string()),
            self.transfer_id.unwrap_or("-"),
            self.direction.map_or("-", |direction| match direction {
                TtyTransferDirection::Send => "send",
                TtyTransferDirection::Receive => "receive",
            }),
            self.bypass_kind,
            self.token_hash.unwrap_or("-"),
        )
    }
}

/// Evaluate a bypass request against an explicit operator policy.
///
/// This is the only seam where verification can authorize anything, and it
/// stays deny-by-default:
///
/// - An explicit direction deny hard-refuses (`Deny`) without prompting, for
///   every bypass kind including the proposed (undecided) ones.
/// - `SameTtyPreauthorized` and `LoopbackAutomation` always prompt here:
///   same-TTY skips are consumed daemon-side (arriving as `Allow` from the
///   worker, decided policy) and loopback automation was declined, so this
///   seam never authorizes either kind itself.
/// - `None` always prompts and ignores any wire credential, exactly like the
///   legacy [`evaluate_transfer_bypass`].
/// - `PasswordCredential` prompts unless the direction was *explicitly* set
///   to `allow` in the config **and** a non-empty `bypass_password` is
///   configured **and** the wire credential verifies against it (bound to the
///   session id via `transfer_bypass::verify_bypass_credential`).
/// - Bypass is only evaluated for session-start (`send`/`receive`) commands;
///   anything else prompts (the state machine rejects pre-approval commands
///   regardless).
///
/// Every `Allow`/`Deny` decision is audit-logged with the authorizing token
/// hash; `Prompt` evaluations log at debug level so the later dialog outcome
/// stays the single info-level row for that transfer.
pub fn evaluate_transfer_bypass_with_policy(
    command: &FileTransferCommand,
    kind: TtyTransferBypassKind,
    policy: &TransferBypassPolicy<'_>,
) -> BypassDecision {
    let direction = match command.action {
        FileTransferAction::Send => TtyTransferDirection::Send,
        FileTransferAction::Receive => TtyTransferDirection::Receive,
        _ => return BypassDecision::Prompt,
    };
    let audit = |decision: TransferConsentDecision, token_hash: Option<&str>| {
        TransferConsentAudit {
            request_id: None,
            transfer_id: Some(command.id.as_str()),
            direction: Some(direction),
            decision,
            bypass_kind: kind,
            token_hash,
        }
        .line()
    };
    if policy.config.resolve(direction) == TransferConsentMode::Deny {
        log::info!("{}", audit(TransferConsentDecision::Deny, None));
        return BypassDecision::Deny;
    }
    match kind {
        TtyTransferBypassKind::SameTtyPreauthorized | TtyTransferBypassKind::LoopbackAutomation => {
            log::debug!("{}", audit(TransferConsentDecision::Prompt, None));
            BypassDecision::Prompt
        }
        TtyTransferBypassKind::None => {
            // No bypass requested: any wire credential stays ignored, exactly
            // like the legacy seam.
            log::debug!("{}", audit(TransferConsentDecision::Prompt, None));
            BypassDecision::Prompt
        }
        TtyTransferBypassKind::PasswordCredential => {
            let password = policy.config.effective_password();
            let credential = command.bypass.as_deref();
            match (
                policy.config.is_explicit_allow(direction),
                password,
                credential,
            ) {
                (true, Some(password), Some(credential))
                    if verify_bypass_credential(
                        &command.id,
                        credential,
                        password,
                        policy.decryption_key,
                        policy.now_ns,
                    ) =>
                {
                    let token_hash = hash_bypass_token(credential);
                    log::info!(
                        "{}",
                        audit(TransferConsentDecision::Allow, Some(&token_hash))
                    );
                    BypassDecision::Allow
                }
                (true, Some(_), Some(_)) => {
                    // Explicitly opted in, but the credential is wrong: a
                    // tamper/replay signal worth a warning. The session id is
                    // codec-bounded to safe characters, so it is safe to log.
                    log::warn!(
                        "OSC 5113 bypass credential for transfer '{}' failed verification; keeping the consent prompt",
                        command.id
                    );
                    log::debug!("{}", audit(TransferConsentDecision::Prompt, None));
                    BypassDecision::Prompt
                }
                _ => {
                    log::debug!("{}", audit(TransferConsentDecision::Prompt, None));
                    BypassDecision::Prompt
                }
            }
        }
    }
}

/// Work emitted by the UI-neutral authorization state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtyTransferAction {
    RequestApproval(TtyTransferApprovalRequest),
    /// A protocol response to write to the PTY.
    Write(Vec<u8>),
    /// A command whose session has received explicit user consent.
    Execute(AuthorizedTtyTransferCommand),
    /// Finish the metadata listing for an approved receive session. The
    /// filesystem executor supplies the local home path in the terminating
    /// `OK` response after every queued path has been inspected.
    CompleteReceiveListing {
        session_id: String,
        quiet: u8,
    },
    /// Drop any filesystem staging owned by this session.
    Abort {
        session_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    CollectReceivePaths,
    AwaitApproval,
    Approved,
}

#[derive(Debug)]
struct TransferSession {
    direction: TtyTransferDirection,
    phase: SessionPhase,
    quiet: u8,
    request_id: u64,
    expected_paths: usize,
    receive_requests: Vec<FileTransferCommand>,
}

/// Bounded OSC 5113 session authorization shared by all native frontends.
#[derive(Debug)]
pub struct TtyTransferManager {
    sessions: HashMap<String, TransferSession>,
    next_request_id: u64,
    receive_supported: bool,
}

impl Default for TtyTransferManager {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            next_request_id: 1,
            receive_supported: true,
        }
    }
}

impl TtyTransferManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a manager for an executor that does not yet implement local
    /// filesystem reads. Unsupported receive requests fail before a consent
    /// prompt or an `OK` response can imply that data will follow.
    pub fn send_only() -> Self {
        Self {
            receive_supported: false,
            ..Self::default()
        }
    }

    /// Consume one decoded command without performing filesystem I/O.
    pub fn handle(&mut self, command: FileTransferCommand) -> Vec<TtyTransferAction> {
        match command.action {
            FileTransferAction::Send => self.start(command, TtyTransferDirection::Send),
            FileTransferAction::Receive => self.start(command, TtyTransferDirection::Receive),
            FileTransferAction::Cancel => self.cancel(command),
            _ => self.continue_session(command),
        }
    }

    /// Grant the exact still-pending request represented by `request_id`.
    pub fn approve(&mut self, request_id: u64) -> Vec<TtyTransferAction> {
        let Some(session_id) = self.session_id_for_request(request_id) else {
            return Vec::new();
        };
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Vec::new();
        };
        if session.phase != SessionPhase::AwaitApproval {
            return Vec::new();
        }

        session.phase = SessionPhase::Approved;
        let mut actions = Vec::new();
        if let Some(response) = status_response(&session_id, None, "OK", session.quiet, false) {
            actions.push(TtyTransferAction::Write(response));
        }
        if session.direction == TtyTransferDirection::Receive {
            let direction = session.direction;
            let quiet = session.quiet;
            actions.extend(
                std::mem::take(&mut session.receive_requests)
                    .into_iter()
                    .map(move |command| {
                        TtyTransferAction::Execute(AuthorizedTtyTransferCommand {
                            direction,
                            quiet,
                            command,
                        })
                    }),
            );
            actions.push(TtyTransferAction::CompleteReceiveListing { session_id, quiet });
        }
        actions
    }

    /// Refuse and discard the exact still-pending approval request.
    pub fn deny(&mut self, request_id: u64) -> Vec<TtyTransferAction> {
        self.reject(request_id, "EPERM:User refused the transfer")
    }

    /// Refuse a pending request for a daemon policy reason such as expiry.
    pub fn reject(&mut self, request_id: u64, status: &str) -> Vec<TtyTransferAction> {
        let Some(session_id) = self.session_id_for_request(request_id) else {
            return Vec::new();
        };
        self.reject_session(&session_id, status)
    }

    /// End one pending or approved session for a daemon lifecycle reason.
    /// Approved sessions emit `Abort` before their terminal-visible error so
    /// filesystem staging is discarded before the peer can retry.
    pub fn reject_session(&mut self, session_id: &str, status: &str) -> Vec<TtyTransferAction> {
        let Some(session) = self.sessions.remove(session_id) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if session.phase == SessionPhase::Approved {
            actions.push(TtyTransferAction::Abort {
                session_id: session_id.to_string(),
            });
        }
        if let Some(response) = status_response(session_id, None, status, session.quiet, true) {
            actions.push(TtyTransferAction::Write(response));
        }
        actions
    }

    /// End every live session, used for PTY EOF and daemon shutdown cleanup.
    pub fn reject_all(&mut self, status: &str) -> Vec<TtyTransferAction> {
        let session_ids: Vec<_> = self.sessions.keys().cloned().collect();
        session_ids
            .into_iter()
            .flat_map(|session_id| self.reject_session(&session_id, status))
            .collect()
    }

    pub fn active_sessions(&self) -> usize {
        self.sessions.len()
    }

    pub fn has_session(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Whether this exact opaque token still names a session awaiting consent.
    ///
    /// Executors use this after processing protocol input so duplicate or
    /// otherwise rejected commands cannot accidentally dismiss a live prompt.
    pub fn is_approval_pending(&self, request_id: u64) -> bool {
        self.session_id_for_request(request_id).is_some()
    }

    /// Session context for a still-pending approval token, for audit logging.
    ///
    /// Returns the wire session id and direction without touching
    /// authorization state. `None` once the token lapsed (approved, denied,
    /// expired, cancelled, or never issued).
    pub fn pending_request_info(&self, request_id: u64) -> Option<(String, TtyTransferDirection)> {
        self.sessions.iter().find_map(|(session_id, session)| {
            (session.request_id == request_id && session.phase == SessionPhase::AwaitApproval)
                .then(|| (session_id.clone(), session.direction))
        })
    }

    fn start(
        &mut self,
        command: FileTransferCommand,
        direction: TtyTransferDirection,
    ) -> Vec<TtyTransferAction> {
        if direction == TtyTransferDirection::Receive && !self.receive_supported {
            return error_response(&command, "ENOTSUP:Receive sessions are not implemented");
        }
        if self.sessions.contains_key(&command.id) {
            return error_response(&command, "EEXIST:Transfer session already exists");
        }
        if self.sessions.len() >= MAX_ACTIVE_SESSIONS {
            return error_response(&command, "ENOSPC:Too many transfer sessions");
        }

        let expected_paths = if direction == TtyTransferDirection::Receive {
            let Some(expected) = command.size.and_then(|size| usize::try_from(size).ok()) else {
                return error_response(&command, "EINVAL:Missing receive path count");
            };
            if expected > MAX_RECEIVE_PATHS {
                return error_response(&command, "E2BIG:Too many receive paths");
            }
            expected
        } else {
            0
        };
        let request_id = self.allocate_request_id();
        let session_id = command.id.clone();
        let phase = if direction == TtyTransferDirection::Receive && expected_paths != 0 {
            SessionPhase::CollectReceivePaths
        } else {
            SessionPhase::AwaitApproval
        };
        self.sessions.insert(
            session_id.clone(),
            TransferSession {
                direction,
                phase,
                quiet: command.quiet,
                request_id,
                expected_paths,
                receive_requests: Vec::with_capacity(expected_paths),
            },
        );

        if phase == SessionPhase::AwaitApproval {
            vec![TtyTransferAction::RequestApproval(
                TtyTransferApprovalRequest {
                    request_id,
                    session_id,
                    direction,
                    paths: Vec::new(),
                },
            )]
        } else {
            Vec::new()
        }
    }

    fn cancel(&mut self, command: FileTransferCommand) -> Vec<TtyTransferAction> {
        let Some(session) = self.sessions.remove(&command.id) else {
            return error_response(&command, "ENOENT:Unknown transfer session");
        };
        let mut actions = Vec::new();
        if session.phase == SessionPhase::Approved {
            actions.push(TtyTransferAction::Abort {
                session_id: command.id.clone(),
            });
        }
        if let Some(response) = status_response(&command.id, None, "CANCELED", session.quiet, false)
        {
            actions.push(TtyTransferAction::Write(response));
        }
        actions
    }

    fn continue_session(&mut self, command: FileTransferCommand) -> Vec<TtyTransferAction> {
        let session_id = command.id.clone();
        let Some(phase) = self.sessions.get(&session_id).map(|session| session.phase) else {
            return error_response(&command, "ENOENT:Unknown transfer session");
        };

        match phase {
            SessionPhase::CollectReceivePaths => self.collect_receive_path(command),
            SessionPhase::AwaitApproval => {
                let quiet = self
                    .sessions
                    .remove(&session_id)
                    .map_or(command.quiet, |session| session.quiet);
                status_response(
                    &session_id,
                    command.file_id.as_deref(),
                    "EINVAL:Command received before authorization",
                    quiet,
                    true,
                )
                .map(TtyTransferAction::Write)
                .into_iter()
                .collect()
            }
            SessionPhase::Approved => self.authorized_command(command),
        }
    }

    fn collect_receive_path(&mut self, command: FileTransferCommand) -> Vec<TtyTransferAction> {
        let session_id = command.id.clone();
        let is_valid_file = command.action == FileTransferAction::File
            && command.file_id.is_some()
            && command.name.as_deref().is_some_and(valid_protocol_path);
        if !is_valid_file {
            let quiet = self
                .sessions
                .remove(&session_id)
                .map_or(command.quiet, |session| session.quiet);
            return status_response(
                &session_id,
                command.file_id.as_deref(),
                "EINVAL:Invalid receive path request",
                quiet,
                true,
            )
            .map(TtyTransferAction::Write)
            .into_iter()
            .collect();
        }

        let session = self.sessions.get_mut(&session_id).expect("session exists");
        let file_id = command.file_id.as_deref().expect("validated file id");
        if session
            .receive_requests
            .iter()
            .any(|request| request.file_id.as_deref() == Some(file_id))
        {
            let quiet = self
                .sessions
                .remove(&session_id)
                .map_or(command.quiet, |session| session.quiet);
            return status_response(
                &session_id,
                Some(file_id),
                "EEXIST:Duplicate file id",
                quiet,
                true,
            )
            .map(TtyTransferAction::Write)
            .into_iter()
            .collect();
        }
        session.receive_requests.push(command);
        if session.receive_requests.len() != session.expected_paths {
            return Vec::new();
        }
        session.phase = SessionPhase::AwaitApproval;
        vec![TtyTransferAction::RequestApproval(
            TtyTransferApprovalRequest {
                request_id: session.request_id,
                session_id,
                direction: session.direction,
                paths: session
                    .receive_requests
                    .iter()
                    .filter_map(|command| command.name.clone())
                    .collect(),
            },
        )]
    }

    fn authorized_command(&mut self, command: FileTransferCommand) -> Vec<TtyTransferAction> {
        let direction = self.sessions[&command.id].direction;
        let closes = matches!(
            (direction, command.action),
            (TtyTransferDirection::Send, FileTransferAction::Finish)
                | (TtyTransferDirection::Receive, FileTransferAction::Finished)
        );
        let permitted = command_shape_is_valid(&command)
            && match direction {
                TtyTransferDirection::Send => matches!(
                    command.action,
                    FileTransferAction::File
                        | FileTransferAction::Data
                        | FileTransferAction::EndData
                        | FileTransferAction::Finish
                ),
                TtyTransferDirection::Receive => matches!(
                    command.action,
                    FileTransferAction::File
                        | FileTransferAction::Data
                        | FileTransferAction::EndData
                        | FileTransferAction::Finished
                ),
            };
        if !permitted {
            let quiet = self
                .sessions
                .remove(&command.id)
                .map_or(command.quiet, |session| session.quiet);
            let mut actions = vec![TtyTransferAction::Abort {
                session_id: command.id.clone(),
            }];
            if let Some(response) = status_response(
                &command.id,
                command.file_id.as_deref(),
                "EINVAL:Action is invalid for transfer direction",
                quiet,
                true,
            ) {
                actions.push(TtyTransferAction::Write(response));
            }
            return actions;
        }
        let authorized = AuthorizedTtyTransferCommand {
            direction,
            quiet: self.sessions[&command.id].quiet,
            command,
        };
        if closes {
            self.sessions.remove(&authorized.command.id);
        }
        vec![TtyTransferAction::Execute(authorized)]
    }

    fn session_id_for_request(&self, request_id: u64) -> Option<String> {
        self.sessions.iter().find_map(|(id, session)| {
            (session.request_id == request_id && session.phase == SessionPhase::AwaitApproval)
                .then(|| id.clone())
        })
    }

    fn allocate_request_id(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        request_id
    }
}

fn command_shape_is_valid(command: &FileTransferCommand) -> bool {
    match command.action {
        FileTransferAction::File => {
            command.file_id.is_some() && command.name.as_deref().is_some_and(valid_protocol_path)
        }
        FileTransferAction::Data | FileTransferAction::EndData => command.file_id.is_some(),
        FileTransferAction::Finish | FileTransferAction::Finished => true,
        _ => false,
    }
}

fn valid_protocol_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_FILE_TRANSFER_PATH_BYTES
        && !bytes.contains(&0)
        && bytes
            .split(|byte| *byte == b'/')
            .all(|part| part.len() <= 255 && part != b"." && part != b"..")
}

fn error_response(command: &FileTransferCommand, status: &str) -> Vec<TtyTransferAction> {
    status_response(
        &command.id,
        command.file_id.as_deref(),
        status,
        command.quiet,
        true,
    )
    .map(TtyTransferAction::Write)
    .into_iter()
    .collect()
}

fn status_response(
    session_id: &str,
    file_id: Option<&str>,
    status: &str,
    quiet: u8,
    is_error: bool,
) -> Option<Vec<u8>> {
    if quiet >= 2 || quiet == 1 && !is_error {
        return None;
    }
    FileTransferCommand {
        action: FileTransferAction::Status,
        id: session_id.to_string(),
        file_id: file_id.map(str::to_string),
        bypass: None,
        quiet: 0,
        mtime: None,
        permissions: None,
        size: None,
        name: None,
        status: Some(status.to_string()),
        parent: None,
        data: Vec::new(),
        compression: None,
        file_type: None,
        transmission_type: None,
    }
    .encode()
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn approval(actions: &[TtyTransferAction]) -> &TtyTransferApprovalRequest {
        let [TtyTransferAction::RequestApproval(request)] = actions else {
            panic!("expected one approval request, got {actions:?}");
        };
        request
    }

    #[test]
    fn send_waits_for_exact_approval_before_emitting_ok_or_commands() {
        let mut manager = TtyTransferManager::new();
        let request_actions = manager.handle(command(FileTransferAction::Send, "send-1"));
        let request_id = approval(&request_actions).request_id;

        let mut early = command(FileTransferAction::File, "send-1");
        early.file_id = Some("f1".into());
        assert!(matches!(
            manager.handle(early).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
        assert_eq!(manager.active_sessions(), 0);
        assert!(manager.approve(request_id).is_empty());

        let request_actions = manager.handle(command(FileTransferAction::Send, "send-1"));
        let request_id = approval(&request_actions).request_id;
        assert!(matches!(
            manager.approve(request_id).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
        assert_eq!(manager.active_sessions(), 1);
    }

    #[test]
    fn duplicate_start_does_not_invalidate_original_approval() {
        let mut manager = TtyTransferManager::new();
        let request_id =
            approval(&manager.handle(command(FileTransferAction::Send, "duplicate"))).request_id;

        assert!(matches!(
            manager
                .handle(command(FileTransferAction::Send, "duplicate"))
                .as_slice(),
            [TtyTransferAction::Write(_)]
        ));
        assert!(manager.is_approval_pending(request_id));
        assert!(matches!(
            manager.approve(request_id).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
    }

    #[test]
    fn lifecycle_rejection_aborts_approved_staging_and_clears_state() {
        let mut manager = TtyTransferManager::new();
        let request_id =
            approval(&manager.handle(command(FileTransferAction::Send, "idle"))).request_id;
        manager.approve(request_id);

        let actions = manager.reject_session("idle", "ETIMEDOUT:Transfer session idle");

        assert!(matches!(
            actions.as_slice(),
            [TtyTransferAction::Abort { session_id }, TtyTransferAction::Write(_)]
                if session_id == "idle"
        ));
        assert_eq!(manager.active_sessions(), 0);
    }

    #[test]
    fn receive_collects_bounded_paths_before_requesting_consent() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Receive, "receive-1");
        start.size = Some(2);
        assert!(manager.handle(start).is_empty());

        let mut first = command(FileTransferAction::File, "receive-1");
        first.file_id = Some("f1".into());
        first.name = Some("~/one".into());
        assert!(manager.handle(first).is_empty());

        let mut second = command(FileTransferAction::File, "receive-1");
        second.file_id = Some("f2".into());
        second.name = Some("/tmp/two".into());
        let request_actions = manager.handle(second);
        let request = approval(&request_actions);
        assert_eq!(request.direction, TtyTransferDirection::Receive);
        assert_eq!(request.paths, ["~/one", "/tmp/two"]);

        let approved = manager.approve(request.request_id);
        assert!(matches!(
            approved.first(),
            Some(TtyTransferAction::Write(_))
        ));
        assert_eq!(
            approved
                .iter()
                .filter(|action| matches!(action, TtyTransferAction::Execute(_)))
                .count(),
            2
        );
        assert!(matches!(
            approved.last(),
            Some(TtyTransferAction::CompleteReceiveListing { session_id, quiet: 0 })
                if session_id == "receive-1"
        ));
    }

    #[test]
    fn receive_rejects_invalid_paths_and_oversized_request_sets() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Receive, "bad-path");
        start.size = Some(1);
        manager.handle(start);
        let mut path = command(FileTransferAction::File, "bad-path");
        path.file_id = Some("f1".into());
        path.name = Some("../outside".into());
        assert!(matches!(
            manager.handle(path).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
        assert_eq!(manager.active_sessions(), 0);

        let mut oversized = command(FileTransferAction::Receive, "too-many");
        oversized.size = Some((MAX_RECEIVE_PATHS + 1) as u64);
        assert!(matches!(
            manager.handle(oversized).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
    }

    #[test]
    fn ordinary_relative_paths_are_valid_home_relative_protocol_names() {
        let mut manager = TtyTransferManager::new();
        let request_id =
            approval(&manager.handle(command(FileTransferAction::Send, "relative"))).request_id;
        manager.approve(request_id);
        let mut file = command(FileTransferAction::File, "relative");
        file.file_id = Some("f1".into());
        file.name = Some("downloads/report.txt".into());

        assert!(matches!(
            manager.handle(file).as_slice(),
            [TtyTransferAction::Execute(_)]
        ));
    }

    #[test]
    fn send_only_manager_rejects_receive_before_requesting_consent() {
        let mut manager = TtyTransferManager::send_only();
        let mut start = command(FileTransferAction::Receive, "receive-disabled");
        start.size = Some(1);

        assert!(matches!(
            manager.handle(start).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
        assert_eq!(manager.active_sessions(), 0);
    }

    #[test]
    fn denial_and_cancel_honor_quiet_levels() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Send, "quiet-1");
        start.quiet = 1;
        let request_id = approval(&manager.handle(start)).request_id;
        assert!(matches!(
            manager.deny(request_id).as_slice(),
            [TtyTransferAction::Write(_)]
        ));

        let mut silent = command(FileTransferAction::Send, "quiet-2");
        silent.quiet = 2;
        let request_id = approval(&manager.handle(silent)).request_id;
        assert!(manager.deny(request_id).is_empty());

        let mut start = command(FileTransferAction::Send, "cancel");
        start.quiet = 1;
        manager.handle(start);
        assert!(manager
            .handle(command(FileTransferAction::Cancel, "cancel"))
            .is_empty());
    }

    #[test]
    fn stale_approval_token_cannot_authorize_reused_session_id() {
        let mut manager = TtyTransferManager::new();
        let stale = approval(&manager.handle(command(FileTransferAction::Send, "same"))).request_id;
        manager.deny(stale);
        let current =
            approval(&manager.handle(command(FileTransferAction::Send, "same"))).request_id;
        assert_ne!(stale, current);
        assert!(manager.approve(stale).is_empty());
        assert!(matches!(
            manager.approve(current).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
    }

    #[test]
    fn approved_sessions_forward_only_direction_valid_actions() {
        let mut manager = TtyTransferManager::new();
        let request_id =
            approval(&manager.handle(command(FileTransferAction::Send, "send"))).request_id;
        manager.approve(request_id);
        let mut data = command(FileTransferAction::Data, "send");
        data.file_id = Some("f1".into());
        assert!(matches!(
            manager.handle(data).as_slice(),
            [TtyTransferAction::Execute(_)]
        ));
        assert!(matches!(
            manager
                .handle(command(FileTransferAction::Finished, "send"))
                .as_slice(),
            [TtyTransferAction::Abort { .. }, TtyTransferAction::Write(_)]
        ));
        assert_eq!(manager.active_sessions(), 0);
    }

    #[test]
    fn bypass_evaluation_is_default_deny_for_every_kind() {
        let mut credentialed = command(FileTransferAction::Send, "bypass");
        credentialed.bypass = Some("sha256:attacker-supplied".into());
        for kind in [
            TtyTransferBypassKind::None,
            TtyTransferBypassKind::SameTtyPreauthorized,
            TtyTransferBypassKind::LoopbackAutomation,
        ] {
            assert_eq!(
                evaluate_transfer_bypass(&credentialed, kind),
                BypassDecision::Prompt,
                "bypass kind {kind:?} must not authorize without an approved policy"
            );
            assert_eq!(
                evaluate_transfer_bypass(&command(FileTransferAction::Send, "plain"), kind),
                BypassDecision::Prompt
            );
        }
        assert_eq!(
            TtyTransferBypassKind::default(),
            TtyTransferBypassKind::None
        );
    }

    #[test]
    fn wire_bypass_credential_never_skips_the_consent_prompt() {
        let mut manager = TtyTransferManager::new();
        let mut start = command(FileTransferAction::Send, "credentialed");
        start.bypass = Some("sha256:attacker-supplied".into());
        let actions = manager.handle(start);
        // The credential is decoded by the codec but ignored by the state
        // machine: exactly one approval request, no authorization.
        let request_id = approval(&actions).request_id;
        assert!(manager.is_approval_pending(request_id));
        assert!(matches!(
            manager.approve(request_id).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
    }

    #[test]
    fn parser_to_authorization_pipeline_never_executes_before_consent() {
        let mut screen = cterm_core::Screen::new(80, 24, Default::default());
        let mut parser = cterm_core::Parser::new();
        parser.parse(&mut screen, b"\x1b]5113;ac=send;id=pipeline\x1b\\");

        let commands = screen.take_kitty_file_transfer_commands();
        let [command] = commands.as_slice() else {
            panic!("expected one decoded command");
        };
        let mut manager = TtyTransferManager::new();
        let actions = manager.handle(command.clone());
        assert!(!actions
            .iter()
            .any(|action| matches!(action, TtyTransferAction::Execute(_))));

        let request_id = approval(&actions).request_id;
        assert!(matches!(
            manager.approve(request_id).as_slice(),
            [TtyTransferAction::Write(_)]
        ));
    }

    // --- Consent-model mechanism tests (defaults unchanged) ---

    use crate::config::{TransferConfig, TransferConsentMode};
    use crate::transfer_bypass::{
        encode_bypass_credential, encrypt_bypass_credential, hash_bypass_token,
        TransferBypassKeypair,
    };

    const POLICY_PASSWORD: &str = "s3cret";
    const POLICY_NOW_NS: u64 = 1_750_000_000_000_000_000;

    fn policy<'a>(
        config: &'a TransferConfig,
        key: Option<&'a [u8; 32]>,
    ) -> TransferBypassPolicy<'a> {
        TransferBypassPolicy {
            config,
            decryption_key: key,
            now_ns: POLICY_NOW_NS,
        }
    }

    fn start_command(action: FileTransferAction, id: &str) -> FileTransferCommand {
        command(action, id)
    }

    fn credentialed(action: FileTransferAction, id: &str, bypass: &str) -> FileTransferCommand {
        let mut credentialed = command(action, id);
        credentialed.bypass = Some(bypass.into());
        credentialed
    }

    /// Out-of-box evaluation is `Prompt` for every kind, direction, and wire
    /// credential: the mechanism changes nothing unless explicitly opted in.
    /// This also pins the legacy seam to the new policy seam under defaults.
    #[test]
    fn policy_defaults_keep_todays_behavior_for_every_kind() {
        let config = TransferConfig::default();
        let policy = policy(&config, None);
        let attacker_credential = encode_bypass_credential("any-session", "attacker-guess");
        for kind in [
            TtyTransferBypassKind::None,
            TtyTransferBypassKind::PasswordCredential,
            TtyTransferBypassKind::SameTtyPreauthorized,
            TtyTransferBypassKind::LoopbackAutomation,
        ] {
            for action in [FileTransferAction::Send, FileTransferAction::Receive] {
                for command in [
                    start_command(action, "plain"),
                    credentialed(action, "any-session", &attacker_credential),
                ] {
                    assert_eq!(
                        evaluate_transfer_bypass_with_policy(&command, kind, &policy),
                        BypassDecision::Prompt,
                        "default policy must prompt for {kind:?} {action:?}"
                    );
                }
            }
        }
        // The legacy seam agrees with the default policy on its kinds.
        for kind in [
            TtyTransferBypassKind::None,
            TtyTransferBypassKind::SameTtyPreauthorized,
            TtyTransferBypassKind::LoopbackAutomation,
        ] {
            assert_eq!(
                evaluate_transfer_bypass(&command(FileTransferAction::Send, "x"), kind),
                BypassDecision::Prompt
            );
        }
    }

    #[test]
    fn direction_deny_hard_refuses_without_prompting() {
        let config = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Deny),
            receive: None,
        };
        let policy = policy(&config, None);
        // Even a *valid* credential cannot escape an explicit deny.
        let valid = encode_bypass_credential("denied-send", POLICY_PASSWORD);
        for kind in [
            TtyTransferBypassKind::None,
            TtyTransferBypassKind::PasswordCredential,
            TtyTransferBypassKind::SameTtyPreauthorized,
            TtyTransferBypassKind::LoopbackAutomation,
        ] {
            assert_eq!(
                evaluate_transfer_bypass_with_policy(
                    &credentialed(FileTransferAction::Send, "denied-send", &valid),
                    kind,
                    &policy
                ),
                BypassDecision::Deny,
                "deny must win for {kind:?}"
            );
            assert_eq!(
                evaluate_transfer_bypass_with_policy(
                    &start_command(FileTransferAction::Send, "denied-send"),
                    kind,
                    &policy
                ),
                BypassDecision::Deny,
                "deny needs no credential for {kind:?}"
            );
        }
        // The unset direction still prompts.
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Receive, "r", &valid),
                TtyTransferBypassKind::PasswordCredential,
                &policy
            ),
            BypassDecision::Prompt
        );
    }

    #[test]
    fn explicit_allow_with_verified_credential_auto_approves() {
        let terminal = TransferBypassKeypair::from_private_bytes([7u8; 32]);
        let config = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let policy = policy(&config, Some(terminal.private_bytes()));
        // sha256 arm.
        let sha_credential = encode_bypass_credential("allow-sha", POLICY_PASSWORD);
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Send, "allow-sha", &sha_credential),
                TtyTransferBypassKind::PasswordCredential,
                &policy
            ),
            BypassDecision::Allow
        );
        // kitty-1 arm.
        let public = terminal.public_bytes();
        let envelope =
            encrypt_bypass_credential("allow-1", POLICY_PASSWORD, &public, POLICY_NOW_NS).unwrap();
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Send, "allow-1", &envelope),
                TtyTransferBypassKind::PasswordCredential,
                &policy
            ),
            BypassDecision::Allow
        );
        // Same credential under kind None stays ignored (today's behavior).
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Send, "allow-sha", &sha_credential),
                TtyTransferBypassKind::None,
                &policy
            ),
            BypassDecision::Prompt
        );
        // The allow does not leak across directions.
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Receive, "allow-sha", &sha_credential),
                TtyTransferBypassKind::PasswordCredential,
                &policy
            ),
            BypassDecision::Prompt
        );
    }

    #[test]
    fn allow_requires_every_opt_in_element() {
        let terminal = TransferBypassKeypair::from_private_bytes([7u8; 32]);
        let public = terminal.public_bytes();
        let valid = encode_bypass_credential("allow-x", POLICY_PASSWORD);
        let envelope =
            encrypt_bypass_credential("allow-x", POLICY_PASSWORD, &public, POLICY_NOW_NS).unwrap();
        let allow_no_password = TransferConfig {
            bypass_password: None,
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let allow_empty_password = TransferConfig {
            bypass_password: Some(String::new()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let allow_with_password = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let unset_with_password = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: None,
            receive: None,
        };
        let ask_with_password = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Ask),
            receive: None,
        };
        // (config, key, credential): every row must prompt.
        type OptInRow<'a> = (TransferConfig, Option<&'a [u8; 32]>, Option<String>);
        let rows: Vec<OptInRow<'_>> = vec![
            (allow_no_password.clone(), None, Some(valid.clone())),
            (allow_empty_password.clone(), None, Some(valid.clone())),
            // No key fails the kitty-1 arm closed (the sha256 arm needs no
            // key, so this row uses the envelope).
            (allow_with_password.clone(), None, Some(envelope.clone())),
            (
                allow_with_password.clone(),
                Some(terminal.private_bytes()),
                None,
            ),
            (
                allow_with_password.clone(),
                Some(terminal.private_bytes()),
                Some(encode_bypass_credential("allow-x", "wrong")),
            ),
            (unset_with_password.clone(), None, Some(valid.clone())),
            (ask_with_password.clone(), None, Some(valid.clone())),
        ];
        for (config, key, credential) in &rows {
            let mut candidate = start_command(FileTransferAction::Send, "allow-x");
            candidate.bypass = credential.clone();
            assert_eq!(
                evaluate_transfer_bypass_with_policy(
                    &candidate,
                    TtyTransferBypassKind::PasswordCredential,
                    &policy(config, *key)
                ),
                BypassDecision::Prompt,
                "must prompt for config {config:?}"
            );
        }
        // The kitty-1 envelope without its decryption key prompts too.
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Send, "allow-x", &envelope),
                TtyTransferBypassKind::PasswordCredential,
                &policy(&allow_with_password, None)
            ),
            BypassDecision::Prompt
        );
    }

    #[test]
    fn non_start_commands_never_bypass() {
        let terminal = TransferBypassKeypair::from_private_bytes([7u8; 32]);
        let config = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: Some(TransferConsentMode::Allow),
        };
        let policy = policy(&config, Some(terminal.private_bytes()));
        for action in [
            FileTransferAction::File,
            FileTransferAction::Data,
            FileTransferAction::EndData,
            FileTransferAction::Finish,
            FileTransferAction::Finished,
            FileTransferAction::Cancel,
            FileTransferAction::Status,
        ] {
            let mut candidate = command(action, "allow-x");
            candidate.bypass = Some(encode_bypass_credential("allow-x", POLICY_PASSWORD));
            // Direction allow plus a valid credential still prompts: bypass is
            // only evaluated for session starts.
            assert_eq!(
                evaluate_transfer_bypass_with_policy(
                    &candidate,
                    TtyTransferBypassKind::PasswordCredential,
                    &policy
                ),
                BypassDecision::Prompt,
                "must prompt for {action:?}"
            );
        }
    }

    #[test]
    fn stale_kitty1_envelope_prompts() {
        let terminal = TransferBypassKeypair::from_private_bytes([7u8; 32]);
        let public = terminal.public_bytes();
        let config = TransferConfig {
            bypass_password: Some(POLICY_PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let stale = encrypt_bypass_credential("allow-x", POLICY_PASSWORD, &public, 0).unwrap();
        assert_eq!(
            evaluate_transfer_bypass_with_policy(
                &credentialed(FileTransferAction::Send, "allow-x", &stale),
                TtyTransferBypassKind::PasswordCredential,
                &policy(&config, Some(terminal.private_bytes()))
            ),
            BypassDecision::Prompt
        );
    }

    #[test]
    fn transfer_consent_audit_line_carries_every_field_and_no_secrets() {
        let token_hash = hash_bypass_token("kitty-1:sensitive-envelope");
        let row = TransferConsentAudit {
            request_id: Some(41),
            transfer_id: Some("allow-1"),
            direction: Some(TtyTransferDirection::Send),
            decision: TransferConsentDecision::Allow,
            bypass_kind: TtyTransferBypassKind::PasswordCredential,
            token_hash: Some(&token_hash),
        }
        .line();
        for expected in [
            "decision=allow",
            "request_id=41",
            "transfer_id=allow-1",
            "direction=send",
            "bypass=PasswordCredential",
            &format!("token_hash={token_hash}"),
        ] {
            assert!(
                row.contains(expected),
                "audit row {row:?} misses {expected}"
            );
        }
        assert!(
            !row.contains("sensitive-envelope"),
            "audit row leaks the token: {row:?}"
        );
        assert!(
            !row.contains(POLICY_PASSWORD),
            "audit row leaks the password: {row:?}"
        );
        // Dialog-style rows format with the human outcome and no token hash.
        let dialog = TransferConsentAudit {
            request_id: Some(7),
            transfer_id: Some("plain"),
            direction: Some(TtyTransferDirection::Receive),
            decision: TransferConsentDecision::Approved,
            bypass_kind: TtyTransferBypassKind::None,
            token_hash: None,
        }
        .line();
        assert!(dialog.contains("decision=approved"), "{dialog:?}");
        assert!(dialog.contains("direction=receive"), "{dialog:?}");
        assert!(dialog.contains("token_hash=-"), "{dialog:?}");
        // Unknown sessions degrade to dashes instead of panicking.
        let unknown = TransferConsentAudit {
            request_id: Some(9),
            transfer_id: None,
            direction: None,
            decision: TransferConsentDecision::Expired,
            bypass_kind: TtyTransferBypassKind::None,
            token_hash: None,
        }
        .line();
        assert!(unknown.contains("transfer_id=-"), "{unknown:?}");
        assert!(unknown.contains("direction=-"), "{unknown:?}");
    }

    #[test]
    fn pending_request_info_supports_audit_without_touching_auth() {
        let mut manager = TtyTransferManager::new();
        let request_id =
            approval(&manager.handle(command(FileTransferAction::Send, "audit"))).request_id;
        assert_eq!(
            manager.pending_request_info(request_id),
            Some(("audit".to_string(), TtyTransferDirection::Send))
        );
        assert_eq!(manager.pending_request_info(0xDEAD), None);
        // Resolving the request spends the token for audit too.
        manager.deny(request_id);
        assert_eq!(manager.pending_request_info(request_id), None);
    }
}
