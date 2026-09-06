//! Source-side (outbound) state for Kitty's OSC 72 drag-and-drop protocol.
//!
//! While [`crate::kitty_dnd::DndDestination`] tracks drops *into* the
//! terminal, this module tracks drags *out of* terminal content: a terminal
//! application registers itself as a drag source (`t=o:x=1`), the frontend
//! reports a user drag gesture (`t=o` with a location), the application
//! answers with an ordered MIME offer plus optional pre-sent data and drag
//! images, and the frontend runs the resulting OS drag session while
//! reporting progress (`t=e`) back to the application.
//!
//! Progress/finish split (spec dnd-protocol.rst:358-376, kitty/dnd.c:1861-1886):
//! `t=e:x=1:y` (accepted), `t=e:x=2:o=` (action-changed hint),
//! `t=e:x=3` (dropped, data requests may follow), `t=e:x=4:y` (finished,
//! terminal — `y=1` canceled), `t=e:x=5:y` (request MIME data). Copy/move
//! finality on the source side comes via `x=2` hints only; the final
//! operation lives destination-side as `t=r:o=` (spec 137-145).
//!
//! Security posture (mirrors the OSC 5113 exact-token consent philosophy):
//!
//! * An OS drag is never started without a genuine user gesture observed by
//!   the frontend. Application offers that arrive with no live gesture are
//!   refused with `t=E;EPERM` and change no state.
//! * `move` is only ever *reported* (`t=e:x=2:o=2`); the terminal never
//!   deletes local data on behalf of the application.
//! * Kitty has no prompt anywhere on the drag-source path (zero
//!   confirm/prompt calls in `kitty/dnd.c`): local-vs-remote only steers the
//!   data path (`is_remote_client` at dnd.c:1538-1541, remote must pre-send
//!   uri-list at 1839-1843, auto-request remote files at 2051-2056; spec
//!   dnd-protocol.rst:202-206, 404-428). Gating different-machine `t=k`
//!   entry reads behind exact-token approval is deliberate hardening beyond
//!   Kitty: same-machine drags proceed without prompting, while
//!   different-machine and unknown-locality peers (fail-closed as remote)
//!   require [`DndSource::approve`] or [`DndSourcePolicy::allow_remote_reads`].
//!   Frontends without a consent prompt must call [`DndSource::deny`], which
//!   aborts the drag.
//! * The terminal never opens application-supplied local paths directly; URI
//!   entries are served only from application-supplied bytes (pre-sent
//!   `t=p` data or consented `t=k` responses). Local-direct reads are an
//!   explicit non-goal until a path-consent story exists.
//! * Capability queries (`t=q`) are answered by the destination side only;
//!   this module deliberately emits no `t=q` response so frontends cannot
//!   double-answer.

use std::collections::{HashMap, HashSet};

use cterm_core::{DndCommand, DndCommandType, MAX_DND_CHUNK_BYTES};

use crate::kitty_dnd::{DndLocation, DndOperation, MAX_DND_MIME_LIST_BYTES, URI_LIST_MIME};

/// Pre-sent application data is accepted up to 64 MiB, the minimum the Kitty
/// specification requires terminals to accept.
pub const MAX_SOURCE_PRESENT_BYTES: usize = 64 * 1024 * 1024;
/// Bound on drag images retained per offer (Denial-of-service limit).
pub const MAX_SOURCE_IMAGES: usize = 16;
/// Bound on outstanding per-MIME data requests (`t=e:x=5`) per drag.
pub const MAX_PENDING_DATA_REQUESTS: usize = 64;
/// Bound on outstanding remote (`t=k`) entry requests per drag.
pub const MAX_PENDING_REMOTE_REQUESTS: usize = 64;
/// Bound on MIME types in one application offer, mirroring the destination.
const MAX_OFFER_MIME_TYPES: usize = 4096;
/// HMAC key for machine-id hashing (Kitty specification, fixed ASCII bytes).
const MACHINE_ID_HMAC_KEY: &[u8] = b"tty-dnd-protocol-machine-id";

/// Policy gate for the exfiltration-sensitive source paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DndSourcePolicy {
    /// Whether the terminal may request remote entry bytes (`t=k`) without
    /// per-entry approval. `false` (default) is fail-closed: every entry
    /// request surfaces as [`DndSourceAction::RequestApproval`].
    pub allow_remote_reads: bool,
    /// Cap on retained pre-sent + on-demand bytes per drag.
    pub max_present_bytes: usize,
}

impl Default for DndSourcePolicy {
    fn default() -> Self {
        Self {
            allow_remote_reads: false,
            max_present_bytes: MAX_SOURCE_PRESENT_BYTES,
        }
    }
}

/// Negotiation phase of one terminal surface's drag-source registration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SourcePhase {
    #[default]
    Idle,
    /// Application armed (`t=o:x=1`); waiting for a user gesture.
    Armed,
    /// Gesture reported to the application; waiting for its MIME offer.
    GestureSent,
    /// Offer accepted; collecting pre-sent data/images, awaiting `t=P:x=-1`.
    Offered,
    /// OS drag session live; progress events flowing.
    Dragging,
}

/// One fully received pre-sent MIME value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedMime {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Image format advertised by the application (`y` key of `t=p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragImageFormat {
    Rgb24,
    Rgba32,
    Png,
    Text,
}

/// One retained drag image (negative `t=p` index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragImage {
    pub format: DragImageFormat,
    pub width: i32,
    pub height: i32,
    /// Background opacity as `o/1024` (text images only).
    pub opacity: u32,
    pub data: Vec<u8>,
}

/// What the frontend should offer to the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsDragOffer {
    pub mime_types: Vec<String>,
    pub operation: DndOperation,
    pub location: DndLocation,
    pub has_image: bool,
}

/// Reference to one remote URI-list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteEntryRef {
    /// 1-based index into the `text/uri-list` entries.
    pub entry: i32,
    /// 1-based index within the parent directory (`0` for top-level).
    pub num: i32,
    /// Parent directory handle (`0` for top-level).
    pub parent: i32,
}

/// Kind of a completed remote entry, from the `X` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteEntryKind {
    File,
    Symlink,
    Directory,
}

/// Exact-token approval request for one remote entry read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReadApprovalRequest {
    pub request_id: u64,
    pub entry: RemoteEntryRef,
    pub client_id: u32,
}

/// Work for a native frontend after processing a source-side PTY command or
/// OS drag event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DndSourceAction {
    /// Write one complete OSC 72 frame to the PTY.
    Write(Vec<u8>),
    /// Start a native OS drag session with this offer, then confirm with
    /// [`DndSource::os_drag_started`] or abort with
    /// [`DndSource::os_drag_failed`].
    BeginOsDrag(OsDragOffer),
    /// Tear down the live native OS drag session.
    CancelOsDrag,
    /// Complete bytes the OS drop target asked for.
    OsDataReady {
        mime_index: Option<usize>,
        entry: Option<RemoteEntryRef>,
        kind: RemoteEntryKind,
        data: Vec<u8>,
        /// Null-separated directory listing (directory entries only).
        dir_entries: Vec<Vec<u8>>,
    },
    /// One in-flight data request failed; the OS session stays live.
    OsDataFailed {
        mime_index: Option<usize>,
        entry: Option<RemoteEntryRef>,
    },
    /// Remote entry bytes need explicit user consent; approve or deny the
    /// exact token (mirrors the OSC 5113 approval flow).
    RequestApproval(RemoteReadApprovalRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRemoteRead {
    entry: RemoteEntryRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IngressKey {
    entry: i32,
    num: i32,
    parent: i32,
}

#[derive(Debug, Default)]
struct IngressBuffer {
    kind_x: i32,
    data: Vec<u8>,
    discarding: bool,
}

/// Source-side OSC 72 state shared by all native frontends.
#[derive(Debug)]
pub struct DndSource {
    policy: DndSourcePolicy,
    phase: SourcePhase,
    client_id: u32,
    /// Peer machine id as sent with `t=o:x=1` (unhashed wire value).
    peer_machine_id: Option<String>,
    offered_mimes: Vec<String>,
    offered_operation: DndOperation,
    offer_payload: Vec<u8>,
    offer_chunking: bool,
    offer_discarding: bool,
    presented: Vec<Option<PresentedMime>>,
    present_buffers: HashMap<usize, Vec<u8>>,
    present_total: usize,
    image_buffers: HashMap<u32, Vec<u8>>,
    image_meta: HashMap<u32, (DragImageFormat, i32, i32, u32)>,
    images: Vec<DragImage>,
    gesture: Option<DndLocation>,
    gesture_live: bool,
    reported_action: DndOperation,
    pending_data: HashSet<u32>,
    data_buffers: HashMap<u32, Vec<u8>>,
    pending_remote: HashMap<IngressKey, bool>,
    remote_ingress: HashMap<IngressKey, IngressBuffer>,
    approvals: HashMap<u64, PendingRemoteRead>,
    next_request_id: u64,
}

impl Default for DndSource {
    fn default() -> Self {
        Self::with_policy(DndSourcePolicy::default())
    }
}

impl DndSource {
    pub fn with_policy(policy: DndSourcePolicy) -> Self {
        Self {
            policy,
            phase: SourcePhase::Idle,
            client_id: 0,
            peer_machine_id: None,
            offered_mimes: Vec::new(),
            offered_operation: DndOperation::None,
            offer_payload: Vec::new(),
            offer_chunking: false,
            offer_discarding: false,
            presented: Vec::new(),
            present_buffers: HashMap::new(),
            present_total: 0,
            image_buffers: HashMap::new(),
            image_meta: HashMap::new(),
            images: Vec::new(),
            gesture: None,
            gesture_live: false,
            reported_action: DndOperation::None,
            pending_data: HashSet::new(),
            data_buffers: HashMap::new(),
            pending_remote: HashMap::new(),
            remote_ingress: HashMap::new(),
            approvals: HashMap::new(),
            next_request_id: 1,
        }
    }

    /// Whether an application currently offers drags on this surface.
    pub fn is_armed(&self) -> bool {
        !matches!(self.phase, SourcePhase::Idle)
    }

    /// Whether remote (`t=k`) entry reads on this surface currently skip
    /// per-entry approval. `false` (default) is fail-closed.
    pub fn allow_remote_reads(&self) -> bool {
        self.policy.allow_remote_reads
    }

    /// Record an explicit user opt-in ("Always allow" in the native remote-read
    /// prompt) for this surface. Scoped to this `DndSource` only — it survives
    /// individual drags (like the clipboard adapter's remembered exact token)
    /// and clears when the surface goes away — and gated to remote reads:
    /// same-machine drags never consult this flag because they never prompt.
    /// While set, different-machine and unknown-locality entry reads stop
    /// surfacing [`DndSourceAction::RequestApproval`] and proceed directly to
    /// `t=k`. Frontends must state that scope in the prompt body.
    pub fn set_allow_remote_reads(&mut self, allow: bool) {
        self.policy.allow_remote_reads = allow;
    }

    /// Whether an OS drag session is currently live.
    pub fn is_dragging(&self) -> bool {
        self.phase == SourcePhase::Dragging
    }

    /// MIME types of the accepted offer (empty outside `Offered`/`Dragging`).
    pub fn offered_mimes(&self) -> &[String] {
        &self.offered_mimes
    }

    /// Operation flags the application offered (copy/move/either).
    pub fn offered_operation(&self) -> DndOperation {
        self.offered_operation
    }

    /// Complete pre-sent values, for native content providers.
    pub fn presented_snapshot(&self) -> Vec<PresentedMime> {
        self.presented.iter().filter_map(Clone::clone).collect()
    }

    /// Whether this surface may serve `text/uri-list` from retained data.
    pub fn has_uri_list(&self) -> bool {
        self.offered_mimes.iter().any(|mime| mime == URI_LIST_MIME)
            && self
                .presented
                .iter()
                .flatten()
                .any(|item| item.mime_type == URI_LIST_MIME)
    }

    /// Apply one validated source-side command from the terminal application.
    pub fn handle_command(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        match command.command_type {
            DndCommandType::OfferDrag => self.handle_offer(command),
            DndCommandType::PresentData => self.handle_present(command),
            DndCommandType::ChangeDragImage => self.handle_change_image(command),
            DndCommandType::DragOfferEvent => self.handle_client_data(command),
            DndCommandType::DragOfferError => self.handle_client_error(command),
            DndCommandType::UriListData => self.handle_remote_data(command),
            _ => Vec::new(),
        }
    }

    /// Report a user drag-start gesture. Emits the `t=o` location event the
    /// application needs before it may offer MIME types. Only valid while
    /// armed; otherwise the gesture belongs to selection or mouse tracking.
    pub fn user_gesture(&mut self, location: DndLocation) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Armed {
            return Vec::new();
        }
        self.phase = SourcePhase::GestureSent;
        self.gesture = Some(location);
        self.gesture_live = true;
        write_actions(
            self.client_id,
            DndCommandType::OfferDrag,
            None,
            Some(location),
            &[],
        )
    }

    /// The user released or cancelled the gesture before the drag started.
    /// A late application offer is then refused with `EPERM`.
    pub fn gesture_ended(&mut self) {
        if self.phase == SourcePhase::GestureSent {
            self.gesture_live = false;
        }
    }

    /// The frontend started the native OS drag session; confirm to the app.
    pub fn os_drag_started(&mut self) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Offered {
            return Vec::new();
        }
        self.phase = SourcePhase::Dragging;
        self.reported_action = DndOperation::None;
        ok_actions(self.client_id)
    }

    /// The frontend could not start the native OS drag session.
    pub fn os_drag_failed(&mut self, code: &str) -> Vec<DndSourceAction> {
        if !matches!(self.phase, SourcePhase::Offered | SourcePhase::Dragging) {
            return Vec::new();
        }
        let actions = error_actions(self.client_id, None, code);
        self.abort_to_armed();
        finish_after_error(actions, true)
    }

    /// The OS accepted the drag for one MIME type (zero-based index).
    pub fn os_accepted(&mut self, mime_index: usize) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        let Ok(index) = u32::try_from(mime_index) else {
            return Vec::new();
        };
        let mut frame = Vec::with_capacity(self.offered_mimes.len().saturating_add(64));
        frame.extend_from_slice(b"\x1b]72;");
        frame.extend_from_slice(b"t=e:x=1");
        append_i32(&mut frame, b'y', i32::try_from(index).unwrap_or(i32::MAX));
        if self.client_id != 0 {
            append_u32(&mut frame, b'i', self.client_id);
        }
        frame.push(b';');
        frame.extend_from_slice(b"\x1b\\");
        vec![DndSourceAction::Write(frame)]
    }

    /// The OS-reported action changed; forwarded only on change so the
    /// application learns an accurate copy/move outcome. This `t=e:x=2:o=`
    /// hint is the only source-side copy/move signal; finality lives
    /// destination-side as `t=r:o=`.
    pub fn os_action_changed(&mut self, action: DndOperation) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        let normalized = match action {
            DndOperation::Copy | DndOperation::Move => action,
            _ => return Vec::new(),
        };
        if normalized == self.reported_action {
            return Vec::new();
        }
        self.reported_action = normalized;
        write_actions(
            self.client_id,
            DndCommandType::DragOfferEvent,
            Some(normalized),
            Some(DndLocation {
                cell_x: 2,
                ..DndLocation::default()
            }),
            &[],
        )
    }

    /// The OS reports the drop happened; data requests may follow.
    pub fn os_dropped(&mut self) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        write_actions(
            self.client_id,
            DndCommandType::DragOfferEvent,
            None,
            Some(DndLocation {
                cell_x: 3,
                ..DndLocation::default()
            }),
            &[],
        )
    }

    /// Serve bytes the OS drop target asked for (zero-based MIME index).
    /// Pre-sent data is served locally; anything else is requested from the
    /// application with `t=e:x=5`.
    pub fn os_request_data(&mut self, mime_index: usize) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging || mime_index >= self.presented.len() {
            return Vec::new();
        }
        if let Some(Some(item)) = self.presented.get(mime_index) {
            let item = item.clone();
            return vec![DndSourceAction::OsDataReady {
                mime_index: Some(mime_index),
                entry: None,
                kind: RemoteEntryKind::File,
                data: item.data,
                dir_entries: Vec::new(),
            }];
        }
        let Ok(index) = u32::try_from(mime_index) else {
            return Vec::new();
        };
        if self.pending_data.contains(&index) {
            return Vec::new();
        }
        if self.pending_data.len() >= MAX_PENDING_DATA_REQUESTS {
            return self.abort_with("EMFILE");
        }
        self.pending_data.insert(index);
        let mut frame = Vec::with_capacity(64);
        frame.extend_from_slice(b"\x1b]72;");
        frame.extend_from_slice(b"t=e:x=5");
        append_i32(&mut frame, b'y', i32::try_from(index).unwrap_or(i32::MAX));
        if self.client_id != 0 {
            append_u32(&mut frame, b'i', self.client_id);
        }
        frame.push(b';');
        frame.extend_from_slice(b"\x1b\\");
        vec![DndSourceAction::Write(frame)]
    }

    /// Request bytes for one `text/uri-list` entry. Same-machine drags
    /// proceed without prompting (Kitty has no drag-source prompt); only
    /// different-machine and unknown-locality (fail-closed as remote) reads
    /// are gated behind exact-token approval unless the policy allows them.
    pub fn os_request_remote_entry(&mut self, entry: RemoteEntryRef) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        let key = IngressKey {
            entry: entry.entry,
            num: entry.num,
            parent: entry.parent,
        };
        if self.pending_remote.contains_key(&key) {
            return Vec::new();
        }
        if self.pending_remote.len() >= MAX_PENDING_REMOTE_REQUESTS {
            return self.abort_with("EMFILE");
        }
        if self.policy.allow_remote_reads || self.is_same_machine() {
            self.pending_remote.insert(key, true);
            return vec![DndSourceAction::Write(remote_request_frame(
                self.client_id,
                entry,
            ))];
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.pending_remote.insert(key, false);
        self.approvals
            .insert(request_id, PendingRemoteRead { entry });
        vec![DndSourceAction::RequestApproval(
            RemoteReadApprovalRequest {
                request_id,
                entry,
                client_id: self.client_id,
            },
        )]
    }

    /// Approve the exact pending remote-read token (explicit user consent).
    pub fn approve(&mut self, request_id: u64) -> Vec<DndSourceAction> {
        let Some(pending) = self.approvals.remove(&request_id) else {
            return Vec::new();
        };
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        let key = IngressKey {
            entry: pending.entry.entry,
            num: pending.entry.num,
            parent: pending.entry.parent,
        };
        self.pending_remote.insert(key, true);
        vec![DndSourceAction::Write(remote_request_frame(
            self.client_id,
            pending.entry,
        ))]
    }

    /// Refuse the exact pending remote-read token; the drag aborts
    /// fail-closed because the protocol offers no per-entry terminal error.
    pub fn deny(&mut self, request_id: u64) -> Vec<DndSourceAction> {
        if self.approvals.remove(&request_id).is_none() {
            return Vec::new();
        }
        self.abort_with("EPERM")
    }

    /// True only when the peer machine id proves same-machine locality.
    /// Unknown peers (absent/unparseable id) and unknown local ids fail
    /// closed toward remote, so those reads stay approval-gated.
    fn is_same_machine(&self) -> bool {
        let Some(peer) = self.peer_machine_id.as_deref() else {
            return false;
        };
        let Some(local) = local_machine_id() else {
            return false;
        };
        !is_remote_peer(Some(&local), peer)
    }

    /// The OS drag finished (`t=e:x=4:y`, terminal: `y=1` canceled, `y=0`
    /// completed). The registration survives for the next gesture; no further
    /// data requests are served after this point.
    pub fn os_finished(&mut self, canceled: bool) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return Vec::new();
        }
        let y = i32::from(canceled);
        let mut frame = Vec::with_capacity(64);
        frame.extend_from_slice(b"\x1b]72;");
        frame.extend_from_slice(b"t=e:x=4");
        append_i32(&mut frame, b'y', y);
        if self.client_id != 0 {
            append_u32(&mut frame, b'i', self.client_id);
        }
        frame.push(b';');
        frame.extend_from_slice(b"\x1b\\");
        let actions = vec![DndSourceAction::Write(frame)];
        self.abort_to_armed();
        actions
    }

    /// Abort the live drag for a terminal-side reason (`t=E;<code>`).
    pub fn os_abort(&mut self, code: &str) -> Vec<DndSourceAction> {
        if !matches!(self.phase, SourcePhase::Offered | SourcePhase::Dragging) {
            return Vec::new();
        }
        self.abort_with(code)
    }

    fn handle_offer(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        // Arm/disarm use the location field `x` as the flag (1/2), not as a
        // coordinate; an offer carries `o=flags` plus a MIME payload.
        if command.operation != 0 {
            return self.handle_offer_list(command);
        }
        match command.cell_x {
            1 => {
                // `t=o:x=1` carries the optional machine id as its payload
                // (spec dnd-protocol.rst:268-272); it must not be rejected.
                self.phase = SourcePhase::Armed;
                self.client_id = command.client_id;
                // The machine id is optional; an absent or malformed value
                // simply classifies the peer as remote (fail-closed).
                self.peer_machine_id = parse_machine_id(&command.payload).map(str::to_owned);
                // Re-arming drops any stale offer state but keeps nothing else.
                self.clear_offer();
                Vec::new()
            }
            2 => {
                if !command.payload.is_empty() {
                    return Vec::new();
                }
                self.reset();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_offer_list(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::GestureSent {
            // No gesture was reported (or the drag already moved on): refuse
            // without changing state so a stray offer cannot arm an OS drag.
            return error_actions(self.client_id, None, "EPERM");
        }
        if !self.gesture_live {
            self.phase = SourcePhase::Armed;
            return error_actions(self.client_id, None, "EPERM");
        }
        let operation = match command.operation {
            1 => DndOperation::Copy,
            2 => DndOperation::Move,
            3 => DndOperation::Either,
            _ => {
                self.phase = SourcePhase::Armed;
                return error_actions(self.client_id, None, "EINVAL");
            }
        };
        if !self.offer_chunking && !self.offer_discarding {
            self.offer_payload.clear();
            self.offered_operation = operation;
        }
        if self.offer_discarding {
            self.offer_chunking = command.more;
            self.offer_discarding = command.more;
            if command.more {
                return Vec::new();
            }
            self.phase = SourcePhase::Armed;
            return error_actions(self.client_id, None, "EFBIG");
        }
        if self.offer_payload.len() + command.payload.len() > MAX_DND_MIME_LIST_BYTES {
            self.offer_payload.clear();
            self.offer_chunking = command.more;
            self.offer_discarding = command.more;
            if command.more {
                return Vec::new();
            }
            self.phase = SourcePhase::Armed;
            return error_actions(self.client_id, None, "EFBIG");
        }
        self.offer_payload.extend_from_slice(&command.payload);
        self.offer_chunking = command.more;
        if command.more {
            return Vec::new();
        }
        let mime_types = match parse_mime_payload(&self.offer_payload) {
            Ok(mime_types) if !mime_types.is_empty() => mime_types,
            _ => {
                self.phase = SourcePhase::Armed;
                return error_actions(self.client_id, None, "EINVAL");
            }
        };
        self.offered_mimes = mime_types;
        self.presented = vec![None; self.offered_mimes.len()];
        self.phase = SourcePhase::Offered;
        Vec::new()
    }

    fn handle_present(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Offered {
            return error_actions(self.client_id, None, "EINVAL");
        }
        if command.cell_x >= 0 {
            return self.handle_present_mime(command);
        }
        self.handle_present_image(command)
    }

    fn handle_present_mime(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        let Ok(index) = usize::try_from(command.cell_x) else {
            return self.abort_with("EINVAL");
        };
        if index >= self.offered_mimes.len() || self.presented[index].is_some() {
            return self.abort_with("EINVAL");
        }
        let buffer = self.present_buffers.entry(index).or_default();
        if buffer.len() + command.payload.len() + self.present_total > self.policy.max_present_bytes
        {
            return self.abort_with("EFBIG");
        }
        let decoded = match base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &command.payload,
        )
        .or_else(|_| {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &command.payload)
        }) {
            Ok(decoded) => decoded,
            Err(_) => return self.abort_with("EINVAL"),
        };
        if buffer.len() + decoded.len() + self.present_total > self.policy.max_present_bytes {
            return self.abort_with("EFBIG");
        }
        buffer.extend_from_slice(&decoded);
        if command.more {
            if buffer.len() + self.present_total > self.policy.max_present_bytes {
                return self.abort_with("EFBIG");
            }
            return Vec::new();
        }
        let data = self.present_buffers.remove(&index).unwrap_or_default();
        self.present_total += data.len();
        self.presented[index] = Some(PresentedMime {
            mime_type: self.offered_mimes[index].clone(),
            data,
        });
        Vec::new()
    }

    fn handle_present_image(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        // Negative indices arrive consecutively starting at -1; the parser
        // exposes them through the signed location field.
        let sequence = u32::try_from(command.cell_x.abs()).unwrap_or(u32::MAX);
        if sequence == 0
            || sequence != self.images.len() as u32 + self.image_buffers.len() as u32 + 1
            || self.images.len() + self.image_buffers.len() >= MAX_SOURCE_IMAGES
        {
            return self.abort_with("EINVAL");
        }
        let format = match command.cell_y {
            24 => DragImageFormat::Rgb24,
            32 => DragImageFormat::Rgba32,
            100 => DragImageFormat::Png,
            0 => DragImageFormat::Text,
            _ => return self.abort_with("EINVAL"),
        };
        if command.pixel_x < 0 || command.pixel_y < 0 {
            return self.abort_with("EINVAL");
        }
        let first = !self.image_buffers.contains_key(&sequence);
        if first {
            if command.pixel_x == 0 || command.pixel_y == 0 {
                return self.abort_with("EINVAL");
            }
            self.image_meta.insert(
                sequence,
                (format, command.pixel_x, command.pixel_y, command.operation),
            );
        } else if self.image_meta.get(&sequence).is_some_and(|meta| {
            meta.0 != format || meta.1 != command.pixel_x || meta.2 != command.pixel_y
        }) {
            return self.abort_with("EINVAL");
        }
        let buffer = self.image_buffers.entry(sequence).or_default();
        if buffer.len() + command.payload.len() + self.present_total > self.policy.max_present_bytes
        {
            return self.abort_with("EFBIG");
        }
        buffer.extend_from_slice(&command.payload);
        if command.more {
            return Vec::new();
        }
        let Some((format, width, height, opacity)) = self.image_meta.remove(&sequence) else {
            return self.abort_with("EINVAL");
        };
        let data = self.image_buffers.remove(&sequence).unwrap_or_default();
        let decoded =
            match base64::Engine::decode(&base64::engine::general_purpose::STANDARD_NO_PAD, &data)
                .or_else(|_| {
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
                }) {
                Ok(decoded) => decoded,
                Err(_) => return self.abort_with("EINVAL"),
            };
        if let Err(code) = validate_image(&format, width, height, &decoded) {
            return self.abort_with(code);
        }
        if decoded.len() + self.present_total > self.policy.max_present_bytes {
            return self.abort_with("EFBIG");
        }
        self.present_total += decoded.len();
        self.images.push(DragImage {
            format,
            width,
            height,
            opacity,
            data: decoded,
        });
        Vec::new()
    }

    fn handle_change_image(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Offered {
            return error_actions(self.client_id, None, "EINVAL");
        }
        if command.cell_x == -1 {
            return self.request_start_drag();
        }
        // Out-of-bounds selection removes the drag image; anything else
        // selects it for the pending OS session.
        Vec::new()
    }

    fn request_start_drag(&mut self) -> Vec<DndSourceAction> {
        if !self.gesture_live {
            self.phase = SourcePhase::Armed;
            return error_actions(self.client_id, None, "EPERM");
        }
        let offer = OsDragOffer {
            mime_types: self.offered_mimes.clone(),
            operation: self.offered_operation,
            location: self.gesture.unwrap_or_default(),
            has_image: !self.images.is_empty(),
        };
        vec![DndSourceAction::BeginOsDrag(offer)]
    }

    fn handle_client_data(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return error_actions(self.client_id, None, "EINVAL");
        }
        let Ok(index) = u32::try_from(command.cell_y) else {
            return Vec::new();
        };
        if !self.pending_data.contains(&index) {
            return Vec::new();
        }
        let buffer = self.data_buffers.entry(index).or_default();
        if buffer.len() + command.payload.len() + self.present_total > self.policy.max_present_bytes
        {
            self.pending_data.remove(&index);
            self.data_buffers.remove(&index);
            let failed = usize::try_from(index).ok();
            return vec![DndSourceAction::OsDataFailed {
                mime_index: failed,
                entry: None,
            }];
        }
        buffer.extend_from_slice(&command.payload);
        if command.more {
            return Vec::new();
        }
        self.pending_data.remove(&index);
        let data = self.data_buffers.remove(&index).unwrap_or_default();
        let decoded =
            match base64::Engine::decode(&base64::engine::general_purpose::STANDARD_NO_PAD, &data)
                .or_else(|_| {
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
                }) {
                Ok(decoded) => decoded,
                Err(_) => {
                    return vec![DndSourceAction::OsDataFailed {
                        mime_index: usize::try_from(index).ok(),
                        entry: None,
                    }];
                }
            };
        if decoded.len() + self.present_total > self.policy.max_present_bytes {
            return vec![DndSourceAction::OsDataFailed {
                mime_index: usize::try_from(index).ok(),
                entry: None,
            }];
        }
        self.present_total += decoded.len();
        vec![DndSourceAction::OsDataReady {
            mime_index: usize::try_from(index).ok(),
            entry: None,
            kind: RemoteEntryKind::File,
            data: decoded,
            dir_entries: Vec::new(),
        }]
    }

    fn handle_client_error(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        // Split `t=E` semantics, matching Kitty exactly (kitty/screen.c
        // `t=E` arm, kitty/dnd.c:2104-2122,1525-1528; spec dnd-protocol.rst:387-399):
        // valid-index `t=E:y=idx` fails only that request (per-item error,
        // offer kept); `y=-1`/missing/ambiguous index aborts the whole drag.
        if !matches!(
            self.phase,
            SourcePhase::GestureSent | SourcePhase::Offered | SourcePhase::Dragging
        ) {
            return Vec::new();
        }
        if command.cell_y == -1 {
            // The application cancels the whole drag (`t=E:y=-1`).
            let had_drag = self.phase == SourcePhase::Dragging;
            self.abort_to_armed();
            return if had_drag {
                vec![DndSourceAction::CancelOsDrag]
            } else {
                Vec::new()
            };
        }
        if !command.has_cell_y {
            // Missing `y` is ambiguous with index 0 at the wire layer, so it
            // aborts fail-closed instead of failing index 0.
            if self.phase == SourcePhase::Dragging {
                return self.abort_with("EINVAL");
            }
            self.abort_to_armed();
            return error_actions(self.client_id, None, "EINVAL");
        }
        let Ok(index) = u32::try_from(command.cell_y) else {
            // Negative other than -1, or otherwise ambiguous.
            if self.phase == SourcePhase::Dragging {
                return self.abort_with("EINVAL");
            }
            self.abort_to_armed();
            return error_actions(self.client_id, None, "EINVAL");
        };
        let valid = usize::try_from(index).is_ok_and(|idx| idx < self.offered_mimes.len());
        if !valid {
            // Out-of-bounds index aborts the whole drag (dnd.c:2104-2109).
            if self.phase == SourcePhase::Dragging {
                return self.abort_with("EINVAL");
            }
            self.abort_to_armed();
            return error_actions(self.client_id, None, "EINVAL");
        }
        if self.phase == SourcePhase::Dragging {
            // Valid index: fail only that request, keep the offer alive
            // (dnd.c:2112-2122 per-item `-err`).
            self.pending_data.remove(&index);
            self.data_buffers.remove(&index);
            return vec![DndSourceAction::OsDataFailed {
                mime_index: usize::try_from(index).ok(),
                entry: None,
            }];
        }
        self.abort_to_armed();
        error_actions(self.client_id, None, "EINVAL")
    }

    fn handle_remote_data(&mut self, command: DndCommand) -> Vec<DndSourceAction> {
        if self.phase != SourcePhase::Dragging {
            return error_actions(self.client_id, None, "EINVAL");
        }
        let key = IngressKey {
            entry: command.cell_x,
            num: command.cell_y,
            parent: command.pixel_y,
        };
        if !self.pending_remote.contains_key(&key) {
            return Vec::new();
        }
        let first = !self.remote_ingress.contains_key(&key);
        if first {
            self.remote_ingress.insert(
                key,
                IngressBuffer {
                    kind_x: command.pixel_x,
                    ..IngressBuffer::default()
                },
            );
        }
        let Some(buffer) = self.remote_ingress.get_mut(&key) else {
            return Vec::new();
        };
        if buffer.discarding {
            if command.more {
                return Vec::new();
            }
            self.pending_remote.remove(&key);
            self.remote_ingress.remove(&key);
            return vec![DndSourceAction::OsDataFailed {
                mime_index: None,
                entry: Some(RemoteEntryRef {
                    entry: key.entry,
                    num: key.num,
                    parent: key.parent,
                }),
            }];
        }
        if buffer.data.len() + command.payload.len() + self.present_total
            > self.policy.max_present_bytes
        {
            buffer.discarding = true;
            buffer.data.clear();
            if command.more {
                return Vec::new();
            }
            self.pending_remote.remove(&key);
            self.remote_ingress.remove(&key);
            return vec![DndSourceAction::OsDataFailed {
                mime_index: None,
                entry: Some(RemoteEntryRef {
                    entry: key.entry,
                    num: key.num,
                    parent: key.parent,
                }),
            }];
        }
        buffer.data.extend_from_slice(&command.payload);
        if command.more {
            return Vec::new();
        }
        let Some(buffer) = self.remote_ingress.remove(&key) else {
            return Vec::new();
        };
        self.pending_remote.remove(&key);
        let decoded = match base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &buffer.data,
        )
        .or_else(|_| {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &buffer.data)
        }) {
            Ok(decoded) => decoded,
            Err(_) => {
                return vec![DndSourceAction::OsDataFailed {
                    mime_index: None,
                    entry: Some(RemoteEntryRef {
                        entry: key.entry,
                        num: key.num,
                        parent: key.parent,
                    }),
                }];
            }
        };
        if decoded.len() + self.present_total > self.policy.max_present_bytes {
            return vec![DndSourceAction::OsDataFailed {
                mime_index: None,
                entry: Some(RemoteEntryRef {
                    entry: key.entry,
                    num: key.num,
                    parent: key.parent,
                }),
            }];
        }
        self.present_total += decoded.len();
        let (kind, dir_entries) = match buffer.kind_x {
            1 => (RemoteEntryKind::Symlink, Vec::new()),
            0 => (RemoteEntryKind::File, Vec::new()),
            _ => {
                let entries = decoded
                    .split(|byte| *byte == 0)
                    .filter(|entry| !entry.is_empty())
                    .map(<[u8]>::to_vec)
                    .collect();
                (RemoteEntryKind::Directory, entries)
            }
        };
        vec![DndSourceAction::OsDataReady {
            mime_index: None,
            entry: Some(RemoteEntryRef {
                entry: key.entry,
                num: key.num,
                parent: key.parent,
            }),
            kind,
            data: decoded,
            dir_entries,
        }]
    }

    fn abort_with(&mut self, code: &str) -> Vec<DndSourceAction> {
        let actions = error_actions(self.client_id, None, code);
        let had_drag = self.phase == SourcePhase::Dragging;
        self.abort_to_armed();
        finish_after_error(actions, had_drag)
    }

    fn abort_to_armed(&mut self) {
        let client_id = self.client_id;
        let peer_machine_id = self.peer_machine_id.clone();
        let policy = self.policy;
        *self = Self::with_policy(policy);
        // A finished or failed drag keeps the registration: the application
        // stays willing until it sends an explicit `t=o:x=2`. Abort paths
        // only run from armed phases, so Armed is always correct here.
        self.phase = SourcePhase::Armed;
        self.client_id = client_id;
        self.peer_machine_id = peer_machine_id;
    }

    fn clear_offer(&mut self) {
        self.offered_mimes.clear();
        self.offered_operation = DndOperation::None;
        self.offer_payload.clear();
        self.offer_chunking = false;
        self.offer_discarding = false;
        self.presented.clear();
        self.present_buffers.clear();
        self.present_total = 0;
        self.image_buffers.clear();
        self.image_meta.clear();
        self.images.clear();
        self.gesture = None;
        self.gesture_live = false;
        self.reported_action = DndOperation::None;
        self.pending_data.clear();
        self.data_buffers.clear();
        self.pending_remote.clear();
        self.remote_ingress.clear();
        self.approvals.clear();
    }

    fn reset(&mut self) {
        let policy = self.policy;
        *self = Self::with_policy(policy);
    }
}

/// Hash a raw machine id per the Kitty specification: HMAC-SHA256 with the
/// fixed key `tty-dnd-protocol-machine-id`, hex-encoded with a `1:` prefix.
/// Hashing keeps the stable machine identifier out of the scrollback/PTY log.
pub fn hash_machine_id(raw_id: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(MACHINE_ID_HMAC_KEY)
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(raw_id.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut hashed = String::with_capacity(2 + digest.len() * 2);
    hashed.push_str("1:");
    for byte in digest {
        hashed.push_str(&format!("{byte:02x}"));
    }
    hashed
}

/// Read and hash this machine's stable identifier. Unix reads
/// `/etc/machine-id`; other platforms return `None` until native
/// identifiers (IOPlatformUUID, registry MachineGuid) are wired.
pub fn local_machine_id() -> Option<String> {
    #[cfg(unix)]
    {
        let raw = std::fs::read_to_string("/etc/machine-id").ok()?;
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        Some(hash_machine_id(raw))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Whether `peer` (wire value from `t=o:x=1`) names a different machine.
/// Unknown versions and missing local ids fail closed toward remote, so
/// remote reads stay consent-gated instead of silently treated as local.
pub fn is_remote_peer(local_hashed: Option<&str>, peer: &str) -> bool {
    let Some(version) = peer.split(':').next() else {
        return true;
    };
    if version != "1" {
        return true;
    }
    match local_hashed {
        Some(local) => local != peer,
        None => true,
    }
}

fn parse_machine_id(payload: &[u8]) -> Option<&str> {
    if payload.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?;
    // Wire value is `version:printable`; anything else is unusable for a
    // locality comparison and the peer is treated as remote.
    if text.is_empty()
        || !text.is_ascii()
        || text.bytes().any(|byte| byte.is_ascii_control())
        || !text.contains(':')
    {
        return None;
    }
    Some(text)
}

fn validate_image(
    format: &DragImageFormat,
    width: i32,
    height: i32,
    data: &[u8],
) -> Result<(), &'static str> {
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or("EINVAL")?;
    match format {
        DragImageFormat::Rgb24 => {
            if pixels.checked_mul(3).is_some_and(|len| len == data.len()) {
                Ok(())
            } else {
                Err("EINVAL")
            }
        }
        DragImageFormat::Rgba32 => {
            if pixels.checked_mul(4).is_some_and(|len| len == data.len()) {
                Ok(())
            } else {
                Err("EINVAL")
            }
        }
        DragImageFormat::Png | DragImageFormat::Text => {
            if data.is_empty() || data.len() > MAX_SOURCE_PRESENT_BYTES {
                Err("EFBIG")
            } else {
                Ok(())
            }
        }
    }
}

fn parse_mime_payload(payload: &[u8]) -> Result<Vec<String>, ()> {
    let text = std::str::from_utf8(payload).map_err(|_| ())?;
    let mime_types = text
        .split(' ')
        .filter(|mime| !mime.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if mime_types.is_empty() || mime_types.len() > MAX_OFFER_MIME_TYPES {
        return Err(());
    }
    let mut total = 0_usize;
    for mime in &mime_types {
        if mime.is_empty()
            || !mime.is_ascii()
            || mime
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            return Err(());
        }
        total = total
            .checked_add(mime.len() + usize::from(total > 0))
            .ok_or(())?;
    }
    if total > MAX_DND_MIME_LIST_BYTES {
        return Err(());
    }
    Ok(mime_types)
}

fn remote_request_frame(client_id: u32, entry: RemoteEntryRef) -> Vec<u8> {
    let mut frame = Vec::with_capacity(64);
    frame.extend_from_slice(b"\x1b]72;");
    frame.extend_from_slice(b"t=k");
    append_i32(&mut frame, b'x', entry.entry);
    if entry.num != 0 {
        append_i32(&mut frame, b'y', entry.num);
    }
    if entry.parent != 0 {
        append_i32(&mut frame, b'Y', entry.parent);
    }
    if client_id != 0 {
        append_u32(&mut frame, b'i', client_id);
    }
    frame.push(b';');
    frame.extend_from_slice(b"\x1b\\");
    frame
}

fn ok_actions(client_id: u32) -> Vec<DndSourceAction> {
    // The multiplexer id rides in metadata when set; the OK payload stays bare.
    let frame = if client_id == 0 {
        b"\x1b]72;t=E;OK\x1b\\".to_vec()
    } else {
        format!("\x1b]72;t=E:i={client_id};OK\x1b\\").into_bytes()
    };
    vec![DndSourceAction::Write(frame)]
}

fn error_actions(
    client_id: u32,
    location: Option<DndLocation>,
    message: &str,
) -> Vec<DndSourceAction> {
    write_actions(
        client_id,
        DndCommandType::DragOfferError,
        None,
        location,
        message.as_bytes(),
    )
}

fn finish_after_error(mut actions: Vec<DndSourceAction>, had_drag: bool) -> Vec<DndSourceAction> {
    if had_drag {
        actions.push(DndSourceAction::CancelOsDrag);
    }
    actions
}

fn write_actions(
    client_id: u32,
    command_type: DndCommandType,
    operation: Option<DndOperation>,
    location: Option<DndLocation>,
    payload: &[u8],
) -> Vec<DndSourceAction> {
    encode_frames(command_type, client_id, operation, location, payload)
        .into_iter()
        .map(DndSourceAction::Write)
        .collect()
}

fn encode_frames(
    command_type: DndCommandType,
    client_id: u32,
    operation: Option<DndOperation>,
    location: Option<DndLocation>,
    payload: &[u8],
) -> Vec<Vec<u8>> {
    let chunks = payload.len().div_ceil(MAX_DND_CHUNK_BYTES).max(1);
    (0..chunks)
        .map(|index| {
            let start = index * MAX_DND_CHUNK_BYTES;
            let end = (start + MAX_DND_CHUNK_BYTES).min(payload.len());
            let chunk = &payload[start..end];
            let more = index + 1 < chunks;
            let mut frame = Vec::with_capacity(chunk.len() + 96);
            frame.extend_from_slice(b"\x1b]72;");
            if index == 0 {
                frame.extend_from_slice(b"t=");
                frame.push(command_byte(command_type));
                if let Some(operation) = operation {
                    append_u32(&mut frame, b'o', operation_value(operation));
                }
                if let Some(location) = location {
                    append_i32(&mut frame, b'x', location.cell_x);
                    append_i32(&mut frame, b'y', location.cell_y);
                    append_i32(&mut frame, b'X', location.pixel_x);
                    append_i32(&mut frame, b'Y', location.pixel_y);
                }
            }
            if more || index > 0 {
                append_u32(&mut frame, b'm', u32::from(more));
            }
            if client_id != 0 {
                append_u32(&mut frame, b'i', client_id);
            }
            frame.push(b';');
            frame.extend_from_slice(chunk);
            frame.extend_from_slice(b"\x1b\\");
            frame
        })
        .collect()
}

fn command_byte(command_type: DndCommandType) -> u8 {
    match command_type {
        DndCommandType::OfferDrag => b'o',
        DndCommandType::PresentData => b'p',
        DndCommandType::ChangeDragImage => b'P',
        DndCommandType::DragOfferEvent => b'e',
        DndCommandType::DragOfferError => b'E',
        DndCommandType::UriListData => b'k',
        DndCommandType::Query => b'q',
        DndCommandType::AcceptDrops => b'a',
        DndCommandType::StopAcceptingDrops => b'A',
        DndCommandType::DropMove => b'm',
        DndCommandType::Drop => b'M',
        DndCommandType::RequestData => b'r',
        DndCommandType::RequestError => b'R',
    }
}

fn operation_value(operation: DndOperation) -> u32 {
    match operation {
        DndOperation::None => 0,
        DndOperation::Copy => 1,
        DndOperation::Move => 2,
        DndOperation::Either => 3,
    }
}

fn append_u32(frame: &mut Vec<u8>, key: u8, value: u32) {
    frame.push(b':');
    frame.push(key);
    frame.push(b'=');
    frame.extend_from_slice(value.to_string().as_bytes());
}

fn append_i32(frame: &mut Vec<u8>, key: u8, value: i32) {
    frame.push(b':');
    frame.push(key);
    frame.push(b'=');
    frame.extend_from_slice(value.to_string().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn arm(source: &mut DndSource) {
        arm_with_peer(source, None);
    }

    fn arm_with_peer(source: &mut DndSource, peer: Option<&str>) {
        let mut enable = command(DndCommandType::OfferDrag);
        enable.cell_x = 1;
        if let Some(peer) = peer {
            enable.payload = peer.as_bytes().to_vec();
        }
        assert!(source.handle_command(enable).is_empty());
        assert!(source.is_armed());
    }

    fn gesture(source: &mut DndSource) {
        let frames = source.user_gesture(DndLocation {
            cell_x: 4,
            cell_y: 6,
            pixel_x: 40,
            pixel_y: 60,
        });
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            &frames[0],
            DndSourceAction::Write(frame) if frame.starts_with(b"\x1b]72;t=o:x=4:y=6:X=40:Y=60;")
        ));
    }

    fn offer(source: &mut DndSource, operation: u32, mimes: &str) {
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = operation;
        offer.payload = mimes.as_bytes().to_vec();
        assert!(source.handle_command(offer).is_empty());
        assert_eq!(source.phase, SourcePhase::Offered);
    }

    fn start_drag(source: &mut DndSource) {
        let mut start = command(DndCommandType::ChangeDragImage);
        start.cell_x = -1;
        let actions = source.handle_command(start);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::BeginOsDrag(_)]
        ));
        assert!(source.os_drag_started().iter().any(|action| matches!(
            action,
            DndSourceAction::Write(frame) if frame.windows(2).any(|part| part == b"OK")
        )));
        assert!(source.is_dragging());
    }

    fn small_policy() -> DndSourcePolicy {
        DndSourcePolicy {
            allow_remote_reads: false,
            max_present_bytes: 1024,
        }
    }

    #[test]
    fn query_is_never_answered_by_the_source_side() {
        let mut source = DndSource::default();
        let mut query = command(DndCommandType::Query);
        query.client_id = 9;
        assert!(source.handle_command(query).is_empty());
    }

    #[test]
    fn destination_commands_are_ignored() {
        let mut source = DndSource::default();
        for command_type in [
            DndCommandType::AcceptDrops,
            DndCommandType::StopAcceptingDrops,
            DndCommandType::DropMove,
            DndCommandType::Drop,
            DndCommandType::RequestData,
            DndCommandType::RequestError,
        ] {
            assert!(source.handle_command(command(command_type)).is_empty());
        }
        assert!(!source.is_armed());
    }

    #[test]
    fn gesture_without_registration_emits_nothing() {
        let mut source = DndSource::default();
        assert!(source.user_gesture(DndLocation::default()).is_empty());
    }

    #[test]
    fn disarm_resets_registration() {
        let mut source = DndSource::default();
        arm(&mut source);
        let mut disable = command(DndCommandType::OfferDrag);
        disable.cell_x = 2;
        assert!(source.handle_command(disable).is_empty());
        assert!(!source.is_armed());
    }

    #[test]
    fn offer_without_gesture_is_eperm_and_arms_nothing() {
        let mut source = DndSource::default();
        arm(&mut source);
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 3;
        offer.payload = b"text/plain".to_vec();
        let actions = source.handle_command(offer);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(5).any(|part| part == b"EPERM")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn offer_after_dead_gesture_is_eperm() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        source.gesture_ended();
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 1;
        offer.payload = b"text/plain".to_vec();
        let actions = source.handle_command(offer);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(5).any(|part| part == b"EPERM")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn action_flags_map_to_copy_move_either_and_reject_other_values() {
        for (flag, expected) in [
            (1, DndOperation::Copy),
            (2, DndOperation::Move),
            (3, DndOperation::Either),
        ] {
            let mut source = DndSource::default();
            arm(&mut source);
            gesture(&mut source);
            offer(&mut source, flag, "text/plain");
            assert_eq!(source.offered_operation(), expected);
        }
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        let mut offer = command(DndCommandType::OfferDrag);
        offer.operation = 7;
        offer.payload = b"text/plain".to_vec();
        let actions = source.handle_command(offer);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(6).any(|part| part == b"EINVAL")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn oversized_offer_is_efbig_and_rearms() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        // The offer MIME list shares the destination 1 MiB bound, not the
        // smaller pre-sent-data policy cap.
        let mut first = command(DndCommandType::OfferDrag);
        first.operation = 1;
        first.more = true;
        first.payload = vec![b'a'; 700_000];
        assert!(source.handle_command(first).is_empty());
        let mut last = command(DndCommandType::OfferDrag);
        last.operation = 1;
        last.payload = vec![b'b'; 400_000];
        let actions = source.handle_command(last);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(5).any(|part| part == b"EFBIG")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn full_negotiation_reaches_os_drag_with_ok() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 3, "text/plain text/uri-list");
        assert_eq!(
            source.offered_mimes(),
            ["text/plain".to_string(), "text/uri-list".to_string()]
        );
        let mut start = command(DndCommandType::ChangeDragImage);
        start.cell_x = -1;
        let actions = source.handle_command(start);
        match actions.as_slice() {
            [DndSourceAction::BeginOsDrag(offer)] => {
                assert_eq!(offer.operation, DndOperation::Either);
                assert_eq!(offer.mime_types.len(), 2);
                assert!(!offer.has_image);
            }
            other => panic!("expected BeginOsDrag, got {other:?}"),
        }
        start_drag_os_confirm(&mut source);
    }

    fn start_drag_os_confirm(source: &mut DndSource) {
        let actions = source.os_drag_started();
        assert!(actions.iter().any(|action| matches!(
            action,
            DndSourceAction::Write(frame) if frame.windows(2).any(|part| part == b"OK")
        )));
        assert!(source.is_dragging());
    }

    #[test]
    fn move_action_is_reported_on_change_only() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 3, "text/plain");
        start_drag(&mut source);
        let changed = source.os_action_changed(DndOperation::Move);
        assert!(matches!(
            changed.as_slice(),
            [DndSourceAction::Write(frame)]
                if frame.windows(4).any(|part| part == b":o=2")
                    && frame.windows(4).any(|part| part == b":x=2")
        ));
        assert!(source.os_action_changed(DndOperation::Move).is_empty());
        let changed = source.os_action_changed(DndOperation::Copy);
        assert!(matches!(
            changed.as_slice(),
            [DndSourceAction::Write(frame)]
                if frame.windows(4).any(|part| part == b":o=1")
                    && frame.windows(4).any(|part| part == b":x=2")
        ));
        // Either/copy-move placeholders never reach the application.
        assert!(source.os_action_changed(DndOperation::Either).is_empty());
        assert!(source.os_action_changed(DndOperation::None).is_empty());
    }

    #[test]
    fn drop_and_finish_events_carry_the_spec_codes() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        start_drag(&mut source);
        let dropped = source.os_dropped();
        assert!(matches!(
            dropped.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(4).any(|part| part == b":x=3")
        ));
        let finished = source.os_finished(false);
        assert!(matches!(
            finished.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(7).any(|part| part == b"x=4:y=0")
        ));
        // Registration survives: another gesture may follow without re-arm.
        assert_eq!(source.phase, SourcePhase::Armed);
        let canceled = {
            gesture(&mut source);
            offer(&mut source, 1, "text/plain");
            start_drag(&mut source);
            source.os_finished(true)
        };
        assert!(matches!(
            canceled.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(7).any(|part| part == b"x=4:y=1")
        ));
    }

    #[test]
    fn present_data_is_reassembled_and_served_to_the_os() {
        let mut source = DndSource::with_policy(small_policy());
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        let mut first = command(DndCommandType::PresentData);
        first.cell_x = 0;
        first.more = true;
        first.payload = b"aGVsbG8s".to_vec();
        assert!(source.handle_command(first).is_empty());
        let mut last = command(DndCommandType::PresentData);
        last.cell_x = 0;
        last.payload = b"IHdvcmxk".to_vec();
        assert!(source.handle_command(last).is_empty());
        start_drag(&mut source);
        let ready = source.os_request_data(0);
        assert!(matches!(
            ready.as_slice(),
            [DndSourceAction::OsDataReady { data, .. }] if data == b"hello, world"
        ));
    }

    #[test]
    fn present_data_over_policy_cap_aborts_fail_closed() {
        let mut source = DndSource::with_policy(small_policy());
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        let mut present = command(DndCommandType::PresentData);
        present.cell_x = 0;
        present.payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            vec![b'x'; 2048],
        )
        .into_bytes();
        let actions = source.handle_command(present);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(5).any(|part| part == b"EFBIG")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn on_demand_mime_data_round_trips_through_the_app() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain text/html");
        start_drag(&mut source);
        // MIME 1 was never pre-sent: the OS pull becomes a `t=e:x=5` request.
        let request = source.os_request_data(1);
        assert!(matches!(
            request.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(7).any(|part| part == b"x=5:y=1")
        ));
        // Duplicate pulls while outstanding are coalesced, not re-requested.
        assert!(source.os_request_data(1).is_empty());
        let mut data = command(DndCommandType::DragOfferEvent);
        data.cell_y = 1;
        data.payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            b"<b>hi</b>",
        )
        .into_bytes();
        let ready = source.handle_command(data);
        assert!(matches!(
            ready.as_slice(),
            [DndSourceAction::OsDataReady { data, .. }] if data == b"<b>hi</b>"
        ));
    }

    #[test]
    fn stray_client_data_without_request_is_ignored() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        start_drag(&mut source);
        let mut data = command(DndCommandType::DragOfferEvent);
        data.cell_y = 0;
        data.payload = b"aGk=".to_vec();
        assert!(source.handle_command(data).is_empty());
        assert!(source.is_dragging());
    }

    #[test]
    fn remote_entry_request_is_fail_closed_until_approved() {
        // Unknown locality (no machine id) fails closed as remote: approval
        // required. Kitty itself has no drag-source prompt; gating remote
        // reads is deliberate hardening beyond Kitty.
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let entry = RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        };
        let actions = source.os_request_remote_entry(entry);
        let request_id = match actions.as_slice() {
            [DndSourceAction::RequestApproval(request)] => {
                assert_eq!(request.entry, entry);
                request.request_id
            }
            other => panic!("expected approval request, got {other:?}"),
        };
        // Stale tokens authorize nothing.
        assert!(source.approve(request_id + 1000).is_empty());
        // Consent emits the `t=k` read for exactly that entry.
        let approved = source.approve(request_id);
        assert!(matches!(
            approved.as_slice(),
            [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=1;")
        ));
        // The application answers with chunked file bytes.
        let mut first = command(DndCommandType::UriListData);
        first.cell_x = 1;
        first.more = true;
        first.payload = b"aGVsbG8s".to_vec();
        assert!(source.handle_command(first).is_empty());
        let mut last = command(DndCommandType::UriListData);
        last.cell_x = 1;
        last.payload = b"IHdvcmxk".to_vec();
        let ready = source.handle_command(last);
        assert!(matches!(
            ready.as_slice(),
            [DndSourceAction::OsDataReady { kind: RemoteEntryKind::File, data, .. }]
                if data == b"hello, world"
        ));
    }

    #[test]
    fn denied_remote_read_aborts_the_drag() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let entry = RemoteEntryRef {
            entry: 2,
            num: 0,
            parent: 0,
        };
        let actions = source.os_request_remote_entry(entry);
        let request_id = match actions.as_slice() {
            [DndSourceAction::RequestApproval(request)] => request.request_id,
            other => panic!("expected approval request, got {other:?}"),
        };
        let denied = source.deny(request_id);
        assert!(matches!(
            denied.as_slice(),
            [DndSourceAction::Write(_), DndSourceAction::CancelOsDrag]
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
        assert!(source.deny(request_id).is_empty());
    }

    #[test]
    fn policy_allowed_remote_reads_skip_approval() {
        let mut source = DndSource::with_policy(DndSourcePolicy {
            allow_remote_reads: true,
            max_present_bytes: MAX_SOURCE_PRESENT_BYTES,
        });
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let actions = source.os_request_remote_entry(RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        });
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=1;")
        ));
    }

    #[test]
    fn always_allow_opt_in_skips_later_remote_approvals() {
        let mut source = DndSource::default();
        assert!(!source.allow_remote_reads());
        // Unknown-locality peer (no machine id): reads are approval-gated.
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let first = RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        };
        let request_id = match source.os_request_remote_entry(first).as_slice() {
            [DndSourceAction::RequestApproval(request)] => {
                assert_eq!(request.entry, first);
                request.request_id
            }
            other => panic!("unknown-locality t=k must prompt, got {other:?}"),
        };
        // The user picks "Always allow": later entries skip approval...
        source.set_allow_remote_reads(true);
        assert!(source.allow_remote_reads());
        let second = source.os_request_remote_entry(RemoteEntryRef {
            entry: 2,
            num: 0,
            parent: 0,
        });
        assert!(matches!(
            second.as_slice(),
            [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=2;")
        ));
        // ...while the earlier exact token still approves exactly once.
        let approved = source.approve(request_id);
        assert!(matches!(
            approved.as_slice(),
            [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=1;")
        ));
        assert!(source.deny(request_id).is_empty());
    }

    #[test]
    fn same_machine_remote_reads_need_no_prompt() {
        let local = local_machine_id().expect("test host provides /etc/machine-id");
        let mut source = DndSource::default();
        arm_with_peer(&mut source, Some(&local));
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        // Exact-token approval is gated to different-machine reads only:
        // same-machine drags proceed directly to `t=k`.
        let actions = source.os_request_remote_entry(RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        });
        assert!(
            matches!(
                actions.as_slice(),
                [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=1;")
            ),
            "same-machine t=k must not prompt, got {actions:?}"
        );
        assert!(source.approvals.is_empty());
    }

    #[test]
    fn different_machine_remote_reads_need_approval() {
        let local = local_machine_id().expect("test host provides /etc/machine-id");
        let remote = hash_machine_id("different-machine-for-test");
        assert_ne!(local, remote);
        let mut source = DndSource::default();
        arm_with_peer(&mut source, Some(&remote));
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let entry = RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        };
        let actions = source.os_request_remote_entry(entry);
        let request_id = match actions.as_slice() {
            [DndSourceAction::RequestApproval(request)] => {
                assert_eq!(request.entry, entry);
                request.request_id
            }
            other => panic!("different-machine t=k must prompt, got {other:?}"),
        };
        let approved = source.approve(request_id);
        assert!(matches!(
            approved.as_slice(),
            [DndSourceAction::Write(frame)] if frame.starts_with(b"\x1b]72;t=k:x=1;")
        ));
    }

    #[test]
    fn remote_directory_lists_split_on_nul() {
        let mut source = DndSource::with_policy(DndSourcePolicy {
            allow_remote_reads: true,
            max_present_bytes: MAX_SOURCE_PRESENT_BYTES,
        });
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/uri-list");
        start_drag(&mut source);
        let entry = RemoteEntryRef {
            entry: 1,
            num: 0,
            parent: 0,
        };
        assert!(matches!(
            source.os_request_remote_entry(entry).as_slice(),
            [DndSourceAction::Write(_)]
        ));
        let mut response = command(DndCommandType::UriListData);
        response.cell_x = 1;
        response.pixel_x = 7;
        response.payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            b"a.txt\x00sub\x00",
        )
        .into_bytes();
        let ready = source.handle_command(response);
        assert!(matches!(
            ready.as_slice(),
            [DndSourceAction::OsDataReady { kind: RemoteEntryKind::Directory, dir_entries, .. }]
                if dir_entries == &vec![b"a.txt".to_vec(), b"sub".to_vec()]
        ));
    }

    #[test]
    fn rgb_image_dimensions_are_validated() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        let mut image = command(DndCommandType::PresentData);
        image.cell_x = -1;
        image.cell_y = 24;
        image.pixel_x = 2;
        image.pixel_y = 1;
        // 2x1 RGB needs exactly 6 bytes; send 3.
        image.payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            [1_u8, 2, 3],
        )
        .into_bytes();
        let actions = source.handle_command(image);
        assert!(matches!(
            actions.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(6).any(|part| part == b"EINVAL")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn png_image_marks_the_os_offer() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 2, "text/uri-list");
        let mut image = command(DndCommandType::PresentData);
        image.cell_x = -1;
        image.cell_y = 100;
        image.pixel_x = 8;
        image.pixel_y = 8;
        image.payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            [0x89, b'P', b'N', b'G'],
        )
        .into_bytes();
        assert!(source.handle_command(image).is_empty());
        let mut start = command(DndCommandType::ChangeDragImage);
        start.cell_x = -1;
        match source.handle_command(start).as_slice() {
            [DndSourceAction::BeginOsDrag(offer)] => {
                assert!(offer.has_image);
                assert_eq!(offer.operation, DndOperation::Move);
            }
            other => panic!("expected BeginOsDrag, got {other:?}"),
        }
    }

    #[test]
    fn application_cancel_tears_down_the_os_drag() {
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        start_drag(&mut source);
        let mut cancel = command(DndCommandType::DragOfferError);
        cancel.cell_y = -1;
        cancel.has_cell_y = true;
        assert_eq!(
            source.handle_command(cancel),
            vec![DndSourceAction::CancelOsDrag]
        );
        assert_eq!(source.phase, SourcePhase::Armed);
    }

    #[test]
    fn valid_index_error_fails_only_that_request() {
        // Per-item `t=E:y=idx` (dnd.c:2112-2122): offer kept, drag alive.
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain text/html");
        start_drag(&mut source);
        // Pull MIME 1 so it is outstanding, then fail it.
        assert!(matches!(
            source.os_request_data(1).as_slice(),
            [DndSourceAction::Write(_)]
        ));
        let mut error = command(DndCommandType::DragOfferError);
        error.cell_y = 1;
        error.has_cell_y = true;
        error.payload = b"EIO".to_vec();
        assert_eq!(
            source.handle_command(error),
            vec![DndSourceAction::OsDataFailed {
                mime_index: Some(1),
                entry: None,
            }]
        );
        assert!(source.is_dragging());
        // The other MIME is still servable: the drag survived.
        assert!(matches!(
            source.os_request_data(0).as_slice(),
            [DndSourceAction::Write(_)]
        ));

        // Valid index without a prior pull still fails per-item, not the drag.
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        start_drag(&mut source);
        let mut error = command(DndCommandType::DragOfferError);
        error.cell_y = 0;
        error.has_cell_y = true;
        error.payload = b"ENOENT".to_vec();
        assert_eq!(
            source.handle_command(error),
            vec![DndSourceAction::OsDataFailed {
                mime_index: Some(0),
                entry: None,
            }]
        );
        assert!(source.is_dragging());
    }

    #[test]
    fn missing_and_out_of_bounds_error_aborts_the_drag() {
        for (cell_y, has_cell_y) in [(0, false), (99, true), (-2, true)] {
            let mut source = DndSource::default();
            arm(&mut source);
            gesture(&mut source);
            offer(&mut source, 1, "text/plain");
            start_drag(&mut source);
            let mut error = command(DndCommandType::DragOfferError);
            error.cell_y = cell_y;
            error.has_cell_y = has_cell_y;
            error.payload = b"EIO".to_vec();
            let actions = source.handle_command(error);
            assert!(
                matches!(
                    actions.as_slice(),
                    [DndSourceAction::Write(frame), DndSourceAction::CancelOsDrag]
                        if frame.windows(6).any(|part| part == b"EINVAL")
                ),
                "y={cell_y} present={has_cell_y} must abort with EINVAL, got {actions:?}"
            );
            assert_eq!(source.phase, SourcePhase::Armed);
        }
    }

    #[test]
    fn finish_x4_is_terminal() {
        // `t=e:x=4:y` ends the drag (dnd.c:1861-1886); no further data flows.
        let mut source = DndSource::default();
        arm(&mut source);
        gesture(&mut source);
        offer(&mut source, 1, "text/plain");
        start_drag(&mut source);
        let finished = source.os_finished(false);
        assert!(matches!(
            finished.as_slice(),
            [DndSourceAction::Write(frame)] if frame.windows(7).any(|part| part == b"x=4:y=0")
        ));
        assert_eq!(source.phase, SourcePhase::Armed);
        assert!(source.os_request_data(0).is_empty());
        assert!(source
            .os_request_remote_entry(RemoteEntryRef {
                entry: 1,
                num: 0,
                parent: 0,
            })
            .is_empty());
    }

    #[test]
    fn machine_id_hash_has_versioned_hex_shape() {
        let hashed = hash_machine_id("test-machine");
        assert!(hashed.starts_with("1:"));
        assert_eq!(hashed.len(), 2 + 64);
        assert_eq!(hashed, hash_machine_id("test-machine"));
        assert_ne!(hashed, hash_machine_id("other-machine"));
    }

    #[test]
    fn peer_classification_fails_closed() {
        let local = hash_machine_id("local");
        assert!(!is_remote_peer(Some(&local), &local));
        assert!(is_remote_peer(Some(&local), &hash_machine_id("remote")));
        assert!(is_remote_peer(Some(&local), "2:anything"));
        assert!(is_remote_peer(None, &local));
        assert!(is_remote_peer(Some(&local), ""));
    }
}
