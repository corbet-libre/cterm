//! Kitty-compatible transfer-bypass credential verification (mechanism only).
//!
//! This module mirrors Kitty 0.48.2's `check_bypass`
//! (`kitty/file_transmission.py`) and the client-side `Encrypt_data`
//! (`tools/crypto/crypto.go`, used by the transfer kitten):
//!
//! - `sha256:` arm: `sha256(request_id + ";" + password)` hex digest compared
//!   in constant time. An unset/empty configured password never verifies.
//! - `kitty-1:` arm: a JSON envelope (`pubkey`/`iv`/`tag`/`encrypted`, all
//!   RFC 1924 base85) whose AES-256-GCM plaintext must be
//!   `<timestamp_ns>:<request_id>;<password>` with the timestamp inside ±5
//!   minutes. The AES key is `SHA256(X25519(private, peer_public))`, matching
//!   Kitty's `EllipticCurveKey.derive_secret` (ECDH + SHA-256) and OpenSSL
//!   AES-256-GCM with a 12-byte nonce, a 16-byte tag, and no AAD.
//!
//! Deliberate fail-closed deviations from Kitty (all stricter, none looser):
//!
//! - Kitty's `sha256` arm already refuses an empty configured password; the
//!   same rule is extended to the `kitty-1` arm, where Kitty would otherwise
//!   compare against `<request_id>;` with an empty password.
//! - GCM tags must be exactly 16 bytes (Kitty/OpenSSL accept other lengths).
//! - Timestamps must be plain ASCII digits (Kitty's `int()` also accepts
//!   signs, underscores, and surrounding whitespace).
//! - Envelopes longer than [`MAX_BYPASS_WIRE_BYTES`] are refused outright,
//!   mirroring the codec's 512-byte bound on the decoded `pw` field.
//! - A degenerate peer public key (X25519 contributory failure) refuses
//!   instead of deriving the all-zero secret.
//!
//! Verification alone never authorizes anything: see
//! [`crate::kitty_file_transfer::evaluate_transfer_bypass_with_policy`],
//! which keeps resolving to `Prompt` unless an explicit opt-in policy is
//! configured. Secrets are never logged; audit rows carry only
//! [`hash_bypass_token`].

use purecrypto::cipher::{Aes256, Aes256Gcm};
use purecrypto::ct::ConstantTimeEq;
use purecrypto::ec::x25519::X25519PrivateKey;
use sha2::{Digest, Sha256};

/// Acceptable envelope age around `now_ns`, in nanoseconds (±5 minutes,
/// matching Kitty's `abs(delta) > 5 * 60 * 1e9` refusal).
pub const BYPASS_TIMESTAMP_WINDOW_NS: u128 = 5 * 60 * 1_000_000_000;

/// Upper bound on a verified wire credential, mirroring the codec's 512-byte
/// bound on the decoded `pw` field (`MAX_FILE_TRANSFER_SAFE_STRING_BYTES`).
/// Anything longer fails closed before any JSON parsing or decryption.
pub const MAX_BYPASS_WIRE_BYTES: usize = 512;

/// Version prefix of the `KITTY_PUBLIC_KEY` environment value
/// (`<version>:<base85 public key>`), matching Kitty's
/// `RC_ENCRYPTION_PROTOCOL_VERSION = '1'`.
pub const KITTY_PUBLIC_KEY_VERSION: &str = "1";

/// Environment variable through which the terminal advertises its transfer
/// bypass public key to child processes.
///
/// Kitty sets this for every child (`kitty/child.py:385`,
/// `env['KITTY_PUBLIC_KEY'] = boss.encryption_public_key`, where the value is
/// computed once per GUI process in `kitty/boss.py:425` as
/// `f'{RC_ENCRYPTION_PROTOCOL_VERSION}:{base85(public)}'`). The daemon mirrors
/// that contract: one process-lifetime [`TransferBypassKeypair`], advertised
/// verbatim to every local PTY child (see
/// [`TransferBypassKeypair::kitty_public_key_env`]). Children hand the value
/// to transfer kittens, which encrypt `kitty-1:` credentials against it; the
/// private half never leaves the daemon.
pub const KITTY_PUBLIC_KEY_ENV: &str = "KITTY_PUBLIC_KEY";

/// What went wrong while *creating* bypass material (keygen/encryption).
/// Verification itself returns `bool`, exactly like Kitty's `check_bypass`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferBypassError {
    /// OS randomness was unavailable.
    #[error("transfer bypass randomness unavailable: {0}")]
    Random(String),
    /// The terminal public key is degenerate (X25519 contributory failure).
    #[error("transfer bypass terminal public key is degenerate")]
    DegeneratePublicKey,
    /// The envelope could not be serialized.
    #[error("transfer bypass envelope encoding failed: {0}")]
    Encode(String),
}

// ---------------------------------------------------------------------------
// RFC 1924 base85 (Python `base64.b85encode`/`b85decode`)
// ---------------------------------------------------------------------------

const B85_ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// Encode bytes as RFC 1924 base85, exactly like Python's `b85encode`.
pub fn b85encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(4) * 5);
    for chunk in data.chunks(4) {
        let mut value: u32 = 0;
        for (i, &byte) in chunk.iter().enumerate() {
            value |= u32::from(byte) << (8 * (3 - i));
        }
        let mut digits = [0u8; 5];
        let mut rest = value;
        for digit in digits.iter_mut().rev() {
            *digit = (rest % 85) as u8;
            rest /= 85;
        }
        // A short final group keeps only `len + 1` characters (Python pads
        // the missing input bytes with zeros, then truncates the output).
        for &digit in &digits[..chunk.len() + 1] {
            out.push(B85_ALPHABET[usize::from(digit)] as char);
        }
    }
    out
}

/// Decode RFC 1924 base85, like Python's `b85decode`.
///
/// Returns `None` for non-ASCII input, unknown characters, or a group whose
/// value overflows 32 bits (Python raises in all three cases). A short final
/// group is padded with `u` (value 84) and truncated to `len - 1` bytes,
/// matching CPython, which decodes even a lone character to empty output;
/// callers enforce exact field lengths afterwards, so this stays fail-closed.
pub fn b85decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 5 * 4 + 4);
    for chunk in bytes.chunks(5) {
        let mut acc: u64 = 0;
        for &char in chunk {
            let digit = B85_ALPHABET.iter().position(|&a| a == char)? as u64;
            acc = acc * 85 + digit;
        }
        for _ in chunk.len()..5 {
            acc = acc * 85 + 84;
        }
        if acc > u64::from(u32::MAX) {
            return None;
        }
        let big_endian = (acc as u32).to_be_bytes();
        out.extend_from_slice(&big_endian[..chunk.len().saturating_sub(1)]);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Credential encoding / verification
// ---------------------------------------------------------------------------

/// Encode a `sha256:` bypass credential, mirroring Kitty's `encode_bypass`:
/// `sha256:` + hex(`SHA256(request_id + ";" + password)`).
pub fn encode_bypass_credential(request_id: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request_id.as_bytes());
    hasher.update(b";");
    hasher.update(password.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn constant_time_str_eq(left: &str, right: &str) -> bool {
    // Lengths are compared first (public metadata, same as
    // `hmac.compare_digest`); equal-length inputs compare in constant time.
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

fn os_random(buffer: &mut [u8]) -> Result<(), TransferBypassError> {
    getrandom::fill(buffer).map_err(|err| TransferBypassError::Random(err.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct BypassEnvelope {
    pubkey: String,
    iv: String,
    tag: String,
    encrypted: String,
}

fn parse_bypass_timestamp(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok()
}

/// Verify one wire `pw` credential exactly as Kitty's `check_bypass` does.
///
/// `request_id` is the transfer session id (Kitty binds the credential to the
/// peer-chosen `id` field, *not* to any terminal-minted token).
/// `decryption_key` is this terminal's X25519 private key for the `kitty-1`
/// arm; `None` fails that arm closed while leaving the `sha256` arm usable.
/// An empty `password` never verifies on either arm.
pub fn verify_bypass_credential(
    request_id: &str,
    wire_bypass: &str,
    password: &str,
    decryption_key: Option<&[u8; 32]>,
    now_ns: u64,
) -> bool {
    if password.is_empty() || wire_bypass.len() > MAX_BYPASS_WIRE_BYTES {
        return false;
    }
    let Some((protocol, rest)) = wire_bypass.split_once(':') else {
        return false;
    };
    match protocol {
        "kitty-1" => verify_kitty1_envelope(request_id, rest, password, decryption_key, now_ns),
        "sha256" => {
            constant_time_str_eq(&encode_bypass_credential(request_id, password), wire_bypass)
        }
        _ => false,
    }
}

fn verify_kitty1_envelope(
    request_id: &str,
    envelope_json: &str,
    password: &str,
    decryption_key: Option<&[u8; 32]>,
    now_ns: u64,
) -> bool {
    let Some(private) = decryption_key else {
        return false;
    };
    let Ok(envelope) = serde_json::from_str::<BypassEnvelope>(envelope_json) else {
        return false;
    };
    if envelope.pubkey.is_empty() {
        return false;
    }
    let (Some(peer_public), Some(iv), Some(ciphertext), Some(tag)) = (
        b85decode(&envelope.pubkey),
        b85decode(&envelope.iv),
        b85decode(&envelope.encrypted),
        b85decode(&envelope.tag),
    ) else {
        return false;
    };
    let (Ok(peer_public), Ok(tag)) = (<[u8; 32]>::try_from(peer_public), <[u8; 16]>::try_from(tag))
    else {
        return false;
    };
    if iv.is_empty() {
        return false;
    }
    // ECDH + SHA-256, mirroring `derive_secret` (SHA256 default) followed by
    // OpenSSL AES-256-GCM decryption with no AAD.
    let Ok(shared_secret) = X25519PrivateKey::from_bytes(*private).diffie_hellman(&peer_public)
    else {
        return false;
    };
    let aes_key = Sha256::digest(shared_secret);
    let Ok(aes_key) = <[u8; 32]>::try_from(aes_key.as_slice()) else {
        return false;
    };
    let gcm = Aes256Gcm::new(Aes256::new(&aes_key));
    let mut plaintext = ciphertext;
    if gcm.decrypt(&iv, b"", &mut plaintext, &tag).is_err() {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&plaintext) else {
        return false;
    };
    // Kitty's `Encrypt_data` emits `<timestamp_ns>:<request_id>;<password>`.
    let Some((timestamp, payload)) = text.split_once(':') else {
        return false;
    };
    let Some(timestamp) = parse_bypass_timestamp(timestamp) else {
        return false;
    };
    let age = (i128::from(now_ns) - i128::from(timestamp)).unsigned_abs();
    if age > BYPASS_TIMESTAMP_WINDOW_NS {
        return false;
    }
    let mut expected = String::with_capacity(request_id.len() + 1 + password.len());
    expected.push_str(request_id);
    expected.push(';');
    expected.push_str(password);
    constant_time_str_eq(payload, &expected)
}

// ---------------------------------------------------------------------------
// Terminal keypair + client-side encryption (transfer-kitten counterpart)
// ---------------------------------------------------------------------------

/// This terminal's X25519 keypair for the `kitty-1` bypass arm.
///
/// The public half is advertised to PTY children as
/// `KITTY_PUBLIC_KEY=1:<base85>` (see [`TransferBypassKeypair::kitty_public_key_env`]);
/// the private half stays with the daemon and is handed to
/// [`verify_bypass_credential`] as the `decryption_key`. There is deliberately
/// no global instance here: lifetime and advertisement belong to the daemon
/// integration, so a key cannot silently become terminal-wide policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferBypassKeypair {
    private: [u8; 32],
}

impl Drop for TransferBypassKeypair {
    fn drop(&mut self) {
        for byte in self.private.iter_mut() {
            *byte = 0;
        }
        let _ = std::hint::black_box(&self.private);
    }
}

impl TransferBypassKeypair {
    /// Generate a fresh keypair from OS randomness.
    pub fn generate() -> Result<Self, TransferBypassError> {
        let mut private = [0u8; 32];
        os_random(&mut private)?;
        Ok(Self { private })
    }

    /// Rebuild a keypair from stored private bytes (clamped on use, per X25519).
    pub fn from_private_bytes(private: [u8; 32]) -> Self {
        Self { private }
    }

    /// The raw private scalar. Callers must treat this as secret key material.
    pub fn private_bytes(&self) -> &[u8; 32] {
        &self.private
    }

    /// The raw 32-byte public key (`X25519(private, base point)`).
    pub fn public_bytes(&self) -> [u8; 32] {
        X25519PrivateKey::from_bytes(self.private).public_key()
    }

    /// The `KITTY_PUBLIC_KEY` environment value to advertise to PTY children.
    pub fn kitty_public_key_env(&self) -> String {
        format!(
            "{KITTY_PUBLIC_KEY_VERSION}:{}",
            b85encode(&self.public_bytes())
        )
    }
}

/// Parse a `KITTY_PUBLIC_KEY` environment value back into a raw public key.
/// Returns `None` for unknown versions, bad base85, or wrong lengths.
pub fn parse_kitty_public_key_env(value: &str) -> Option<[u8; 32]> {
    let (version, encoded) = value.split_once(':')?;
    if version != KITTY_PUBLIC_KEY_VERSION {
        return None;
    }
    <[u8; 32]>::try_from(b85decode(encoded)?).ok()
}

/// Encrypt a `kitty-1:` bypass credential for `terminal_public_key`,
/// mirroring Kitty's client-side `Encrypt_data`: the plaintext is
/// `<now_ns>:<request_id>;<password>`, encrypted under an ephemeral X25519
/// key with AES-256-GCM (12-byte nonce, 16-byte tag, no AAD).
pub fn encrypt_bypass_credential(
    request_id: &str,
    password: &str,
    terminal_public_key: &[u8; 32],
    now_ns: u64,
) -> Result<String, TransferBypassError> {
    let mut ephemeral = [0u8; 32];
    os_random(&mut ephemeral)?;
    let ephemeral_key = X25519PrivateKey::from_bytes(ephemeral);
    // Wipe the local copy; `ephemeral_key` wipes itself on drop.
    for byte in ephemeral.iter_mut() {
        *byte = 0;
    }
    let shared_secret = ephemeral_key
        .diffie_hellman(terminal_public_key)
        .map_err(|_| TransferBypassError::DegeneratePublicKey)?;
    let aes_key = Sha256::digest(shared_secret);
    let mut shared_wipe = shared_secret;
    for byte in shared_wipe.iter_mut() {
        *byte = 0;
    }
    let _ = std::hint::black_box(&shared_wipe);
    let aes_key = <[u8; 32]>::try_from(aes_key.as_slice())
        .map_err(|_| TransferBypassError::Encode("sha256 output is not 32 bytes".into()))?;
    let plaintext = format!("{now_ns}:{request_id};{password}");
    let mut ciphertext = plaintext.into_bytes();
    let mut nonce = [0u8; 12];
    os_random(&mut nonce)?;
    let gcm = Aes256Gcm::new(Aes256::new(&aes_key));
    let tag = gcm.encrypt(&nonce, b"", &mut ciphertext);
    let envelope = BypassEnvelope {
        pubkey: b85encode(&ephemeral_key.public_key()),
        iv: b85encode(&nonce),
        tag: b85encode(&tag),
        encrypted: b85encode(&ciphertext),
    };
    let json = serde_json::to_string(&envelope)
        .map_err(|err| TransferBypassError::Encode(err.to_string()))?;
    Ok(format!("kitty-1:{json}"))
}

/// Hash a wire bypass credential for audit rows. The hex digest authorizes
/// nothing and reveals nothing on its own; the raw credential (and the
/// configured password) must never be logged.
pub fn hash_bypass_token(wire_bypass: &str) -> String {
    format!("{:x}", Sha256::digest(wire_bypass.as_bytes()))
}

/// Current time as nanoseconds since the Unix epoch, for the ±5-minute
/// envelope window. `None` before the epoch (callers fail closed).
pub fn current_time_ns() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_nanos().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "test-session";
    const PASSWORD: &str = "s3cret";

    // CPython `base64.b85encode` outputs, generated against the interpreter
    // (no upstream vectors exist in kitty-src: `kitty_tests/` has no bypass
    // coverage; `rg bypass kitty_tests/` matches only unrelated identifiers).
    const B85_VECTORS: &[(&[u8], &str)] = &[
        (b"hello", "Xk~0{Zv"),
        (&[0], "00"),
        (&[0, 1], "009"),
        (&[0, 1, 2], "009C"),
        (&[0, 1, 2, 3], "009C6"),
        (&[0, 1, 2, 3, 4], "009C61O"),
        (
            &[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ],
            "009C61O)~M2nh-c3=Iws5D^j+6crX17#SKH9337X",
        ),
        (&[0u8; 32], "0000000000000000000000000000000000000000"),
        (
            b"\x00\xff\x01\x02test-payload-1234",
            "0RI63bY*jNEpTCZY;R#?Eip1PGy",
        ),
    ];

    #[test]
    fn base85_matches_cpython_vectors() {
        for (raw, encoded) in B85_VECTORS {
            assert_eq!(b85encode(raw), *encoded, "encode {raw:02x?}");
            assert_eq!(
                b85decode(encoded).as_deref(),
                Some(*raw),
                "decode {encoded}"
            );
        }
    }

    #[test]
    fn base85_round_trip_covers_every_remainder() {
        for len in 0..70usize {
            let raw: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let encoded = b85encode(&raw);
            let expected_len = len / 4 * 5
                + match len % 4 {
                    0 => 0,
                    remainder => remainder + 1,
                };
            assert_eq!(encoded.len(), expected_len, "length {len}");
            assert_eq!(b85decode(&encoded).as_deref(), Some(raw.as_slice()));
        }
    }

    #[test]
    fn base85_rejects_garbage_fail_closed() {
        assert_eq!(b85decode(""), Some(Vec::new()));
        assert!(b85decode("has space").is_none());
        assert!(b85decode("ünïcodé").is_none());
        // 85^5 - 1 overflows u32; CPython raises "base85 overflow".
        assert!(b85decode("~~~~~").is_none());
    }

    // CPython `hashlib.sha256(b"test-session;s3cret").hexdigest()`.
    const SHA256_VECTOR: &str =
        "sha256:acefc4a6a9658b6bd45a556c2a43c0dc2396f337ab73d30f8fa2b13e0d9e0d69";

    #[test]
    fn sha256_arm_matches_kitty_encoding() {
        assert_eq!(encode_bypass_credential(SESSION, PASSWORD), SHA256_VECTOR);
    }

    fn verify_sha256(request_id: &str, wire: &str, password: &str) -> bool {
        verify_bypass_credential(request_id, wire, password, None, 0)
    }

    #[test]
    fn sha256_arm_accepts_the_configured_password() {
        assert!(verify_sha256(SESSION, SHA256_VECTOR, PASSWORD));
    }

    #[test]
    fn sha256_arm_rejects_wrong_password_request_id_and_tampering() {
        assert!(!verify_sha256(SESSION, SHA256_VECTOR, "wrong"));
        assert!(!verify_sha256("other-session", SHA256_VECTOR, PASSWORD));
        let mut tampered = String::from(SHA256_VECTOR);
        tampered.replace_range(10..11, "0");
        assert!(!verify_sha256(SESSION, &tampered, PASSWORD));
        // Truncated digest and unknown protocols fail closed.
        assert!(!verify_sha256(SESSION, "sha256:abc", PASSWORD));
        assert!(!verify_sha256(SESSION, "md5:anything", PASSWORD));
        assert!(!verify_sha256(SESSION, "", PASSWORD));
        assert!(!verify_sha256(SESSION, "no-separator", PASSWORD));
    }

    #[test]
    fn empty_configured_password_never_verifies() {
        let encoded = encode_bypass_credential(SESSION, "");
        assert!(!verify_sha256(SESSION, &encoded, ""));
        assert!(!verify_sha256(SESSION, SHA256_VECTOR, ""));
    }

    #[test]
    fn overlong_wire_credentials_fail_closed_before_parsing() {
        let long = format!("sha256:{}", "a".repeat(MAX_BYPASS_WIRE_BYTES));
        assert!(long.len() > MAX_BYPASS_WIRE_BYTES);
        assert!(!verify_sha256(SESSION, &long, PASSWORD));
    }

    fn fixed_keypair() -> TransferBypassKeypair {
        TransferBypassKeypair::from_private_bytes([7u8; 32])
    }

    const NOW_NS: u64 = 1_750_000_000_000_000_000;

    fn kitty1_round_trip(password: &str) -> (TransferBypassKeypair, String) {
        let terminal = fixed_keypair();
        let public = terminal.public_bytes();
        let wire = encrypt_bypass_credential(SESSION, password, &public, NOW_NS)
            .expect("encryption with OS randomness");
        assert!(wire.len() <= MAX_BYPASS_WIRE_BYTES);
        (terminal, wire)
    }

    fn verify_kitty1(
        request_id: &str,
        wire: &str,
        password: &str,
        key: &TransferBypassKeypair,
        now_ns: u64,
    ) -> bool {
        verify_bypass_credential(
            request_id,
            wire,
            password,
            Some(key.private_bytes()),
            now_ns,
        )
    }

    #[test]
    fn kitty1_arm_round_trip_verifies() {
        let (terminal, wire) = kitty1_round_trip(PASSWORD);
        assert!(verify_kitty1(SESSION, &wire, PASSWORD, &terminal, NOW_NS));
    }

    #[test]
    fn kitty1_arm_rejects_wrong_password_and_request_id() {
        let (terminal, wire) = kitty1_round_trip(PASSWORD);
        assert!(!verify_kitty1(SESSION, &wire, "wrong", &terminal, NOW_NS));
        assert!(!verify_kitty1(
            "other-session",
            &wire,
            PASSWORD,
            &terminal,
            NOW_NS
        ));
    }

    #[test]
    fn kitty1_arm_rejects_tampered_envelopes() {
        let (terminal, wire) = kitty1_round_trip(PASSWORD);
        let mut envelope: BypassEnvelope =
            serde_json::from_str(wire.strip_prefix("kitty-1:").unwrap()).unwrap();
        // Flip one ciphertext byte.
        let mut ciphertext = b85decode(&envelope.encrypted).unwrap();
        ciphertext[0] ^= 1;
        envelope.encrypted = b85encode(&ciphertext);
        let tampered = format!("kitty-1:{}", serde_json::to_string(&envelope).unwrap());
        assert!(!verify_kitty1(
            SESSION, &tampered, PASSWORD, &terminal, NOW_NS
        ));
        // Flip one tag byte.
        let mut envelope: BypassEnvelope =
            serde_json::from_str(wire.strip_prefix("kitty-1:").unwrap()).unwrap();
        let mut tag = b85decode(&envelope.tag).unwrap();
        tag[0] ^= 1;
        envelope.tag = b85encode(&tag);
        let tampered = format!("kitty-1:{}", serde_json::to_string(&envelope).unwrap());
        assert!(!verify_kitty1(
            SESSION, &tampered, PASSWORD, &terminal, NOW_NS
        ));
        // Truncated tag (not 16 bytes) and truncated IV fail closed.
        let mut envelope: BypassEnvelope =
            serde_json::from_str(wire.strip_prefix("kitty-1:").unwrap()).unwrap();
        envelope.tag = b85encode(&[0u8; 15]);
        let tampered = format!("kitty-1:{}", serde_json::to_string(&envelope).unwrap());
        assert!(!verify_kitty1(
            SESSION, &tampered, PASSWORD, &terminal, NOW_NS
        ));
    }

    #[test]
    fn kitty1_arm_enforces_the_five_minute_window() {
        let (terminal, wire) = kitty1_round_trip(PASSWORD);
        let window = u64::try_from(BYPASS_TIMESTAMP_WINDOW_NS).unwrap();
        assert!(verify_kitty1(
            SESSION,
            &wire,
            PASSWORD,
            &terminal,
            NOW_NS + window
        ));
        assert!(verify_kitty1(
            SESSION,
            &wire,
            PASSWORD,
            &terminal,
            NOW_NS - window
        ));
        assert!(!verify_kitty1(
            SESSION,
            &wire,
            PASSWORD,
            &terminal,
            NOW_NS + window + 1
        ));
        assert!(!verify_kitty1(
            SESSION,
            &wire,
            PASSWORD,
            &terminal,
            NOW_NS - window - 1
        ));
    }

    #[test]
    fn kitty1_arm_needs_the_terminal_private_key() {
        let (terminal, wire) = kitty1_round_trip(PASSWORD);
        // No key configured: fail closed (the sha256 arm is unaffected).
        assert!(!verify_bypass_credential(
            SESSION, &wire, PASSWORD, None, NOW_NS
        ));
        assert!(verify_sha256(SESSION, SHA256_VECTOR, PASSWORD));
        // Wrong key: fail closed.
        let other = TransferBypassKeypair::from_private_bytes([9u8; 32]);
        assert!(!verify_kitty1(SESSION, &wire, PASSWORD, &other, NOW_NS));
        let _ = terminal;
    }

    #[test]
    fn kitty1_arm_rejects_malformed_envelopes() {
        let terminal = fixed_keypair();
        for wire in [
            "kitty-1:not-json".to_string(),
            "kitty-1:{\"pubkey\":\"\",\"iv\":\"\",\"tag\":\"\",\"encrypted\":\"\"}".to_string(),
            "kitty-1:{\"iv\":\"AA\",\"tag\":\"AA\",\"encrypted\":\"AA\"}".to_string(),
            "kitty-1:{\"pubkey\":\"!!!\",\"iv\":\"AA\",\"tag\":\"AA\",\"encrypted\":\"AA\"}"
                .to_string(),
            "kitty-1:{\"pubkey\":\"AA\",\"iv\":\"AA\",\"tag\":\"AA\",\"encrypted\":\"AA\"}"
                .to_string(),
        ] {
            assert!(
                !verify_kitty1(SESSION, &wire, PASSWORD, &terminal, NOW_NS),
                "{wire}"
            );
        }
        // Empty password fails even the kitty-1 arm (stricter than Kitty).
        let public = terminal.public_bytes();
        let wire = encrypt_bypass_credential(SESSION, "", &public, NOW_NS).unwrap();
        assert!(!verify_bypass_credential(
            SESSION,
            &wire,
            "",
            Some(terminal.private_bytes()),
            NOW_NS
        ));
    }

    /// Encrypt-then-wrap helper for crafting hostile plaintexts with a valid
    /// tag, proving theplaintext checks (UTF-8, colon, timestamp) fail closed.
    fn envelope_with_plaintext(plaintext: &[u8], key: &TransferBypassKeypair) -> String {
        use purecrypto::cipher::{Aes256, Aes256Gcm};
        let mut ephemeral = [0x42u8; 32];
        // Deterministic stand-in randomness: test-only, never production.
        ephemeral[0] ^= 0x01;
        let ephemeral_key = X25519PrivateKey::from_bytes(ephemeral);
        let shared = ephemeral_key.diffie_hellman(&key.public_bytes()).unwrap();
        let aes_key = Sha256::digest(shared);
        let aes_key = <[u8; 32]>::try_from(aes_key.as_slice()).unwrap();
        let mut ciphertext = plaintext.to_vec();
        let nonce = [0x11u8; 12];
        let gcm = Aes256Gcm::new(Aes256::new(&aes_key));
        let tag = gcm.encrypt(&nonce, b"", &mut ciphertext);
        let envelope = BypassEnvelope {
            pubkey: b85encode(&ephemeral_key.public_key()),
            iv: b85encode(&nonce),
            tag: b85encode(&tag),
            encrypted: b85encode(&ciphertext),
        };
        format!("kitty-1:{}", serde_json::to_string(&envelope).unwrap())
    }

    #[test]
    fn kitty1_arm_rejects_hostile_plaintexts_with_valid_tags() {
        let terminal = fixed_keypair();
        // Non-UTF8, missing colon, and non-numeric timestamp payloads.
        for plaintext in [
            vec![0xff, 0xfe, 0x00, 0x01],
            b"no-colon-here".to_vec(),
            b"notanum:test-session;s3cret".to_vec(),
            format!("{NOW_NS}:test-session").into_bytes(),
            format!("{}:test-session;", u64::MAX).into_bytes(),
        ] {
            let wire = envelope_with_plaintext(&plaintext, &terminal);
            assert!(
                !verify_kitty1(SESSION, &wire, PASSWORD, &terminal, NOW_NS),
                "{}",
                String::from_utf8_lossy(&plaintext)
            );
        }
        // Correctly shaped plaintext still verifies through this path.
        let wire = envelope_with_plaintext(
            format!("{NOW_NS}:{SESSION};{PASSWORD}").as_bytes(),
            &terminal,
        );
        assert!(verify_kitty1(SESSION, &wire, PASSWORD, &terminal, NOW_NS));
    }

    #[test]
    fn keypair_env_format_round_trips() {
        let keypair = fixed_keypair();
        let env = keypair.kitty_public_key_env();
        assert!(env.starts_with("1:"), "{env}");
        assert_eq!(
            parse_kitty_public_key_env(&env),
            Some(keypair.public_bytes())
        );
        assert_eq!(parse_kitty_public_key_env("2:AA"), None);
        assert_eq!(parse_kitty_public_key_env("no-separator"), None);
        assert_eq!(parse_kitty_public_key_env("1:!!!"), None);
        assert_eq!(parse_kitty_public_key_env("1:AA"), None);
    }

    #[test]
    fn generated_keypairs_are_fresh_and_usable() {
        let first = TransferBypassKeypair::generate().unwrap();
        let second = TransferBypassKeypair::generate().unwrap();
        assert_ne!(first.public_bytes(), second.public_bytes());
        let public = first.public_bytes();
        let wire = encrypt_bypass_credential(SESSION, PASSWORD, &public, NOW_NS).unwrap();
        assert!(verify_bypass_credential(
            SESSION,
            &wire,
            PASSWORD,
            Some(first.private_bytes()),
            NOW_NS
        ));
    }

    #[test]
    fn token_hash_is_stable_opaque_and_hex() {
        let first = hash_bypass_token(SHA256_VECTOR);
        assert_eq!(first, hash_bypass_token(SHA256_VECTOR));
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!first.contains(PASSWORD));
        assert_ne!(first, hash_bypass_token("sha256:other"));
    }
}
