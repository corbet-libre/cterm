//! Shared Kitty OSC 99 desktop-notification model.
//!
//! One state machine for the daemon parser and all three native frontends
//! (GApplication notifications, UserNotifications.framework, Win32 balloons).
//! The authoritative Kitty spec is `desktop-notifications.rst` (Kitty 0.36+):
//! chunked `title`/`body`/`icon`/`buttons` payloads, stable `i=` replace/close
//! IDs, `a=` activation actions (`focus`, `report`), `c=` close reporting,
//! `o=` delivery occasions, `f=`/`t=` filter identity, `n=`/`g=`/`p=icon`
//! icons, `p=buttons` buttons, `s=` sounds, `w=` expiry, `p=?` capability
//! queries and `p=alive` liveness polling.
//!
//! Advertised capability set (single source of truth, see
//! [`capability_reply`]): `p=title,body,icon,buttons,?,alive,close`,
//! `a=focus,report`, `o=always,unfocused,invisible`, `u=0,1,2`,
//! `s=silent,system`, `w=1`, `c=1`. Named sounds beyond `silent`/`system`
//! are accepted but rendered as the platform default (see per-platform
//! matrix in `docs/parity.md`).

use std::collections::{HashMap, VecDeque};

/// Maximum decoded bytes for one notification title.
pub const MAX_NOTIFICATION_TITLE_BYTES: usize = 1024;
/// Maximum decoded bytes for one notification body.
pub const MAX_NOTIFICATION_BODY_BYTES: usize = 4096;
/// Maximum decoded bytes for one `p=buttons` payload before splitting.
pub const MAX_NOTIFICATION_BUTTONS_BYTES: usize = 4096;
/// Maximum number of buttons kept per notification.
pub const MAX_NOTIFICATION_BUTTONS: usize = 8;
/// Maximum decoded bytes kept per button label.
pub const MAX_NOTIFICATION_BUTTON_BYTES: usize = 256;
/// Maximum decoded bytes for one `p=icon` payload (PNG/JPEG/GIF, ~256x256).
pub const MAX_NOTIFICATION_ICON_BYTES: usize = 1024 * 1024;
/// Maximum icon-cache entries (evicted oldest-first).
pub const MAX_ICON_CACHE_ENTRIES: usize = 32;
/// Maximum total icon-cache bytes.
pub const MAX_ICON_CACHE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum tracked live notification IDs (matches Kitty multiplexer practice).
pub const MAX_ACTIVE_NOTIFICATIONS: usize = 64;
/// Maximum decoded bytes for the `f=` application name.
pub const MAX_APP_NAME_BYTES: usize = 256;
/// Maximum notification types (`t=`) kept per notification.
pub const MAX_NOTIFICATION_TYPES: usize = 8;
/// Maximum decoded bytes per notification type.
pub const MAX_NOTIFICATION_TYPE_BYTES: usize = 128;
/// Maximum icon names (`n=`) kept per notification.
pub const MAX_ICON_NAMES: usize = 8;
/// Maximum decoded bytes per icon name.
pub const MAX_ICON_NAME_BYTES: usize = 256;
/// Maximum decoded bytes for a named `s=` sound.
pub const MAX_SOUND_NAME_BYTES: usize = 64;

/// When a notification may be delivered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NotificationOccasion {
    /// Always honor the request.
    #[default]
    Always,
    /// Only when the issuing window lacks keyboard focus.
    Unfocused,
    /// Only when the issuing window is unfocused and not visible
    /// (inactive tab or inactive OS window).
    Invisible,
}

impl NotificationOccasion {
    /// Parse an `o=` value; unknown values return `None` (caller keeps current).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            "unfocused" => Some(Self::Unfocused),
            "invisible" => Some(Self::Invisible),
            _ => None,
        }
    }

    /// Wire value for capability replies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Unfocused => "unfocused",
            Self::Invisible => "invisible",
        }
    }
}

/// Whether an `o=`-gated notification should be delivered.
pub fn should_deliver(occasion: NotificationOccasion, is_focused: bool, is_visible: bool) -> bool {
    match occasion {
        NotificationOccasion::Always => true,
        NotificationOccasion::Unfocused => !is_focused,
        NotificationOccasion::Invisible => !is_focused && !is_visible,
    }
}

/// Strict Kitty identifier: `[a-zA-Z0-9_\-+.]{1,64}`.
/// `i=0` is reserved for untracked responses and must never be used by apps.
pub fn notification_id_is_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'.'))
}

/// Sanitize an ID before echoing it back in a terminal report (input-injection
/// hardening per the Kitty spec). Invalid/missing IDs become `"0"`.
pub fn sanitize_id_for_report(id: Option<&str>) -> String {
    match id {
        Some(id) if notification_id_is_valid(id) && id != "0" => id.to_owned(),
        _ => "0".to_owned(),
    }
}

/// Decode one possibly-unpadded base64 chunk (Kitty permits chunking before or
/// after encoding, so the final chunk may lack `=` padding).
pub fn decode_notification_base64(chunk: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(chunk) {
        return Some(decoded);
    }
    // Retry with padding restored (0-3 `=` to reach a multiple of 4).
    let mut padded = chunk.to_vec();
    let remainder = padded.len() % 4;
    if remainder == 1 {
        return None;
    }
    if remainder != 0 {
        padded.extend(std::iter::repeat_n(b'=', 4 - remainder));
    }
    base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .ok()
}

/// Truncate a UTF-8 string to a byte bound on a char boundary.
pub fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Split a decoded `p=buttons` payload on U+2028 (Line Separator), dropping
/// empties, capping count and per-label length.
pub fn split_buttons(payload: &str) -> Vec<String> {
    payload
        .split('\u{2028}')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .take(MAX_NOTIFICATION_BUTTONS)
        .map(|label| truncate_utf8(label, MAX_NOTIFICATION_BUTTON_BYTES).to_owned())
        .collect()
}

/// Activation report: `<OSC> 99 ; i=id ; <terminator>` or
/// `<OSC> 99 ; i=id ; button_number <terminator>` (1-based).
pub fn activation_report(id: Option<&str>, button: Option<usize>) -> Vec<u8> {
    let id = sanitize_id_for_report(id);
    match button {
        Some(number) => format!("\x1b]99;i={id};{number}\x1b\\").into_bytes(),
        None => format!("\x1b]99;i={id};\x1b\\").into_bytes(),
    }
}

/// Close report: `<OSC> 99 ; i=id : p=close ; <terminator>`, or the
/// `untracked` variant for platforms whose OS gives no dismiss signal
/// (GTK/Cocoa): `<OSC> 99 ; i=id : p=close ; untracked <terminator>`.
pub fn close_report(id: Option<&str>, untracked: bool) -> Vec<u8> {
    let id = sanitize_id_for_report(id);
    if untracked {
        format!("\x1b]99;i={id}:p=close;untracked\x1b\\").into_bytes()
    } else {
        format!("\x1b]99;i={id}:p=close;\x1b\\").into_bytes()
    }
}

/// Liveness reply: `<OSC> 99 ; i=query : p=alive ; id1,id2 <terminator>`.
pub fn alive_reply(query_id: &str, alive_ids: &[String], bell_terminated: bool) -> Vec<u8> {
    let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
    format!(
        "\x1b]99;i={query_id}:p=alive;{}{terminator}",
        alive_ids.join(",")
    )
    .into_bytes()
}

/// Capability reply. This is the single source of truth for the advertised
/// set — it must never list a payload/action the frontends do not wire.
/// Currently: title/body/icon/buttons/alive/close payloads, focus+report
/// activation, all three occasions, all urgencies, silent+system sounds,
/// expiry, and close events.
pub fn capability_reply(query_id: &str, bell_terminated: bool) -> Vec<u8> {
    let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
    format!(
        "\x1b]99;i={query_id}:p=?;p=title,body,icon,buttons,?,alive,close:a=focus,report:o=always,unfocused,invisible:u=0,1,2:s=silent,system:w=1:c=1{terminator}"
    )
    .into_bytes()
}

/// Terminal-side expiry policy for one notification's `w=` value.
///
/// Kitty ground truth (`desktop-notifications.rst`, Kitty 0.36+): an absent
/// or negative `w` (the default is `-1`) defers to whatever expiry policy
/// the OS notification service has; `w=0` requests never-expire (best
/// effort — some platforms honor it and some do not); positive values are
/// terminal-enforced auto-close deadlines (robust: the terminal withdraws
/// the notification itself, so it is guaranteed closed past the deadline
/// barring earlier user/OS dismissal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryPolicy {
    /// No terminal timer; the OS notification service decides the lifetime.
    OsDefault,
    /// Never schedule a terminal auto-close. The notification persists in
    /// the frontend registry and the daemon `p=alive` set until the app
    /// replaces it (same `i=`), explicitly closes it (`p=close`), it is
    /// evicted by the bounded live-ID cap, or the frontend process exits.
    /// The OS may still retire the visible banner (best effort). A daemon
    /// reconnect does not clear the daemon-side `p=alive` entry (sessions
    /// survive UI restarts), but the frontend registry is process-local:
    /// after a UI restart the banner is gone locally with no replay while
    /// the daemon still lists the ID until the app closes it.
    Never,
    /// Terminal-enforced auto-close after this many milliseconds, with a
    /// real (tracked) close report when `c=1` was requested.
    After(u64),
}

/// Resolve one raw `w=` value to its terminal-side expiry policy.
pub fn expiry_policy(expire_time: Option<i32>) -> ExpiryPolicy {
    match expire_time {
        Some(milliseconds) if milliseconds > 0 => ExpiryPolicy::After(milliseconds as u64),
        Some(0) => ExpiryPolicy::Never,
        _ => ExpiryPolicy::OsDefault,
    }
}

/// Effective native timeout in milliseconds: `Some` is a terminal-enforced
/// auto-close deadline (`w > 0`); `None` means no terminal timer — either
/// the OS default (`w` absent/negative) or never-expire (`w = 0`, see
/// [`never_expires`] / [`expiry_policy`] to tell them apart).
pub fn effective_timeout_ms(expire_time: Option<i32>) -> Option<u64> {
    match expiry_policy(expire_time) {
        ExpiryPolicy::After(milliseconds) => Some(milliseconds),
        ExpiryPolicy::OsDefault | ExpiryPolicy::Never => None,
    }
}

/// Whether `w=` requests never-expire semantics (`w = 0` exactly).
/// Never-expire schedules no terminal timer on any backend; see
/// [`ExpiryPolicy::Never`] for the exact persistence contract.
pub fn never_expires(expire_time: Option<i32>) -> bool {
    expiry_policy(expire_time) == ExpiryPolicy::Never
}

/// Ordered terminal-to-application reports for one user activation.
///
/// Kitty contract: when a notification requested both activation
/// (`a=report`) and close (`c=1`) reports, activation emits the activation
/// report FIRST followed by the close report. Either report may also appear
/// alone; expiry/replace emit only the close report (never an activation),
/// and a close report never precedes its activation.
pub fn activation_close_sequence(
    id: Option<&str>,
    button: Option<usize>,
    report_activation: bool,
    report_close: bool,
) -> Vec<Vec<u8>> {
    let mut reports = Vec::with_capacity(2);
    if report_activation {
        reports.push(activation_report(id, button));
    }
    if report_close {
        reports.push(close_report(id, false));
    }
    reports
}

/// Normalize one user-configured filter entry: surrounding whitespace is
/// insignificant, empty entries never match anything. Matching itself is an
/// exact, case-sensitive comparison against the parsed `f=`/`t=` identity.
pub fn normalize_filter_entry(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// User-configured notification allow/deny lists over the `f=` application
/// name and `t=` notification types.
///
/// All lists empty (the default) allows everything — today's behavior.
/// Otherwise: deny wins over allow, and a non-empty allow-list requires a
/// positive match. Untagged notifications (no `f=`/`t=`, e.g. legacy OSC
/// 9/777) therefore never match a deny entry but also never satisfy an
/// allow-list. This is intentionally simpler than Kitty's
/// `filter_notification` `field:regexp` boolean grammar (which also matches
/// title/body): exact per-app/type lists cover the common cases with
/// TOML-native config; title/body/regex filtering stays future work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationFilter {
    /// `f=` names that may deliver when non-empty (untagged never matches).
    pub allow_apps: Vec<String>,
    /// `f=` names that never deliver (wins over `allow_apps`).
    pub deny_apps: Vec<String>,
    /// `t=` types of which at least one must be present when non-empty.
    pub allow_types: Vec<String>,
    /// `t=` types that never deliver (wins over `allow_types`).
    pub deny_types: Vec<String>,
}

impl NotificationFilter {
    /// Whether this filter allows everything (all lists empty).
    pub fn is_allow_all(&self) -> bool {
        self.allow_apps.is_empty()
            && self.deny_apps.is_empty()
            && self.allow_types.is_empty()
            && self.deny_types.is_empty()
    }

    /// Whether a notification with this filter identity may be delivered.
    /// Enforcement point for frontends: call before occasion filtering and
    /// before any registry/report side effects.
    pub fn allows(&self, app_name: Option<&str>, notif_types: &[String]) -> bool {
        if let Some(app) = app_name {
            if self.deny_apps.iter().any(|denied| denied == app) {
                return false;
            }
        }
        if notif_types
            .iter()
            .any(|kind| self.deny_types.iter().any(|denied| denied == kind))
        {
            return false;
        }
        if !self.allow_apps.is_empty()
            && !app_name.is_some_and(|app| self.allow_apps.iter().any(|allowed| allowed == app))
        {
            return false;
        }
        if !self.allow_types.is_empty()
            && !notif_types
                .iter()
                .any(|kind| self.allow_types.iter().any(|allowed| allowed == kind))
        {
            return false;
        }
        true
    }
}

/// Shared-model enforcement seam: whether a notification with this filter
/// identity may be delivered under `filter`. Tested identically to
/// [`NotificationFilter::allows`].
pub fn notification_allowed(
    filter: &NotificationFilter,
    app_name: Option<&str>,
    notif_types: &[String],
) -> bool {
    filter.allows(app_name, notif_types)
}

/// Bounded live-ID set backing `p=alive` and replace/close. Insertion-ordered
/// with oldest-first eviction past [`MAX_ACTIVE_NOTIFICATIONS`].
#[derive(Debug, Default)]
pub struct ActiveNotificationSet {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl ActiveNotificationSet {
    /// Track one shown notification ID. Returns an evicted ID, if any.
    pub fn insert(&mut self, id: String) -> Option<String> {
        if self.ids.contains(&id) {
            return None;
        }
        let mut evicted = None;
        if self.ids.len() >= MAX_ACTIVE_NOTIFICATIONS {
            while self.ids.len() >= MAX_ACTIVE_NOTIFICATIONS {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                if self.ids.remove(&oldest) {
                    evicted = Some(oldest);
                    break;
                }
            }
        }
        self.order.push_back(id.clone());
        self.ids.insert(id);
        evicted
    }

    /// Forget one closed notification. Returns true when it was tracked.
    /// Also prunes pending eviction-order entries so show/close churn over
    /// many IDs cannot grow the order queue without bound.
    pub fn remove(&mut self, id: &str) -> bool {
        let removed = self.ids.remove(id);
        if removed {
            self.order.retain(|entry| entry != id);
        }
        removed
    }

    /// Whether an ID is currently considered alive.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Sorted snapshot for deterministic `p=alive` replies.
    pub fn sorted_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.ids.iter().cloned().collect();
        ids.sort_unstable();
        ids
    }

    /// Number of tracked IDs.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no IDs are tracked.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Bounded `g=` icon-data cache (oldest-first eviction on entry or byte cap).
#[derive(Debug, Default)]
pub struct IconCache {
    entries: HashMap<String, Vec<u8>>,
    order: VecDeque<String>,
    total_bytes: usize,
}

impl IconCache {
    /// Cache icon bytes under `id`. Oversized single icons are rejected.
    /// Returns true when cached.
    pub fn insert(&mut self, id: String, data: Vec<u8>) -> bool {
        if data.is_empty() || data.len() > MAX_NOTIFICATION_ICON_BYTES {
            return false;
        }
        if let Some(previous) = self.entries.insert(id.clone(), data) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.len());
        } else {
            self.order.push_back(id.clone());
        }
        if let Some(data) = self.entries.get(&id) {
            self.total_bytes = self.total_bytes.saturating_add(data.len());
        }
        while self.entries.len() > MAX_ICON_CACHE_ENTRIES || self.total_bytes > MAX_ICON_CACHE_BYTES
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            // The same ID may appear twice in `order` after re-insertion;
            // only the removal that actually drops an entry frees bytes.
            if let Some(removed) = self.entries.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.len());
                if oldest == id {
                    return false;
                }
            }
        }
        self.entries.contains_key(&id)
    }

    /// Fetch cached icon bytes.
    pub fn get(&self, id: &str) -> Option<&[u8]> {
        self.entries.get(id).map(Vec::as_slice)
    }

    /// Number of cached icons.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

use std::collections::HashSet;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_reply_advertises_only_wired_features() {
        let reply = String::from_utf8(capability_reply("q", false)).unwrap();
        assert!(reply.starts_with("\x1b]99;i=q:p=?;"));
        assert!(reply.contains("p=title,body,icon,buttons,?,alive,close"));
        assert!(reply.contains("a=focus,report"));
        assert!(reply.contains("o=always,unfocused,invisible"));
        assert!(reply.contains("u=0,1,2"));
        assert!(reply.contains("s=silent,system"));
        assert!(reply.contains("w=1"));
        assert!(reply.contains("c=1"));
        assert!(!reply.contains("c=0"));
        assert!(reply.ends_with("\x1b\\"));
        let bell = String::from_utf8(capability_reply("q", true)).unwrap();
        assert!(bell.ends_with('\x07'));
    }

    #[test]
    fn activation_and_close_reports_follow_kitty_shape() {
        assert_eq!(
            activation_report(Some("build"), None),
            b"\x1b]99;i=build;\x1b\\".to_vec()
        );
        assert_eq!(
            activation_report(Some("build"), Some(2)),
            b"\x1b]99;i=build;2\x1b\\".to_vec()
        );
        assert_eq!(
            activation_report(None, None),
            b"\x1b]99;i=0;\x1b\\".to_vec()
        );
        assert_eq!(
            activation_report(Some("bad id!"), None),
            b"\x1b]99;i=0;\x1b\\".to_vec()
        );
        assert_eq!(
            close_report(Some("build"), false),
            b"\x1b]99;i=build:p=close;\x1b\\".to_vec()
        );
        assert_eq!(
            close_report(Some("build"), true),
            b"\x1b]99;i=build:p=close;untracked\x1b\\".to_vec()
        );
        assert_eq!(
            close_report(None, false),
            b"\x1b]99;i=0:p=close;\x1b\\".to_vec()
        );
    }

    #[test]
    fn occasion_filtering_matches_spec() {
        assert!(should_deliver(NotificationOccasion::Always, true, true));
        assert!(!should_deliver(NotificationOccasion::Unfocused, true, true));
        assert!(should_deliver(NotificationOccasion::Unfocused, false, true));
        assert!(!should_deliver(
            NotificationOccasion::Invisible,
            false,
            true
        ));
        assert!(!should_deliver(
            NotificationOccasion::Invisible,
            true,
            false
        ));
        assert!(should_deliver(
            NotificationOccasion::Invisible,
            false,
            false
        ));
        assert_eq!(NotificationOccasion::parse("bogus"), None);
    }

    #[test]
    fn expiry_policy_distinguishes_default_never_and_deadline() {
        assert_eq!(expiry_policy(None), ExpiryPolicy::OsDefault);
        assert_eq!(expiry_policy(Some(-1)), ExpiryPolicy::OsDefault);
        assert_eq!(expiry_policy(Some(i32::MIN)), ExpiryPolicy::OsDefault);
        assert_eq!(expiry_policy(Some(0)), ExpiryPolicy::Never);
        assert_eq!(expiry_policy(Some(1)), ExpiryPolicy::After(1));
        assert_eq!(expiry_policy(Some(5000)), ExpiryPolicy::After(5000));
    }

    #[test]
    fn effective_timeout_schedules_only_positive_deadlines() {
        // Neither the OS default nor never-expire schedules a terminal timer;
        // use `expiry_policy`/`never_expires` to tell them apart. w>0
        // behavior is unchanged.
        assert_eq!(effective_timeout_ms(None), None);
        assert_eq!(effective_timeout_ms(Some(-1)), None);
        assert_eq!(effective_timeout_ms(Some(0)), None);
        assert_eq!(effective_timeout_ms(Some(250)), Some(250));
    }

    #[test]
    fn never_expires_is_exactly_w_zero() {
        assert!(!never_expires(None));
        assert!(!never_expires(Some(-1)));
        assert!(never_expires(Some(0)));
        assert!(!never_expires(Some(1)));
    }

    #[test]
    fn activation_close_sequence_orders_activation_before_close() {
        // Both flags: activation first, then the tracked (non-untracked)
        // close — the Kitty activation contract.
        assert_eq!(
            activation_close_sequence(Some("job"), None, true, true),
            vec![
                activation_report(Some("job"), None),
                close_report(Some("job"), false),
            ]
        );
        // Button numbers ride on the activation report only.
        assert_eq!(
            activation_close_sequence(Some("ask"), Some(2), true, true),
            vec![
                activation_report(Some("ask"), Some(2)),
                close_report(Some("ask"), false),
            ]
        );
        // Either report alone.
        assert_eq!(
            activation_close_sequence(Some("job"), None, true, false),
            vec![activation_report(Some("job"), None)]
        );
        assert_eq!(
            activation_close_sequence(Some("job"), None, false, true),
            vec![close_report(Some("job"), false)]
        );
        // Neither requested: expiry/replace paths must not reuse this helper
        // to fabricate reports.
        assert!(activation_close_sequence(Some("job"), None, false, false).is_empty());
    }

    #[test]
    fn filter_defaults_to_allow_all() {
        let filter = NotificationFilter::default();
        assert!(filter.is_allow_all());
        // Today's behavior: untagged legacy notifications deliver, and so
        // does any tagged one.
        assert!(notification_allowed(&filter, None, &[]));
        assert!(notification_allowed(
            &filter,
            Some("anything"),
            &["marketing".to_owned()]
        ));
    }

    #[test]
    fn filter_deny_wins_over_allow() {
        let filter = NotificationFilter {
            allow_apps: vec!["editor".to_owned()],
            deny_apps: vec!["editor".to_owned()],
            allow_types: vec!["email".to_owned()],
            deny_types: vec!["email".to_owned()],
        };
        assert!(!filter.is_allow_all());
        assert!(!notification_allowed(
            &filter,
            Some("editor"),
            &["email".to_owned()]
        ));
        // Non-denied values still pass their allow-lists.
        let filter = NotificationFilter {
            allow_apps: vec!["editor".to_owned(), "ci".to_owned()],
            deny_apps: vec!["spammy".to_owned()],
            allow_types: vec!["email".to_owned()],
            deny_types: vec!["marketing".to_owned()],
        };
        assert!(notification_allowed(
            &filter,
            Some("ci"),
            &["email".to_owned()]
        ));
        assert!(!notification_allowed(
            &filter,
            Some("spammy"),
            &["email".to_owned()]
        ));
        assert!(!notification_allowed(
            &filter,
            Some("ci"),
            &["marketing".to_owned()]
        ));
    }

    #[test]
    fn filter_allow_lists_require_a_positive_match() {
        let filter = NotificationFilter {
            allow_apps: vec!["myapp".to_owned()],
            ..Default::default()
        };
        assert!(notification_allowed(&filter, Some("myapp"), &[]));
        assert!(!notification_allowed(&filter, Some("other"), &[]));
        // Untagged app names never satisfy an allow-list but never match a
        // deny-list either.
        assert!(!notification_allowed(&filter, None, &[]));
        let deny_only = NotificationFilter {
            deny_apps: vec!["spammy".to_owned()],
            ..Default::default()
        };
        assert!(notification_allowed(&deny_only, None, &[]));
        assert!(notification_allowed(&deny_only, Some("myapp"), &[]));

        let types = NotificationFilter {
            allow_types: vec!["email".to_owned()],
            ..Default::default()
        };
        assert!(notification_allowed(
            &types,
            Some("myapp"),
            &["chat".to_owned(), "email".to_owned()]
        ));
        assert!(!notification_allowed(
            &types,
            Some("myapp"),
            &["chat".to_owned()]
        ));
        // Typeless notifications cannot satisfy a type allow-list.
        assert!(!notification_allowed(&types, Some("myapp"), &[]));
        assert!(!notification_allowed(&types, None, &[]));
    }

    #[test]
    fn filter_entries_normalize_whitespace_and_empties() {
        assert_eq!(
            normalize_filter_entry("  myapp  "),
            Some("myapp".to_owned())
        );
        assert_eq!(normalize_filter_entry(""), None);
        assert_eq!(normalize_filter_entry("   "), None);
    }

    #[test]
    fn active_set_remove_prunes_eviction_order() {
        let mut set = ActiveNotificationSet::default();
        set.insert("a".to_owned());
        set.insert("b".to_owned());
        set.insert("c".to_owned());
        assert!(set.remove("b"));
        // Churn: refill to the cap. Without order pruning the stale `b`
        // entry would linger in the queue; eviction must still drop the
        // oldest *live* ID (`a`) first.
        for index in 0..(MAX_ACTIVE_NOTIFICATIONS - 2) {
            set.insert(format!("fill-{index}"));
        }
        assert_eq!(set.len(), MAX_ACTIVE_NOTIFICATIONS);
        let evicted = set.insert("new".to_owned());
        assert_eq!(evicted.as_deref(), Some("a"));
        assert!(!set.contains("a"));
        assert!(set.contains("c"));
        assert!(set.contains("new"));
    }

    #[test]
    fn active_set_evicts_oldest_past_cap() {
        let mut set = ActiveNotificationSet::default();
        for index in 0..MAX_ACTIVE_NOTIFICATIONS {
            assert_eq!(set.insert(format!("id-{index}")), None);
        }
        assert_eq!(set.len(), MAX_ACTIVE_NOTIFICATIONS);
        // Re-inserting a known ID is a no-op (replace, not duplicate).
        assert_eq!(set.insert("id-0".to_owned()), None);
        assert_eq!(set.len(), MAX_ACTIVE_NOTIFICATIONS);
        let evicted = set.insert("new".to_owned());
        assert_eq!(evicted.as_deref(), Some("id-0"));
        assert!(!set.contains("id-0"));
        assert!(set.contains("new"));
        assert!(set.remove("new"));
        assert!(!set.remove("missing"));
        let mut sorted = set.sorted_ids();
        let mut expected = sorted.clone();
        expected.sort_unstable();
        assert_eq!(sorted, expected);
        sorted.clear();
        assert!(sorted.is_empty());
    }

    #[test]
    fn icon_cache_bounds_entries_and_bytes() {
        let mut cache = IconCache::default();
        assert!(cache.is_empty());
        assert!(cache.insert("a".to_owned(), vec![1, 2, 3]));
        assert_eq!(cache.get("a"), Some([1, 2, 3].as_slice()));
        assert_eq!(cache.len(), 1);
        assert!(!cache.insert("big".to_owned(), vec![0; MAX_NOTIFICATION_ICON_BYTES + 1]));
        assert!(!cache.insert("empty".to_owned(), Vec::new()));
        for index in 0..MAX_ICON_CACHE_ENTRIES + 5 {
            assert!(cache.insert(format!("k-{index}"), vec![9; 16]));
        }
        assert_eq!(cache.len(), MAX_ICON_CACHE_ENTRIES);
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn buttons_split_on_line_separator_with_caps() {
        let payload = "One\u{2028}Two\u{2028}  \u{2028}Three";
        assert_eq!(split_buttons(payload), vec!["One", "Two", "Three"]);
        let many = (0..20)
            .map(|i| format!("b{i}"))
            .collect::<Vec<_>>()
            .join("\u{2028}");
        assert_eq!(split_buttons(&many).len(), MAX_NOTIFICATION_BUTTONS);
        let long = "x".repeat(MAX_NOTIFICATION_BUTTON_BYTES + 10);
        let labels = split_buttons(&long);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].len(), MAX_NOTIFICATION_BUTTON_BYTES);
    }

    #[test]
    fn base64_decode_tolerates_missing_padding() {
        // "Hello" without padding.
        assert_eq!(
            decode_notification_base64(b"SGVsbG8"),
            Some(b"Hello".to_vec())
        );
        assert_eq!(
            decode_notification_base64(b"SGVsbG8="),
            Some(b"Hello".to_vec())
        );
        assert_eq!(decode_notification_base64(b"!!!"), None);
        assert_eq!(decode_notification_base64(b"a"), None);
    }
}
