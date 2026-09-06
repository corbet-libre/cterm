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
| Expiry (`w=`) | Terminal-enforced withdraw + close report | Enforced, pre-existing thread + close report¹ | Enforced, pre-existing thread + close report¹ |
| Filtering (`o=`) | Focus/visibility checked | `invisible` ≈ `unfocused` (no tab-visibility hook)¹ | Focus/visibility checked¹ |
| Filter identity (`f=`/`t=`) | Parsed, no user filter UI yet | Parsed, no user filter UI yet | Parsed, no user filter UI yet |

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

## Open spec questions

1. `untracked` timing: sent as an immediate disclaimer on GTK/Cocoa — Kitty's
   own timing (immediate vs on-drop) is ambiguous from the spec text alone.
2. `p=alive` on untracked stacks stays daemon-optimistic (daemon cannot see
   OS dismissals); whether Kitty prunes it on `untracked` is unverified.
3. `w=0` (never expire) is best-effort everywhere; OS policy may still retire.
4. No `filter_notification`-equivalent user config exists yet (`f=`/`t=` are
   parsed and carried for future filtering).
