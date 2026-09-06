//! Daemon-side transfer consent policy: operator config plus the daemon keypair.
//!
//! The consent *mechanism* (password verification, per-direction allow/ask/deny,
//! [`evaluate_transfer_bypass_with_policy`](cterm_app::evaluate_transfer_bypass_with_policy))
//! lives in `cterm-app`; this module is the daemon integration that was missing
//! (see `docs/transfer-bypass-policy.md`):
//!
//! - [`DaemonTransferContext`] carries the loaded [`TransferConfig`](cterm_app::TransferConfig)
//!   and one process-lifetime [`TransferBypassKeypair`](cterm_app::TransferBypassKeypair)
//!   into the transfer worker path, so live transfers consult the seam instead
//!   of always prompting.
//! - The keypair mirrors Kitty exactly: one per daemon process, generated at
//!   startup like Kitty's per-`Boss` `EllipticCurveKey` (`kitty/boss.py:425`),
//!   advertised to every local PTY child as `KITTY_PUBLIC_KEY`
//!   (`kitty/child.py:385`). The private half is held in daemon memory only —
//!   never written to disk, never sent anywhere — and rotates on every daemon
//!   (re)start. A child holding a stale value after a restart simply fails
//!   closed back to the consent prompt.
//! - Defaults are unchanged: with stock config (no password, no explicit
//!   direction modes) every evaluation resolves to `Prompt`, byte-identical to
//!   the legacy always-prompt path. Flipping any default remains Julian's
//!   policy decision; this module decides none of it.

use std::sync::Arc;

use cterm_app::{
    current_time_ns, evaluate_transfer_bypass_with_policy, BypassDecision, TransferBypassKeypair,
    TransferBypassPolicy, TransferConfig, TtyTransferBypassKind, KITTY_PUBLIC_KEY_ENV,
};
use cterm_core::{FileTransferAction, FileTransferCommand};

/// Daemon-wide transfer consent context: operator config plus the daemon keypair.
///
/// One instance is shared by every session (held by [`SessionManager`](super::SessionManager)
/// and cloned into each [`SessionState`](super::SessionState)); the transfer
/// worker snapshots the evaluation inputs per session-start command. `Clone`
/// is cheap (config plus 32 secret bytes); the type deliberately does not
/// implement `Debug` so the private half can never reach a log line.
#[derive(Clone)]
pub struct DaemonTransferContext {
    config: TransferConfig,
    keypair: Option<TransferBypassKeypair>,
}

impl DaemonTransferContext {
    /// Build a context from operator config, generating a fresh process-lifetime
    /// keypair. OS-randomness failure degrades to a keyless context: the
    /// `kitty-1` envelope arm fails closed while the `sha256` arm keeps working.
    pub fn new(config: TransferConfig) -> Self {
        Self {
            config,
            keypair: TransferBypassKeypair::generate().ok(),
        }
    }

    /// Stock behavior: default (always-prompt) config with a fresh keypair.
    /// Key advertisement alone never authorizes anything without an explicit
    /// opt-in config.
    pub fn with_defaults() -> Self {
        Self::new(TransferConfig::default())
    }

    /// Deterministic constructor for tests: explicit config plus keypair.
    #[cfg(test)]
    pub fn with_keypair(config: TransferConfig, keypair: TransferBypassKeypair) -> Self {
        Self {
            config,
            keypair: Some(keypair),
        }
    }

    /// Keyless constructor for tests: the `kitty-1` arm must fail closed.
    #[cfg(test)]
    pub fn keyless(config: TransferConfig) -> Self {
        Self {
            config,
            keypair: None,
        }
    }

    /// The operator transfer config this context evaluates under.
    pub fn transfer_config(&self) -> &TransferConfig {
        &self.config
    }

    /// Whether this context holds a private key for the `kitty-1` arm.
    pub fn has_keypair(&self) -> bool {
        self.keypair.is_some()
    }

    /// The `KITTY_PUBLIC_KEY` environment value to advertise to PTY children,
    /// or `None` when keyless. The value carries only the public half.
    pub fn kitty_public_key_env_value(&self) -> Option<String> {
        self.keypair.as_ref().map(|key| key.kitty_public_key_env())
    }

    /// Advertise the daemon public key to one child's environment, exactly as
    /// Kitty does (`kitty/child.py:385` sets the variable unconditionally for
    /// every child). Any client-supplied value is overwritten: a PTY child
    /// must not be able to spoof the terminal's identity to its own kittens.
    /// Keyless contexts leave the environment untouched (no key to advertise).
    pub fn inject_child_env(&self, env: &mut Vec<(String, String)>) {
        let Some(public) = self.kitty_public_key_env_value() else {
            return;
        };
        if let Some(entry) = env.iter_mut().find(|(key, _)| key == KITTY_PUBLIC_KEY_ENV) {
            entry.1 = public;
        } else {
            env.push((KITTY_PUBLIC_KEY_ENV.to_string(), public));
        }
    }

    /// Evaluate one session-start command against the consent seam.
    ///
    /// This is the daemon counterpart of Kitty's bypass check in
    /// `ActiveSend`/`ActiveReceive.__init__` (`kitty/file_transmission.py`):
    /// a non-empty wire `pw` credential is evaluated as
    /// [`TtyTransferBypassKind::PasswordCredential`]; anything else (no
    /// credential, empty credential) evaluates as
    /// [`TtyTransferBypassKind::None`], which ignores wire bytes exactly like
    /// the legacy seam. Non-start commands always resolve to `Prompt` (the
    /// evaluation seam itself enforces start-only bypass; the worker additionally
    /// only calls this for starts).
    pub fn evaluate_session_start(&self, command: &FileTransferCommand) -> BypassDecision {
        if !matches!(
            command.action,
            FileTransferAction::Send | FileTransferAction::Receive
        ) {
            return BypassDecision::Prompt;
        }
        let kind = if command
            .bypass
            .as_deref()
            .is_some_and(|credential| !credential.is_empty())
        {
            TtyTransferBypassKind::PasswordCredential
        } else {
            TtyTransferBypassKind::None
        };
        // `current_time_ns` only fails before the Unix epoch; falling back to
        // zero keeps the `sha256` arm (which ignores time) usable while the
        // `kitty-1` freshness window fails closed for any real timestamp.
        let policy = TransferBypassPolicy {
            config: &self.config,
            decryption_key: self.keypair.as_ref().map(|key| key.private_bytes()),
            now_ns: current_time_ns().unwrap_or(0),
        };
        evaluate_transfer_bypass_with_policy(command, kind, &policy)
    }
}

impl Default for DaemonTransferContext {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Shareable handle type used by the session layer.
pub type SharedTransferContext = Arc<DaemonTransferContext>;

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_app::{
        encode_bypass_credential, encrypt_bypass_credential, parse_kitty_public_key_env,
        TransferConsentMode,
    };

    const PASSWORD: &str = "s3cret";

    fn keypair() -> TransferBypassKeypair {
        TransferBypassKeypair::from_private_bytes([7u8; 32])
    }

    fn start(action: FileTransferAction, id: &str) -> FileTransferCommand {
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

    fn credentialed(id: &str, bypass: &str) -> FileTransferCommand {
        let mut command = start(FileTransferAction::Send, id);
        command.bypass = Some(bypass.into());
        command
    }

    /// Out of the box the daemon seam keeps today's behavior for every
    /// direction, kind selection, and wire credential: always prompt.
    #[test]
    fn default_context_prompts_for_everything() {
        let context = DaemonTransferContext::with_keypair(TransferConfig::default(), keypair());
        let attacker = encode_bypass_credential("any-session", "attacker-guess");
        for action in [FileTransferAction::Send, FileTransferAction::Receive] {
            for mut command in [
                start(action, "plain"),
                credentialed("any-session", &attacker),
            ] {
                command.action = action;
                assert_eq!(
                    context.evaluate_session_start(&command),
                    BypassDecision::Prompt,
                    "default context must prompt for {action:?}"
                );
            }
        }
        // Non-start commands always prompt, even credentialed under opt-in config.
        let allow = TransferConfig {
            bypass_password: Some(PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let allowed = DaemonTransferContext::with_keypair(allow, keypair());
        let mut data = start(FileTransferAction::Data, "allow-x");
        data.bypass = Some(encode_bypass_credential("allow-x", PASSWORD));
        assert_eq!(
            allowed.evaluate_session_start(&data),
            BypassDecision::Prompt
        );
    }

    #[test]
    fn explicit_allow_with_verified_credential_auto_approves() {
        let key = keypair();
        let config = TransferConfig {
            bypass_password: Some(PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let context = DaemonTransferContext::with_keypair(config, key);
        // sha256 arm.
        let sha = encode_bypass_credential("allow-sha", PASSWORD);
        assert_eq!(
            context.evaluate_session_start(&credentialed("allow-sha", &sha)),
            BypassDecision::Allow
        );
        // kitty-1 arm, encrypted to the advertised public key.
        let public = keypair().public_bytes();
        let now = current_time_ns().expect("system clock is before the Unix epoch");
        let envelope = encrypt_bypass_credential("allow-1", PASSWORD, &public, now).unwrap();
        assert_eq!(
            context.evaluate_session_start(&credentialed("allow-1", &envelope)),
            BypassDecision::Allow
        );
    }

    /// A wrong password keeps the consent prompt, even with `allow` configured.
    #[test]
    fn wrong_password_still_prompts_under_allow() {
        let config = TransferConfig {
            bypass_password: Some(PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let context = DaemonTransferContext::with_keypair(config, keypair());
        let wrong = encode_bypass_credential("allow-x", "wrong-guess");
        assert_eq!(
            context.evaluate_session_start(&credentialed("allow-x", &wrong)),
            BypassDecision::Prompt
        );
        // Missing credential under allow also prompts.
        assert_eq!(
            context.evaluate_session_start(&start(FileTransferAction::Send, "allow-x")),
            BypassDecision::Prompt
        );
    }

    /// An explicit direction deny hard-refuses without prompting, for every
    /// wire credential including valid ones.
    #[test]
    fn explicit_deny_hard_refuses_without_prompt() {
        let config = TransferConfig {
            bypass_password: Some(PASSWORD.into()),
            send: Some(TransferConsentMode::Deny),
            receive: None,
        };
        let context = DaemonTransferContext::with_keypair(config, keypair());
        let valid = encode_bypass_credential("denied-send", PASSWORD);
        assert_eq!(
            context.evaluate_session_start(&credentialed("denied-send", &valid)),
            BypassDecision::Deny
        );
        assert_eq!(
            context.evaluate_session_start(&start(FileTransferAction::Send, "denied-send")),
            BypassDecision::Deny
        );
        // The unset direction still prompts.
        let mut receive = credentialed("r", &valid);
        receive.action = FileTransferAction::Receive;
        assert_eq!(
            context.evaluate_session_start(&receive),
            BypassDecision::Prompt
        );
    }

    /// Generated contexts hold fresh keypairs whose advertisement round-trips
    /// to the public half; child-supplied values are overwritten, never merged.
    #[test]
    fn keypair_generation_and_advertisement() {
        let first = DaemonTransferContext::with_defaults();
        let second = DaemonTransferContext::with_defaults();
        assert!(first.has_keypair());
        let first_public = first.kitty_public_key_env_value().unwrap();
        let second_public = second.kitty_public_key_env_value().unwrap();
        assert!(first_public.starts_with("1:"));
        assert_ne!(first_public, second_public);

        // The advertised value parses back to the keypair's public half.
        let deterministic =
            DaemonTransferContext::with_keypair(TransferConfig::default(), keypair());
        let advertised = deterministic.kitty_public_key_env_value().unwrap();
        assert_eq!(
            parse_kitty_public_key_env(&advertised),
            Some(keypair().public_bytes())
        );

        // Injection appends once, then overwrites (a child must not spoof it).
        let mut env = vec![("TERM".to_string(), "xterm-256color".to_string())];
        deterministic.inject_child_env(&mut env);
        assert_eq!(env.len(), 2);
        deterministic.inject_child_env(&mut env);
        assert_eq!(env.len(), 2);
        let mut spoofed = vec![(KITTY_PUBLIC_KEY_ENV.to_string(), "1:spoofed".to_string())];
        deterministic.inject_child_env(&mut spoofed);
        assert_eq!(
            spoofed,
            vec![(KITTY_PUBLIC_KEY_ENV.to_string(), advertised)]
        );

        // The private half never appears in the environment: neither the
        // full 32-byte hex nor the raw debug rendering may leak.
        let private_hex: String = keypair()
            .private_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(private_hex.len(), 64);
        for (_, value) in &env {
            assert!(!value.contains(&private_hex));
        }
    }

    #[test]
    fn keyless_context_advertises_nothing_and_fails_kitty1_closed() {
        let config = TransferConfig {
            bypass_password: Some(PASSWORD.into()),
            send: Some(TransferConsentMode::Allow),
            receive: None,
        };
        let context = DaemonTransferContext::keyless(config);
        assert!(!context.has_keypair());
        assert!(context.kitty_public_key_env_value().is_none());
        let mut env = vec![("TERM".to_string(), "xterm-256color".to_string())];
        context.inject_child_env(&mut env);
        assert_eq!(env.len(), 1);
        // The sha256 arm needs no key, so allow still works there.
        let sha = encode_bypass_credential("allow-sha", PASSWORD);
        assert_eq!(
            context.evaluate_session_start(&credentialed("allow-sha", &sha)),
            BypassDecision::Allow
        );
    }
}
