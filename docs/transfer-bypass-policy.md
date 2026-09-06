# Transfer consent-bypass policy (PROPOSAL — not implemented)

Status: **draft for Julian's decision. No bypass exists in the code.**
`TtyTransferBypassKind` (`crates/cterm-app/src/kitty_file_transfer.rs`) and
`evaluate_transfer_bypass` resolve every request to `BypassDecision::Prompt`,
and the regression tests `bypass_evaluation_is_default_deny_for_every_kind`
and `wire_bypass_credential_never_skips_the_consent_prompt` lock that in. Any
future bypass must extend that seam deliberately; nothing in the codec, the
authorization state machine, or the daemon worker may be relaxed to "make a
bypass work".

## Threat model

Kitty's OSC 5113 file transfer lets any process that can write to the PTY
(master side) ask the terminal to move files:

- **Send direction**: after one approval, the peer supplies file names,
  contents, and POSIX metadata. Approval therefore grants the peer the
  ability to create or atomically replace **any file the ctermd account can
  write**, including dotfiles, keys, and scheduled-task definitions. File
  names arrive *after* approval, so the prompt cannot show what will land.
- **Receive direction**: the peer names local paths it wants read back over
  the TTY. Approval discloses file contents to whoever controls the
  PTY-writing process (possibly a remote SSH peer).
- The `pw` (bypass) credential on the wire is **attacker-controlled input**:
  decoding it must never authorize anything. Today it is parsed, bounded
  (512 bytes), and ignored.

Attacker capabilities assumed: full control of PTY output bytes (malicious or
compromised remote program, escape-sequence injection via `cat` of untrusted
text, replayed byte streams). Attacker goals: silent file write/read without
a human noticing, prompt fatigue leading to reflexive approval, and
session-id/token confusion across reconnects.

Out of scope for this policy: sandboxing the daemon account itself, SSH-level
authentication, and endpoint compromise (if ctermd's account is owned, no
prompt helps).

## Proposed bypass conditions (all require Julian's go)

1. **Same-TTY pre-authorized sessions.** The local user runs an explicit
   opt-in command (e.g. `cterm transfer --allow-once --session <id>`) in the
   same terminal session, which mints a single-use, short-lived
   pre-authorization bound to the exact `(daemon session, transfer id)` pair.
   Only the *next* matching request skips the dialog; everything else still
   prompts. Pre-authorizations die with the session and never survive daemon
   relaunch.
2. **Loopback automation.** Transfers where the PTY peer provably is the
   local machine itself (cterm's own scripts/tests driving a local shell),
   gated behind an explicit per-session flag set from the UI process — never
   inferred from wire bytes. Intended for scripted tests and local tooling,
   not for general use.

Both are **narrower than Kitty's own `pw` bypass**, which trusts any peer
presenting a credential derived from the user-configured
`file_transfer_confirmation_bypass` password (verified against Kitty 0.48.2's
`check_bypass`: either `sha256(request_id + ";" + password)` compared with
`hmac.compare_digest`, or a `kitty-1:` JSON envelope carrying
`pubkey`/`iv`/`tag`/`encrypted` AES-256-GCM fields whose decrypted payload
must equal `request_id + ";" + password` with a timestamp inside ±5 minutes).
A password-derived credential adds no security over prompting once any
PTY-writing process can present it, while removing the human — so cterm
deliberately has no equivalent yet (the wire field is parsed, bounded, and
ignored).

## What stays always-prompt

- Every **send** session that was not covered by a live, exact-match
  pre-authorization from condition 1 or 2 above.
- Every **receive** session naming paths outside an explicitly approved set
  (receive paths are known before approval, so a future policy may allow
  scoping; until then: prompt).
- Any request arriving while another session's prompt is pending, after a
  peer `cancel`, after expiry, or after session-id reuse — token confusion
  must always fail closed, bypass or not.
- Daemon relaunch with live transfer state stays refused (existing
  fail-closed behavior, covered by
  `test_tty_file_transfer_consent_replay_and_relaunch_refusal`).
- Managed mode disables the complete transfer surface, bypass included.

## Audit-logging requirements

Whether or not a bypass ever lands, every consent decision must already be
logged; a bypass adds rows, never removes them:

- Daemon: `request_id`, transfer id, direction, decision
  (approved/denied/expired/cancelled), and — once it exists — the bypass kind
  evaluated plus which pre-authorization (by token hash, never the token)
  authorized it. `apply_dialog_outcome` logs approve/deny/expire/cancel today.
- Frontend: dialog shown/dismissed, expiry observed, lifecycle cancellation.
- Bypass use must be **counted and reviewable** (e.g. surfaced in session
  metadata) so silent auto-approval cannot become the unnoticed default.
- Deny-by-default violations (a bypass authorizing outside its exact scope)
  must be a hard error with staging discarded, mirroring `reject_session`.

## Open questions for Julian

1. Is condition 1 (same-TTY `--allow-once`) acceptable at all, or should
   every transfer always show a dialog, no exceptions?
2. Is condition 2 (loopback automation flag) needed for scripted
   test/tooling flows, and if so, who may set the flag (UI process only?
   config file?).
3. Should pre-authorizations be visible/auditable in the UI (e.g. a session
   badge), and should loopback use require a config opt-in defaulting off?
4. Retention: how long may pre-authorizations live (single next request only
   vs. time-boxed), and must they be listed/revocable?
5. Does the shared-secret (`pw`) style bypass need any interop story for
   Kitty-native tooling, or is "always prompt" the permanent answer?
