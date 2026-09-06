# Desktop notifications (OSC 9 / OSC 777 / Kitty OSC 99)

Single shared model in `cterm-core::notifications`, parsed by the daemon and
mirrored by every native frontend. The frontend mirror (`process_mirror` →
`TerminalEvent::DesktopNotification`) is authoritative for delivery; the
daemon `DesktopNotificationEvent` gRPC stream remains the original
title/body/focus/close common subset.

## Advertised capability set (single source of truth)

`cterm_core::notifications::capability_reply` answers `p=?` with exactly what
is wired — never more:

```
p=title,body,icon,buttons,?,alive,close
a=focus,report
o=always,unfocused,invisible
u=0,1,2
s=silent,system
w=1
c=1
```

## Per-platform delivery matrix

| Feature | GTK (GApplication) | Cocoa (UserNotifications) | Win32 (balloons) |
|---|---|---|---|
| Title / body / urgency | Yes | Yes (interruption levels) | Yes (info/error) |
| Replace / close by `i=` | Yes | Yes | Yes |
| Activation report (`a=report`) | Yes (targeted default action) | Yes, seam-reuse¹ | Yes (click event)¹ |
| Button reports | Yes (one action per button) | Degraded: activation without button number; hover-menu actions open | Missing: balloons have no buttons; toast migration open |
| Close report (`c=1`) | `untracked` disclaimer + real close on expiry | `untracked` disclaimer + real close on activation/expiry¹ | Real close (click/hide/timeout)¹ |
| `p=alive` polling | Yes (daemon optimistic set) | Yes | Yes |
| Icons (`n=`/`p=icon`/`g=`) | Names via theme, bytes via BytesIcon | Degraded: app icon (attachment file-URL hop open)¹ | Degraded: app icon¹ |
| Sounds (`s=`) | Degraded: no GNotification sound control | `silent` suppresses, rest default | `silent` → NOSOUND, rest default |
| Expiry (`w=`) | Terminal-enforced withdraw + close report; `w=0` never schedules a timer (persists until replaced/closed/evicted/app-exit, OS may still retire) | Enforced, pre-existing thread + close report¹ | Enforced, pre-existing thread + close report¹ (the 12 s default timer also covers `w=0` — known gap, see below) |
| Filtering (`o=`) | Focus/visibility checked | `invisible` ≈ `unfocused` (no tab-visibility hook)¹ | Focus/visibility checked¹ |
| Filter identity (`f=`/`t=`) | Parsed + user allow/deny lists (`[notifications]`, default allow-all) | Parsed, user filtering still open | Parsed, user filtering still open |

¹ Seam-reuse: same core builders/registry pattern as GTK, but Cocoa/Win32 do
not compile on Linux CI here — macOS/Windows builds must verify.

## Reporting wire shapes (Kitty)

- Activation: `ESC ] 99 ; i=<id> ; ST` (or `; <1-based button>` for buttons);
  unidentified uses `i=0`. Sent only when `a=report` was requested.
- Close: `ESC ] 99 ; i=<id> : p=close ; ST`, or `; untracked ST` where the OS
  gives no dismiss signal (GTK/Cocoa). Sent only when `c=1` was requested;
  activation with both flags sends activation then close.
- Unknown `p=close` IDs are ignored (no `Close` event); invalid IDs are
  sanitized to unidentified, never echoed back.

## Bounds (DoS hardening)

Title 1024 B, body 4096 B, buttons payload 4096 B (≤8 labels, 256 B each),
icon 1 MiB (cache 32 entries / 8 MiB, oldest-first), app name 256 B, ≤8 types
(128 B), ≤8 icon names (256 B), sound names 64 B, 64 live IDs (oldest-first).
Unpadded base64 chunks are accepted per the Kitty chunking rule.

## `w=0` never-expire semantics (pinned)

Per Kitty (`desktop-notifications.rst`): `w` absent/negative (default `-1`)
defers to the OS notification service; `w=0` requests never-expire (best
effort — the OS may still retire the banner); `w>0` is a terminal-enforced
auto-close deadline in milliseconds. The shared model resolves this once
(`cterm_core::notifications::expiry_policy` → `OsDefault` / `Never` /
`After(ms)`); `w>0` behavior is unchanged.

`Never` means: the terminal schedules **no** auto-close timer. The
notification persists in the frontend registry and the daemon `p=alive`
set until the app replaces it (same `i=` supersedes, stale expiry timers
are generation-guarded), explicitly closes it (`p=close`), it is evicted
by the 64-ID oldest-first cap, or the frontend process exits. A daemon
reconnect does not clear the daemon-side `p=alive` entry (sessions
survive UI restarts), but the frontend registry is process-local: after a
UI restart the banner is gone locally with no replay while the daemon
still lists the ID until the app closes it.

Known gap: Win32 applies its 12 s default timer also to `w=0` (needs a
Windows host to verify a fix); GTK/Cocoa schedule no timer for `w=0`.

## User filtering (`[notifications]`)

Operator allow/deny lists over the parsed `f=` (app name) and `t=`
(notification types) identity, in `config.toml` (see
`docs/configuration.md`). All lists default to empty = allow everything
(today's behavior). Otherwise deny wins over allow, and a non-empty
allow-list requires a positive match (exact, case-sensitive, trimmed;
blank entries ignored). Untagged notifications (no `f=`/`t=`, e.g. legacy
OSC 9/777) never match a deny entry but also never satisfy an allow-list.

```toml
[notifications]
# allow_apps = ["myapp"]  # only these f= names deliver when non-empty
# deny_apps = ["spammy"]  # these f= names never deliver (wins over allow)
# allow_types = ["email"] # a listed t= is required when non-empty
# deny_types = ["marketing"]
```

Enforced in the shared model (`NotificationFilter::allows`) before
delivery and before any registry/report side effects: filtered
notifications are never shown, never tracked for activation, and never
reported (the daemon's optimistic `p=alive` set still answers polls).
Wired on GTK (missing/unreadable config fails open to allow-all);
Cocoa/Win32 still allow-all (no behavior change there). Deliberately
simpler than Kitty's `filter_notification` `field:regexp` boolean grammar
(which also matches title/body): title/body/regex filtering stays future
work.

## Report ordering (pinned)

- Activation with both flags sends the activation report FIRST followed by
  the close report (`activation_close_sequence`); expiry/replace send only
  the close report (never an activation); a close never precedes its
  activation. On GTK an activation-closed notification leaves the registry
  (current generation only) and is withdrawn, so a pending expiry timer
  cannot duplicate the close.
- `p=alive` is daemon-optimistic: frontend expiry/activation never prune
  the parser set — only app `p=close`, replace-eviction, or the 64-ID cap
  do. Unknown-ID closes stay a no-op (no `Close` event).

## Open spec questions

1. `untracked` timing: sent as an immediate disclaimer on GTK/Cocoa — Kitty's
   own timing (immediate vs on-drop) is ambiguous from the spec text alone.
2. `p=alive` on untracked stacks stays daemon-optimistic (daemon cannot see
   OS dismissals); whether Kitty prunes it on `untracked` is unverified.
