//! Bounded wire codec for Kitty's OSC 5522 rich-clipboard protocol.
//!
//! The terminal-side session and consent policy intentionally live above the
//! raw parsing here where possible: decoding an escape sequence must never
//! grant clipboard access. Protocol:
//! <https://sw.kovidgoyal.net/kitty/clipboard/>
//!
//! Wire shape: `OSC 5522 ; metadata ; payload ST` where `metadata` is a
//! colon-separated `key=value` list and `payload` is base64 data (possibly
//! empty). Raw keys (`type`, `loc`, `id`) travel verbatim; `pw`, `name`, and
//! `mime` values are base64-encoded UTF-8. Terminal replies always echo a
//! sanitized `id` when the request carried one.
//!
//! Consent model (mirrors the Kitty file-transfer philosophy: exact-token
//! reuse plus deny-by-default prompts, never a weaker path):
//! - MIME-list reads (payload decodes to `.`) need no prompt.
//! - Data reads carrying a `pw`+`name` that exactly matches a previously
//!   authorized token, or a live one-time paste-event token for the same
//!   location, are pre-authorized.
//! - Every other data read must be approved through a native frontend prompt;
//!   denial is reported as `EPERM`.
//! - Writes are allowed without a prompt (Kitty's own default), but a write
//!   may register its `pw`+`name` for future reads.

use base64::engine::general_purpose::{STANDARD as PADDED_BASE64, STANDARD_NO_PAD};
use base64::Engine;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Maximum accepted MIME type length, in bytes.
///
/// Real-world types (`text/plain`, `image/png`, long vendor trees) are far
/// shorter; anything longer cannot name a usable clipboard format.
pub const MAX_RICH_MIME_BYTES: usize = 256;
/// Maximum MIME types accepted in one read request or alias packet.
pub const MAX_RICH_MIME_COUNT: usize = 32;
/// Maximum decoded bytes in a single `wdata` chunk (protocol-mandated).
pub const MAX_RICH_CHUNK_BYTES: usize = 4096;
/// Maximum accumulated decoded bytes for one MIME type in a write session.
pub const MAX_RICH_ITEM_BYTES: usize = 1024 * 1024;
/// Maximum accumulated decoded bytes across a whole write session.
pub const MAX_RICH_SESSION_BYTES: usize = 4 * 1024 * 1024;
/// Maximum distinct MIME types accumulated in one write session.
pub const MAX_RICH_WRITE_ITEMS: usize = 32;
/// Maximum aliases recorded in one write session.
pub const MAX_RICH_ALIAS_COUNT: usize = 32;
/// Maximum decoded bytes for a `pw` or `name` credential field.
pub const MAX_RICH_CREDENTIAL_BYTES: usize = 512;
/// Maximum retained characters of a multiplexer `id` after sanitizing.
pub const MAX_RICH_ID_CHARS: usize = 128;
/// Maximum rich-clipboard operations retained until the frontend drains them.
pub const MAX_PENDING_RICH_CLIPBOARD_OPS: usize = 32;
/// Maximum reads awaiting frontend data before the oldest is evicted.
pub const MAX_PENDING_RICH_READS: usize = 8;
/// Maximum exact-token (`pw`+`name`) consents remembered per screen.
pub const MAX_AUTHORIZED_CLIPBOARD_PASSWORDS: usize = 16;
/// Maximum live one-time paste-event tokens per screen.
pub const MAX_PASTE_PASSWORDS: usize = 8;
/// How long a minted paste-event token stays usable (besides single-use).
pub const PASTE_PASSWORD_TTL: Duration = Duration::from_secs(120);

/// Which native clipboard a rich operation addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RichClipboardLocation {
    #[default]
    Clipboard,
    Primary,
}

impl RichClipboardLocation {
    fn from_loc_value(value: &str) -> Option<Self> {
        match value {
            "primary" => Some(Self::Primary),
            _ => None,
        }
    }

    /// Wire name used in paste-event notifications (`loc=primary`).
    pub fn loc_value(self) -> Option<&'static str> {
        match self {
            Self::Clipboard => None,
            Self::Primary => Some("primary"),
        }
    }
}

/// Terminal-side status for OSC 5522 replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichClipboardStatus {
    Perm,
    NoSys,
    Busy,
    Inval,
    Io,
}

impl RichClipboardStatus {
    /// Protocol `status=` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Perm => "EPERM",
            Self::NoSys => "ENOSYS",
            Self::Busy => "EBUSY",
            Self::Inval => "EINVAL",
            Self::Io => "EIO",
        }
    }
}

/// One typed clipboard payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichClipboardItem {
    pub mime: String,
    pub data: Vec<u8>,
}

/// MIME aliases sharing one transmitted payload (`type=walias`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichClipboardAlias {
    pub target: String,
    pub aliases: Vec<String>,
}

/// Application-supplied consent credential (`pw` + human-friendly `name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichCredential {
    pub password: String,
    pub name: String,
}

/// A validated application read request awaiting frontend data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichReadRequest {
    /// Sanitized multiplexer id echoed in every reply (may be empty).
    pub id: String,
    pub location: RichClipboardLocation,
    /// Requested MIME types; empty when `list_only`.
    pub mimes: Vec<String>,
    /// True when the payload decoded to `.` (MIME listing, no prompt).
    pub list_only: bool,
    /// Credential presented with the request, if both halves were valid.
    pub credential: Option<RichCredential>,
    /// True when core policy already allows this read (listing, exact-token
    /// consent, or a live paste-event token). Otherwise the frontend must
    /// show a deny-by-default native prompt before answering.
    pub pre_authorized: bool,
    /// Human name for prompt text (`credential.name` or a fallback).
    pub app_name: Option<String>,
}

/// Terminal-side operation queued for the native frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RichClipboardOperation {
    /// A completed write session: install these types natively, then done.
    /// (`status=DONE` was already queued to the PTY at completion.)
    Write {
        id: String,
        location: RichClipboardLocation,
        items: Vec<RichClipboardItem>,
        aliases: Vec<RichClipboardAlias>,
    },
    /// A validated read: fetch native data (prompting first unless
    /// `request.pre_authorized`) and answer through
    /// `Screen::complete_rich_clipboard_read` / `..._list` / `fail_...`.
    Read { request: RichReadRequest },
}

/// One-time paste-event token minted with a 5522-mode notification.
///
/// Public so [`Screen`](crate::screen::Screen) can hold the queue; fields stay
/// crate-internal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePassword {
    pub(crate) password: String,
    pub(crate) location: RichClipboardLocation,
    pub(crate) created: Instant,
}

/// Accumulator for one `type=write` session.
#[derive(Debug, Default)]
pub struct RichWriteSession {
    pub(crate) location: RichClipboardLocation,
    pub(crate) id: String,
    pub(crate) items: Vec<RichClipboardItem>,
    pub(crate) aliases: Vec<RichClipboardAlias>,
    pub(crate) session_bytes: usize,
    pub(crate) failed: bool,
}

impl RichWriteSession {
    pub fn new(location: RichClipboardLocation, id: String) -> Self {
        Self {
            location,
            id,
            items: Vec::new(),
            aliases: Vec::new(),
            session_bytes: 0,
            failed: false,
        }
    }

    /// Append one decoded chunk to `mime`'s payload.
    pub fn add_chunk(&mut self, mime: &str, chunk: Vec<u8>) -> Result<(), RichClipboardStatus> {
        let mime = validate_mime(mime).ok_or(RichClipboardStatus::Inval)?;
        if chunk.len() > MAX_RICH_CHUNK_BYTES {
            return Err(RichClipboardStatus::Inval);
        }
        let item = if let Some(item) = self.items.iter_mut().find(|item| item.mime == mime) {
            item
        } else {
            if self.items.len() >= MAX_RICH_WRITE_ITEMS {
                return Err(RichClipboardStatus::Inval);
            }
            self.items.push(RichClipboardItem {
                mime,
                data: Vec::new(),
            });
            self.items.last_mut().expect("just pushed")
        };
        if item.data.len().saturating_add(chunk.len()) > MAX_RICH_ITEM_BYTES
            || self.session_bytes.saturating_add(chunk.len()) > MAX_RICH_SESSION_BYTES
        {
            return Err(RichClipboardStatus::Io);
        }
        item.data.extend_from_slice(&chunk);
        self.session_bytes = self.session_bytes.saturating_add(chunk.len());
        Ok(())
    }

    /// Record `aliases` for an already- (or later-) transmitted `target`.
    /// Forward references resolve at completion; unknown targets are dropped
    /// there rather than failing the session.
    pub fn add_alias(
        &mut self,
        target: &str,
        aliases: Vec<String>,
    ) -> Result<(), RichClipboardStatus> {
        let target = validate_mime(target).ok_or(RichClipboardStatus::Inval)?;
        if aliases.is_empty() || aliases.len() > MAX_RICH_MIME_COUNT {
            return Err(RichClipboardStatus::Inval);
        }
        if self.aliases.len() >= MAX_RICH_ALIAS_COUNT {
            return Err(RichClipboardStatus::Inval);
        }
        let mut clean = Vec::with_capacity(aliases.len());
        for alias in aliases {
            clean.push(validate_mime(&alias).ok_or(RichClipboardStatus::Inval)?);
        }
        self.aliases.push(RichClipboardAlias {
            target,
            aliases: clean,
        });
        Ok(())
    }

    /// Resolve aliases against received payloads, dropping references to
    /// MIME types that never arrived.
    pub fn finish(self) -> (Vec<RichClipboardItem>, Vec<RichClipboardAlias>) {
        let aliases = self
            .aliases
            .into_iter()
            .filter(|alias| self.items.iter().any(|item| item.mime == alias.target))
            .collect();
        (self.items, aliases)
    }
}

/// Decode base64, accepting Kitty's unpadded form as well as padded form.
///
/// Replies are always padded; inputs accept both so compliant senders are
/// never rejected on padding style. Garbage is still an error.
pub fn decode_clipboard_base64(input: &str) -> Option<Vec<u8>> {
    if input.is_empty() {
        return Some(Vec::new());
    }
    PADDED_BASE64
        .decode(input)
        .or_else(|_| STANDARD_NO_PAD.decode(input))
        .ok()
}

/// Validate and normalize one MIME type (`type/subtype`, token chars only,
/// no parameters). Returns the lowercase form.
pub fn validate_mime(mime: &str) -> Option<String> {
    if mime.is_empty() || mime.len() > MAX_RICH_MIME_BYTES {
        return None;
    }
    let (r#type, subtype) = mime.split_once('/')?;
    if !is_token(r#type) || !is_token(subtype) {
        return None;
    }
    Some(mime.to_ascii_lowercase())
}

fn is_token(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= MAX_RICH_MIME_BYTES / 2
        && part.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-' | b'*'
                )
        })
}

/// Sanitize a multiplexer `id`: keep `[a-zA-Z0-9-_+.]`, drop the rest,
/// truncate to [`MAX_RICH_ID_CHARS`].
pub fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'))
        .take(MAX_RICH_ID_CHARS)
        .collect()
}

/// Decode one base64 credential field (`pw`/`name`/`mime`): must be valid
/// UTF-8 within [`MAX_RICH_CREDENTIAL_BYTES`] decoded bytes.
fn decode_credential_field(encoded: &str) -> Option<String> {
    let decoded = decode_clipboard_base64(encoded)?;
    if decoded.len() > MAX_RICH_CREDENTIAL_BYTES {
        return None;
    }
    String::from_utf8(decoded).ok()
}

/// Parsed `key=value` metadata of one OSC 5522 packet.
#[derive(Debug, Default)]
pub(crate) struct RichMetadata {
    pub(crate) packet_type: Option<String>,
    pub(crate) location: RichClipboardLocation,
    pub(crate) password: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) mime: Option<String>,
    pub(crate) id: String,
    pub(crate) malformed_credential: bool,
}

/// Split `metadata` on `:` into key/value pairs. Unknown keys are ignored
/// for forward compatibility. A present-but-undecodable `pw`/`name`/`mime`
/// marks the packet malformed instead of silently dropping the value.
pub(crate) fn parse_metadata(metadata: &str) -> RichMetadata {
    let mut parsed = RichMetadata::default();
    if metadata.is_empty() {
        return parsed;
    }
    for pair in metadata.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "type" => {
                if parsed.packet_type.is_none() {
                    parsed.packet_type = Some(value.to_string());
                }
            }
            "loc" => {
                if let Some(location) = RichClipboardLocation::from_loc_value(value) {
                    parsed.location = location;
                } else if value != "clipboard" {
                    // Unknown locations are never silently remapped to the
                    // system clipboard; the packet is malformed instead.
                    parsed.malformed_credential = true;
                }
            }
            "pw" => match decode_credential_field(value) {
                Some(password) => parsed.password = Some(password),
                None => parsed.malformed_credential = true,
            },
            "name" => match decode_credential_field(value) {
                Some(name) => parsed.name = Some(name),
                None => parsed.malformed_credential = true,
            },
            "mime" => match decode_credential_field(value) {
                Some(mime) => parsed.mime = Some(mime),
                None => parsed.malformed_credential = true,
            },
            "id" if parsed.id.is_empty() => {
                parsed.id = sanitize_id(value);
            }
            "id" => {}
            // `status` only ever flows terminal->app; anything else unknown
            // is ignored so newer senders keep working.
            _ => {}
        }
    }
    parsed
}

impl RichMetadata {
    /// Credential counts only when BOTH halves are present and non-empty,
    /// per the spec (password without a name ≡ no password).
    pub(crate) fn credential(&self) -> Option<RichCredential> {
        match (&self.password, &self.name) {
            (Some(password), Some(name)) if !password.is_empty() && !name.is_empty() => {
                Some(RichCredential {
                    password: password.clone(),
                    name: name.clone(),
                })
            }
            _ => None,
        }
    }
}

/// A read-request payload that is not usable base64, not UTF-8, not a MIME
/// list, or not the bare `.` listing probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedReadPayload;

/// Decode a read-request payload into MIME types, or `None` for the `.`
/// listing probe. `Err(MalformedReadPayload)` means malformed input (bad
/// base64, bad MIME, or too many types).
pub fn parse_read_payload(payload: &str) -> Result<Option<Vec<String>>, MalformedReadPayload> {
    let decoded = decode_clipboard_base64(payload).ok_or(MalformedReadPayload)?;
    let text = std::str::from_utf8(&decoded).map_err(|_| MalformedReadPayload)?;
    let mut parts = text.split_ascii_whitespace().peekable();
    if parts.peek() == Some(&".") {
        parts.next();
        if parts.next().is_none() {
            return Ok(None);
        }
        return Err(MalformedReadPayload);
    }
    let mut mimes = Vec::new();
    for part in text.split_ascii_whitespace() {
        if mimes.len() >= MAX_RICH_MIME_COUNT {
            return Err(MalformedReadPayload);
        }
        mimes.push(validate_mime(part).ok_or(MalformedReadPayload)?);
    }
    if mimes.is_empty() {
        return Err(MalformedReadPayload);
    }
    Ok(Some(mimes))
}

/// Mint an unguessable one-time paste token (hex, 128 bits).
pub fn mint_paste_password() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Practically unreachable (OS RNG failure); fall back to a hashed
        // time/counter mix rather than minting a constant. Still unique per
        // call, but treat any fallback token as short-lived.
        use std::hash::{Hash, Hasher};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
        let mixed = hasher.finish();
        bytes[..8].copy_from_slice(&mixed.to_le_bytes());
        bytes[8..].copy_from_slice(&(!mixed).to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn id_suffix(id: &str) -> String {
    // Defense in depth: ids are sanitized at parse time, but encoding paths
    // also serve frontend-supplied values.
    let id = sanitize_id(id);
    if id.is_empty() {
        String::new()
    } else {
        format!(":id={id}")
    }
}

fn loc_prefix(location: RichClipboardLocation) -> &'static str {
    match location {
        RichClipboardLocation::Clipboard => "",
        RichClipboardLocation::Primary => ":loc=primary",
    }
}

/// Encode one terminal->app packet: `ESC ] 5522 ; metadata ; payload ST`.
fn encode_packet(metadata: String, payload_base64: &str) -> Vec<u8> {
    format!("\x1b]5522;{metadata};{payload_base64}\x1b\\").into_bytes()
}

/// Encode a complete read answer: `OK`, one or more `DATA` chunks per MIME
/// type (raw chunks of at most 4096 bytes, padded base64), then `DONE`.
pub fn encode_read_responses(id: &str, items: &[RichClipboardItem]) -> Vec<Vec<u8>> {
    let suffix = id_suffix(id);
    let mut out = vec![encode_packet(format!("type=read:status=OK{suffix}"), "")];
    for item in items {
        let encoded_mime = PADDED_BASE64.encode(item.mime.as_bytes());
        if item.data.is_empty() {
            out.push(encode_packet(
                format!("type=read:status=DATA:mime={encoded_mime}{suffix}"),
                "",
            ));
            continue;
        }
        for chunk in item.data.chunks(MAX_RICH_CHUNK_BYTES) {
            out.push(encode_packet(
                format!("type=read:status=DATA:mime={encoded_mime}{suffix}"),
                &PADDED_BASE64.encode(chunk),
            ));
        }
    }
    out.push(encode_packet(format!("type=read:status=DONE{suffix}"), ""));
    out
}

/// Encode a MIME-list answer: one `DATA` packet with `mime=.` carrying the
/// space-separated type list.
pub fn encode_list_responses(id: &str, types: &[String]) -> Vec<Vec<u8>> {
    let suffix = id_suffix(id);
    let encoded_mime = PADDED_BASE64.encode(b".");
    let mut clean: Vec<&str> = types
        .iter()
        .filter_map(|mime| validate_mime(mime).is_some().then_some(mime.as_str()))
        .collect();
    clean.sort();
    clean.dedup();
    let payload = PADDED_BASE64.encode(clean.join(" ").as_bytes());
    vec![
        encode_packet(format!("type=read:status=OK{suffix}"), ""),
        encode_packet(
            format!("type=read:status=DATA:mime={encoded_mime}{suffix}"),
            &payload,
        ),
        encode_packet(format!("type=read:status=DONE{suffix}"), ""),
    ]
}

/// Encode a read-side error (replaces the `OK` packet; no `DONE` follows).
pub fn encode_read_error(id: &str, status: RichClipboardStatus) -> Vec<u8> {
    let suffix = id_suffix(id);
    encode_packet(format!("type=read:status={}{suffix}", status.as_str()), "")
}

/// Encode a write-side completion or error reply.
pub fn encode_write_status(id: &str, status: Option<RichClipboardStatus>) -> Vec<u8> {
    let suffix = id_suffix(id);
    let status = status.map_or("DONE", RichClipboardStatus::as_str);
    encode_packet(format!("type=write:status={status}{suffix}"), "")
}

/// Encode an unsolicited 5522-mode paste notification: `OK`, one `DATA`
/// with `mime=.` listing available types, then `DONE`. All three carry the
/// single-use `pw`; `loc=primary` marks primary-selection pastes.
pub fn encode_paste_notification(
    location: RichClipboardLocation,
    types: &[String],
    password: &str,
) -> Vec<Vec<u8>> {
    let loc = loc_prefix(location);
    let encoded_pw = PADDED_BASE64.encode(password.as_bytes());
    let pw = format!(":pw={encoded_pw}");
    let encoded_mime = PADDED_BASE64.encode(b".");
    let mut clean: Vec<&str> = types
        .iter()
        .filter_map(|mime| validate_mime(mime).is_some().then_some(mime.as_str()))
        .collect();
    clean.sort();
    clean.dedup();
    let payload = PADDED_BASE64.encode(clean.join(" ").as_bytes());
    vec![
        encode_packet(format!("type=read:status=OK{loc}{pw}"), ""),
        encode_packet(
            format!("type=read:status=DATA:mime={encoded_mime}{loc}{pw}"),
            &payload,
        ),
        encode_packet(format!("type=read:status=DONE{loc}{pw}"), ""),
    ]
}

/// Check a presented credential against exact-token consents and live
/// one-time paste tokens. Consumes a matching paste token (single-use).
/// Returns true when the read is pre-authorized.
pub fn resolve_read_consent(
    credential: Option<&RichCredential>,
    location: RichClipboardLocation,
    authorized: &[(String, String)],
    paste_passwords: &mut VecDeque<PastePassword>,
) -> bool {
    let Some(credential) = credential else {
        return false;
    };
    if authorized
        .iter()
        .any(|(password, name)| password == &credential.password && name == &credential.name)
    {
        return true;
    }
    let now = Instant::now();
    paste_passwords.retain(|entry| now.duration_since(entry.created) < PASTE_PASSWORD_TTL);
    if let Some(index) = paste_passwords
        .iter()
        .position(|entry| entry.password == credential.password && entry.location == location)
    {
        paste_passwords.remove(index);
        return true;
    }
    false
}

/// Remember an exact-token consent, evicting the oldest past the cap.
/// Returns false when the pair is not storable (empty halves).
pub fn remember_consent(
    authorized: &mut Vec<(String, String)>,
    password: &str,
    name: &str,
) -> bool {
    if password.is_empty()
        || name.is_empty()
        || password.len() > MAX_RICH_CREDENTIAL_BYTES
        || name.len() > MAX_RICH_CREDENTIAL_BYTES
    {
        return false;
    }
    if !authorized
        .iter()
        .any(|(stored_pw, stored_name)| stored_pw == password && stored_name == name)
    {
        if authorized.len() >= MAX_AUTHORIZED_CLIPBOARD_PASSWORDS {
            authorized.remove(0);
        }
        authorized.push((password.to_string(), name.to_string()));
    }
    true
}

/// Mint and retain one one-time paste token, evicting the oldest past the cap.
pub fn remember_paste_password(
    paste_passwords: &mut VecDeque<PastePassword>,
    location: RichClipboardLocation,
) -> String {
    let password = mint_paste_password();
    if paste_passwords.len() >= MAX_PASTE_PASSWORDS {
        paste_passwords.pop_front();
    }
    paste_passwords.push_back(PastePassword {
        password: password.clone(),
        location,
        created: Instant::now(),
    });
    password
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_validation_accepts_common_types_and_normalizes_case() {
        assert_eq!(validate_mime("text/plain"), Some("text/plain".to_string()));
        assert_eq!(validate_mime("Text/HTML"), Some("text/html".to_string()));
        assert_eq!(validate_mime("image/png"), Some("image/png".to_string()));
        assert_eq!(
            validate_mime("application/vnd.custom+json"),
            Some("application/vnd.custom+json".to_string())
        );
    }

    #[test]
    fn mime_validation_rejects_parameters_and_garbage() {
        assert_eq!(validate_mime(""), None);
        assert_eq!(validate_mime("textplain"), None);
        assert_eq!(validate_mime("text/"), None);
        assert_eq!(validate_mime("/plain"), None);
        assert_eq!(validate_mime("text/plain;charset=utf-8"), None);
        assert_eq!(validate_mime("text/pl ain"), None);
        assert_eq!(validate_mime("text/pl;ain"), None);
        assert_eq!(validate_mime("."), None);
        assert_eq!(validate_mime(&"a".repeat(300)), None);
    }

    #[test]
    fn id_sanitizing_strips_outside_charset_and_truncates() {
        assert_eq!(sanitize_id("abc-12_XY+.z"), "abc-12_XY+.z");
        assert_eq!(sanitize_id("a;b:c d/e\\f"), "abcdef");
        assert_eq!(sanitize_id(""), "");
        assert_eq!(sanitize_id(&"a".repeat(200)).len(), MAX_RICH_ID_CHARS);
    }

    #[test]
    fn metadata_parsing_keeps_known_keys_and_ignores_unknown() {
        let pw = PADDED_BASE64.encode(b"secret");
        let name = PADDED_BASE64.encode("My App");
        let metadata = parse_metadata(&format!(
            "type=read:loc=primary:pw={pw}:name={name}:id=a b!c:future=1"
        ));
        assert_eq!(metadata.packet_type.as_deref(), Some("read"));
        assert_eq!(metadata.location, RichClipboardLocation::Primary);
        assert_eq!(
            metadata.credential(),
            Some(RichCredential {
                password: "secret".to_string(),
                name: "My App".to_string(),
            })
        );
        // `id` keeps only the multiplexer charset.
        assert_eq!(metadata.id, "abc");
        assert!(!metadata.malformed_credential);
    }

    #[test]
    fn password_without_name_counts_as_no_credential() {
        let pw = PADDED_BASE64.encode(b"secret");
        let metadata = parse_metadata(&format!("type=read:pw={pw}"));
        assert_eq!(metadata.credential(), None);
        let metadata = parse_metadata(&format!(
            "type=read:pw={pw}:name={}",
            PADDED_BASE64.encode(b"")
        ));
        assert_eq!(metadata.credential(), None);
    }

    #[test]
    fn undecodable_credential_fields_flag_malformed() {
        let metadata = parse_metadata("type=read:pw=!!!:name=also-bad!!!");
        assert!(metadata.malformed_credential);
        assert_eq!(metadata.credential(), None);
    }

    #[test]
    fn read_payload_parses_lists_and_listing_probe() {
        let payload = PADDED_BASE64.encode(b"text/plain image/png");
        assert_eq!(
            parse_read_payload(&payload),
            Ok(Some(vec![
                "text/plain".to_string(),
                "image/png".to_string()
            ]))
        );
        let probe = PADDED_BASE64.encode(b".");
        assert_eq!(parse_read_payload(&probe), Ok(None));
        // Probe mixed with types is malformed.
        let mixed = PADDED_BASE64.encode(b". text/plain");
        assert_eq!(parse_read_payload(&mixed), Err(MalformedReadPayload));
        assert_eq!(parse_read_payload("!!!"), Err(MalformedReadPayload));
        assert_eq!(
            parse_read_payload(&PADDED_BASE64.encode(b"text/no way")),
            Err(MalformedReadPayload),
        );
        assert_eq!(
            parse_read_payload(&PADDED_BASE64.encode(b"")),
            Err(MalformedReadPayload)
        );
    }

    #[test]
    fn read_payload_rejects_too_many_types() {
        let many = (0..MAX_RICH_MIME_COUNT + 1)
            .map(|index| format!("type/x{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            parse_read_payload(&PADDED_BASE64.encode(many)),
            Err(MalformedReadPayload)
        );
    }

    #[test]
    fn write_session_accumulates_chunks_and_resolves_aliases() {
        let mut session = RichWriteSession::new(RichClipboardLocation::Clipboard, String::new());
        session.add_chunk("text/plain", b"hello ".to_vec()).unwrap();
        session.add_chunk("text/plain", b"world".to_vec()).unwrap();
        session
            .add_alias("text/plain", vec!["text/x-alias".to_string()])
            .unwrap();
        session
            .add_alias("missing/type", vec!["text/x-ghost".to_string()])
            .unwrap();
        let (items, aliases) = session.finish();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].data, b"hello world");
        // Only the alias whose target arrived survives.
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].target, "text/plain");
    }

    #[test]
    fn write_session_enforces_bounds() {
        let mut session = RichWriteSession::new(RichClipboardLocation::Clipboard, String::new());
        assert_eq!(
            session.add_chunk("text/plain", vec![0u8; MAX_RICH_CHUNK_BYTES + 1]),
            Err(RichClipboardStatus::Inval)
        );
        assert_eq!(
            session.add_chunk("not a mime", vec![1]),
            Err(RichClipboardStatus::Inval)
        );
        for index in 0..MAX_RICH_WRITE_ITEMS {
            session
                .add_chunk(&format!("type/x{index}"), vec![1])
                .unwrap();
        }
        assert_eq!(
            session.add_chunk("type/overflow", vec![1]),
            Err(RichClipboardStatus::Inval)
        );
    }

    #[test]
    fn read_replies_chunk_at_4096_and_echo_id() {
        let items = vec![RichClipboardItem {
            mime: "text/plain".to_string(),
            data: vec![b'x'; MAX_RICH_CHUNK_BYTES + 10],
        }];
        let replies = encode_read_responses("a/b!c", &items);
        // IDs are echoed sanitized: the caller sanitizes before encoding.
        assert_eq!(replies.len(), 4);
        assert!(replies[0].starts_with(b"\x1b]5522;type=read:status=OK:id=abc;"));
        assert!(replies[3].starts_with(b"\x1b]5522;type=read:status=DONE:id=abc;"));
        for reply in &replies[1..3] {
            assert!(reply.windows(5).any(|w| w == b"=DATA"));
        }
        // Every packet is ST-terminated.
        for reply in &replies {
            assert!(reply.ends_with(b"\x1b\\"));
        }
    }

    #[test]
    fn consent_exact_match_and_paste_single_use() {
        let mut authorized = Vec::new();
        assert!(remember_consent(&mut authorized, "pw1", "App"));
        assert!(!remember_consent(&mut authorized, "", "App"));
        let credential = RichCredential {
            password: "pw1".to_string(),
            name: "App".to_string(),
        };
        let mut pastes = VecDeque::new();
        assert!(resolve_read_consent(
            Some(&credential),
            RichClipboardLocation::Clipboard,
            &authorized,
            &mut pastes
        ));
        // Wrong name does not match.
        let wrong = RichCredential {
            password: "pw1".to_string(),
            name: "Other".to_string(),
        };
        assert!(!resolve_read_consent(
            Some(&wrong),
            RichClipboardLocation::Clipboard,
            &authorized,
            &mut pastes
        ));

        // One-time paste token: usable once, location-scoped.
        let password = remember_paste_password(&mut pastes, RichClipboardLocation::Primary);
        let paste_credential = RichCredential {
            password: password.clone(),
            name: "Paste event".to_string(),
        };
        assert!(!resolve_read_consent(
            Some(&paste_credential),
            RichClipboardLocation::Clipboard,
            &authorized,
            &mut pastes
        ));
        assert!(resolve_read_consent(
            Some(&paste_credential),
            RichClipboardLocation::Primary,
            &authorized,
            &mut pastes
        ));
        assert!(!resolve_read_consent(
            Some(&paste_credential),
            RichClipboardLocation::Primary,
            &authorized,
            &mut pastes
        ));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::screen::{ClipboardOperation, ScreenConfig};
    use crate::term::{Terminal, TerminalEvent};

    fn b64(data: &[u8]) -> String {
        PADDED_BASE64.encode(data)
    }

    fn collect(input: &[u8]) -> (Terminal, Vec<TerminalEvent>, Vec<Vec<u8>>) {
        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        let (events, responses) = term.process_collecting(input);
        (term, events, responses)
    }

    fn rich_ops(events: &[TerminalEvent]) -> Vec<RichClipboardOperation> {
        events
            .iter()
            .filter_map(|event| match event {
                TerminalEvent::RichClipboardRequest(op) => Some(op.clone()),
                _ => None,
            })
            .collect()
    }

    fn write_packets(mime: &str, chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut packets = vec![b"\x1b]5522;type=write\x1b\\".to_vec()];
        let encoded_mime = b64(mime.as_bytes());
        for chunk in chunks {
            packets.push(
                format!(
                    "\x1b]5522;type=wdata:mime={encoded_mime};{}\x1b\\",
                    b64(chunk)
                )
                .into_bytes(),
            );
        }
        packets
    }

    #[test]
    fn write_session_sets_typed_data_with_done_reply() {
        let mut input = write_packets("text/plain", &[b"hello ", b"world"]);
        input.extend(
            write_packets("image/png", &[&[0u8; 100]])
                .into_iter()
                .skip(1),
        );
        let alias_target = b64(b"text/plain");
        let alias_list = b64(b"text/x-alias");
        input.push(
            format!("\x1b]5522;type=walias:mime={alias_target};{alias_list}\x1b\\").into_bytes(),
        );
        input.push(b"\x1b]5522;type=wdata\x1b\\".to_vec());

        let (term, events, responses) = collect(&input.concat());
        assert!(term.screen().has_rich_clipboard_ops() == rich_ops(&events).is_empty());
        let ops = rich_ops(&events);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            RichClipboardOperation::Write {
                location,
                items,
                aliases,
                ..
            } => {
                assert_eq!(*location, RichClipboardLocation::Clipboard);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].mime, "text/plain");
                assert_eq!(items[0].data, b"hello world");
                assert_eq!(items[1].mime, "image/png");
                assert_eq!(aliases.len(), 1);
                assert_eq!(aliases[0].target, "text/plain");
            }
            RichClipboardOperation::Read { .. } => panic!("expected a write"),
        }
        // The terminal acknowledged completion; the frontend op is separate.
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b]5522;type=write:status=DONE;\x1b\\");
        let _ = term;
    }

    #[test]
    fn write_error_replies_once_then_ignores_until_new_write() {
        let mime = b64(b"text/plain");
        let input = [
            b"\x1b]5522;type=write\x1b\\".to_vec(),
            format!("\x1b]5522;type=wdata:mime={mime};!!!not-base64!!!\x1b\\").into_bytes(),
            // Ignored: the failed session only accepts a fresh `type=write`.
            format!("\x1b]5522;type=wdata:mime={mime};{}\x1b\\", b64(b"late")).into_bytes(),
            b"\x1b]5522;type=wdata\x1b\\".to_vec(),
        ]
        .concat();
        let (_, events, responses) = collect(&input);
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0],
            b"\x1b]5522;type=write:status=EINVAL;\x1b\\".to_vec()
        );
        assert!(rich_ops(&events).is_empty());

        // A fresh session recovers.
        let mut input = write_packets("text/plain", &[b"ok"]);
        input.push(b"\x1b]5522;type=wdata\x1b\\".to_vec());
        let (_, events, responses) = collect(&input.concat());
        assert_eq!(responses.len(), 1);
        assert_eq!(rich_ops(&events).len(), 1);
    }

    #[test]
    fn oversize_chunk_fails_the_session() {
        let mime = b64(b"text/plain");
        let big = vec![b'x'; MAX_RICH_CHUNK_BYTES + 1];
        let input = [
            b"\x1b]5522;type=write\x1b\\".to_vec(),
            format!("\x1b]5522;type=wdata:mime={mime};{}\x1b\\", b64(&big)).into_bytes(),
        ]
        .concat();
        let (_, _, responses) = collect(&input);
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=write:status=EINVAL;"));
    }

    #[test]
    fn data_read_needs_prompt_then_answers_through_completion() {
        let payload = b64(b"text/plain");
        let (_, events, responses) =
            collect(format!("\x1b]5522;type=read;{payload}\x1b\\").as_bytes());
        // No immediate reply: the frontend must supply native data.
        assert!(responses.is_empty());
        let ops = rich_ops(&events);
        assert_eq!(ops.len(), 1);
        let request = match &ops[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert!(!request.list_only);
        assert!(!request.pre_authorized);
        assert_eq!(request.mimes, vec!["text/plain".to_string()]);

        // Answering with native data produces OK/DATA/DONE.
        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        term.process_collecting(format!("\x1b]5522;type=read;{payload}\x1b\\").as_bytes());
        let replies = term.screen_mut().complete_rich_clipboard_read(
            &request.id,
            request.location,
            &[RichClipboardItem {
                mime: "text/plain".to_string(),
                data: b"native".to_vec(),
            }],
        );
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0], b"\x1b]5522;type=read:status=OK;\x1b\\".to_vec());
        assert!(replies[1].starts_with(b"\x1b]5522;type=read:status=DATA:mime="));
        assert!(replies[1].ends_with(format!(";{}\x1b\\", b64(b"native")).as_bytes()));
        assert_eq!(
            replies[2],
            b"\x1b]5522;type=read:status=DONE;\x1b\\".to_vec()
        );
        // A duplicate answer has nothing left to answer.
        assert!(term
            .screen_mut()
            .complete_rich_clipboard_read(&request.id, request.location, &[])
            .is_empty());
    }

    #[test]
    fn mime_list_read_needs_no_prompt() {
        let payload = b64(b".");
        let (_, events, responses) =
            collect(format!("\x1b]5522;type=read;{payload}\x1b\\").as_bytes());
        assert!(responses.is_empty());
        let ops = rich_ops(&events);
        let request = match &ops[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert!(request.list_only);
        assert!(request.pre_authorized);

        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        term.process_collecting(format!("\x1b]5522;type=read;{payload}\x1b\\").as_bytes());
        let replies = term.screen_mut().complete_rich_clipboard_list(
            &request.id,
            request.location,
            &["text/plain".to_string(), "image/png".to_string()],
        );
        assert_eq!(replies.len(), 3);
        // The listing travels as the payload of the `.` MIME type.
        let list_mime = b64(b".");
        assert!(replies[1]
            .starts_with(format!("\x1b]5522;type=read:status=DATA:mime={list_mime};").as_bytes()));
    }

    #[test]
    fn exact_token_consent_preauthorizes_later_reads() {
        let pw = b64(b"app-secret");
        let name = b64(b"My Editor");
        let payload = b64(b"text/plain");
        let read = format!("\x1b]5522;type=read:pw={pw}:name={name};{payload}\x1b\\");

        let (_, events, _) = collect(read.as_bytes());
        let first = match &rich_ops(&events)[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert!(!first.pre_authorized);

        // Denial answers EPERM.
        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        term.process_collecting(read.as_bytes());
        let denied = term.screen_mut().fail_rich_clipboard_read(
            &first.id,
            first.location,
            RichClipboardStatus::Perm,
        );
        assert_eq!(
            denied,
            vec![b"\x1b]5522;type=read:status=EPERM;\x1b\\".to_vec()]
        );

        // "Always allow" remembers the exact token; the next identical read
        // is pre-authorized without a prompt.
        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        term.process_collecting(read.as_bytes());
        assert!(term
            .screen_mut()
            .authorize_rich_clipboard("app-secret", "My Editor"));
        let (_, events, _) = {
            let (events, _) = term.process_collecting(read.as_bytes());
            ((), events, ())
        };
        let second = match &rich_ops(&events)[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert!(second.pre_authorized);

        // A different password does not ride along.
        let other_pw = b64(b"other-secret");
        let other = format!("\x1b]5522;type=read:pw={other_pw}:name={name};{payload}\x1b\\");
        let (_, events, _) = collect(other.as_bytes());
        match &rich_ops(&events)[0] {
            RichClipboardOperation::Read { request } => assert!(!request.pre_authorized),
            _ => panic!("expected a read"),
        }
    }

    #[test]
    fn paste_event_token_redeems_once_for_matching_reads() {
        use crate::screen::Screen;

        let mut screen = Screen::new(80, 24, ScreenConfig::default());
        // Disabled by default: nothing is minted and nothing is sent.
        assert!(screen
            .mint_rich_paste_notification(
                RichClipboardLocation::Clipboard,
                &["text/plain".to_string()]
            )
            .is_empty());

        screen.modes.rich_clipboard_paste_events = true;
        let notification = screen.mint_rich_paste_notification(
            RichClipboardLocation::Clipboard,
            &["text/plain".to_string()],
        );
        assert_eq!(notification.len(), 3);
        // Extract the single-use password from the OK packet.
        let ok = String::from_utf8(notification[0].clone()).unwrap();
        let encoded_pw = ok
            .split(":pw=")
            .nth(1)
            .unwrap()
            .trim_end_matches(';')
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let password = String::from_utf8(PADDED_BASE64.decode(&encoded_pw).unwrap()).unwrap();
        assert!(!password.is_empty());

        let name = b64(b"Paste event");
        let pw = b64(password.as_bytes());
        let payload = b64(b"text/plain");
        let read = format!("\x1b]5522;type=read:pw={pw}:name={name};{payload}\x1b\\");
        let mut parser = crate::parser::Parser::new();
        parser.parse(&mut screen, read.as_bytes());
        let ops: Vec<_> = screen.take_rich_clipboard_ops();
        let request = match &ops[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert!(request.pre_authorized);

        // Single-use: replaying the same token prompts again.
        parser.parse(&mut screen, read.as_bytes());
        let ops: Vec<_> = screen.take_rich_clipboard_ops();
        match &ops[0] {
            RichClipboardOperation::Read { request } => assert!(!request.pre_authorized),
            _ => panic!("expected a read"),
        }
    }

    #[test]
    fn decrqm_reports_paste_event_mode() {
        use crate::parser::Parser;
        use crate::screen::Screen;

        let mut screen = Screen::new(80, 24, ScreenConfig::default());
        let mut parser = Parser::new();
        parser.parse(&mut screen, b"\x1b[?5522$p");
        assert_eq!(
            screen.take_pending_responses(),
            vec![b"\x1b[?5522;2$y".to_vec()]
        );
        parser.parse(&mut screen, b"\x1b[?5522h\x1b[?5522$p");
        assert!(screen.modes.rich_clipboard_paste_events);
        assert_eq!(
            screen.take_pending_responses(),
            vec![b"\x1b[?5522;1$y".to_vec()]
        );
        parser.parse(&mut screen, b"\x1b[?5522l");
        assert!(!screen.modes.rich_clipboard_paste_events);
    }

    #[test]
    fn hostile_packets_are_rejected_without_state_change() {
        // Unknown packet type: ignored silently.
        let (_, events, responses) = collect(b"\x1b]5522;type=selfdestruct;\x1b\\");
        assert!(responses.is_empty());
        assert!(rich_ops(&events).is_empty());

        // Read with bad base64 payload.
        let (_, _, responses) = collect(b"\x1b]5522;type=read;!!!\x1b\\");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=read:status=EINVAL;"));

        // Read with an invalid MIME type.
        let payload = b64(b"text/ok bad//mime");
        let (_, _, responses) = collect(format!("\x1b]5522;type=read;{payload}\x1b\\").as_bytes());
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=read:status=EINVAL;"));

        // Read with an oversize credential.
        let big_pw = b64(&vec![b'p'; MAX_RICH_CREDENTIAL_BYTES + 1]);
        let name = b64(b"App");
        let payload = b64(b"text/plain");
        let (_, _, responses) = collect(
            format!("\x1b]5522;type=read:pw={big_pw}:name={name};{payload}\x1b\\").as_bytes(),
        );
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=read:status=EINVAL;"));

        // Read with too many MIME types.
        let many = (0..MAX_RICH_MIME_COUNT + 1)
            .map(|index| format!("type/x{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (_, _, responses) =
            collect(format!("\x1b]5522;type=read;{}\x1b\\", b64(many.as_bytes())).as_bytes());
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=read:status=EINVAL;"));

        // Data outside any write session is ignored (no reply, no op).
        let mime = b64(b"text/plain");
        let (_, events, responses) = collect(
            format!("\x1b]5522;type=wdata:mime={mime};{}\x1b\\", b64(b"orphan")).as_bytes(),
        );
        assert!(responses.is_empty());
        assert!(rich_ops(&events).is_empty());

        // Unknown `loc` values never remap to the system clipboard.
        let (_, _, responses) = collect(
            format!(
                "\x1b]5522;type=read:loc=evilcorp;{payload}\x1b\\",
                payload = b64(b".")
            )
            .as_bytes(),
        );
        assert_eq!(responses.len(), 1);
        assert!(responses[0].starts_with(b"\x1b]5522;type=read:status=EINVAL;"));
    }

    #[test]
    fn reply_ids_echo_sanitized() {
        let payload = b64(b"text/plain");
        // `!` and space are framing-safe but outside the id charset.
        let (_, events, _) =
            collect(format!("\x1b]5522;type=read:id=a!b c;{payload}\x1b\\").as_bytes());
        let request = match &rich_ops(&events)[0] {
            RichClipboardOperation::Read { request } => request.clone(),
            _ => panic!("expected a read"),
        };
        assert_eq!(request.id, "abc");

        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        term.process_collecting(format!("\x1b]5522;type=read:id=a!b c;{payload}\x1b\\").as_bytes());
        let denied = term.screen_mut().fail_rich_clipboard_read(
            "abc",
            RichClipboardLocation::Clipboard,
            RichClipboardStatus::Perm,
        );
        assert_eq!(
            denied,
            vec![b"\x1b]5522;type=read:status=EPERM:id=abc;\x1b\\".to_vec()]
        );
    }

    #[test]
    fn pending_reads_are_bounded() {
        let payload = b64(b"text/plain");
        let mut term = Terminal::new(80, 24, ScreenConfig::default());
        for index in 0..MAX_PENDING_RICH_READS + 2 {
            let input = format!("\x1b]5522;type=read:id=r{index};{payload}\x1b\\").into_bytes();
            term.process_collecting(&input);
        }
        // The oldest entry was evicted: answering it finds nothing.
        assert!(term
            .screen_mut()
            .complete_rich_clipboard_list("r0", RichClipboardLocation::Clipboard, &[],)
            .is_empty());
        // The newest is still answerable.
        let last = format!("r{}", MAX_PENDING_RICH_READS + 1);
        assert!(!term
            .screen_mut()
            .complete_rich_clipboard_read(
                &last,
                RichClipboardLocation::Clipboard,
                &[RichClipboardItem {
                    mime: "text/plain".to_string(),
                    data: b"x".to_vec(),
                }]
            )
            .is_empty());
    }

    #[test]
    fn osc52_text_behavior_is_unchanged() {
        use base64::Engine;

        // OSC 52 set still queues the same plain operation.
        let data = PADDED_BASE64.encode(b"plain text");
        let (_, events, _) = collect(format!("\x1b]52;c;{data}\x1b\\").as_bytes());
        let sets: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TerminalEvent::ClipboardRequest(op) => Some(op.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sets.len(), 1);
        match &sets[0] {
            ClipboardOperation::Set { selection, data } => {
                assert_eq!(*selection, crate::screen::ClipboardSelection::Clipboard);
                assert_eq!(data, b"plain text");
            }
            _ => panic!("expected an OSC 52 set"),
        }
        // OSC 52 queries still queue; OSC 5522 traffic is separate.
        let (_, events, _) = collect(b"\x1b]52;c;?\x1b\\");
        assert!(
            events.iter().any(|event| matches!(
                event,
                TerminalEvent::ClipboardRequest(ClipboardOperation::Query { .. })
            )),
            "OSC 52 query must still queue"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, TerminalEvent::RichClipboardRequest(_))),
            "OSC 52 must never produce rich operations"
        );
    }
}
