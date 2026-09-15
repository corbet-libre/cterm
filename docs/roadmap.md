# Roadmap: terminal-landscape features

Candidate features gathered from a survey of the terminal landscape
(Kitty through 0.48.x, Ghostty 1.2/1.3, WezTerm, Contour, foot, iTerm2,
URxvt, Alacritty, Ghostty-source tooling, plus new cross-terminal
proposals such as bracketed-paste MIME mode and OSC 133 extensions),
de-duplicated against what cterm already ships (ligatures, scrollbar
overlay, DECSET 5522 paste events, Kitty keyboard/graphics/DnD/transfer
support, and the rest of `README.md`).

Ground rules: Safe Rust only (no `unsafe`, no Lua scripting layer —
Lua stays a later conversation). Every item lands with the same
evidence discipline as `parity.md`: headless behavioral tests first,
live differentials where a peer exists, native/compositor verification
where only real backends prove it.

| # | Feature | Source / Standard | Status in cterm | Value | Effort |
|---|---|---|---|---|---|
| 1 | Hints mode (keyboard selection of URLs/files/words as overlay) | kitty hints, WezTerm QuickSelect | Core done (URL + path matchers, cell-accurate coords, home-row labels, 256-match cap); overlay + open/copy/paste actions follow per backend | High — core mouseless workflow | Medium |
| 2 | OSC 133 `click-events` + `cl=line` (click into active prompt moves cursor) | Ghostty 1.3, Fish 4.1+, Nushell, semantic-prompts proposal | Core done (`click_events=1&#124;2`, `special_key=1`, prompt detection, arrow-walk; `cl=line` needs the proposal text); native mouse-hook wiring open | High — felt daily, shells already support it | Small–Medium |
| 3 | ConEmu OSC `9;4` progress (native GUI progress bar) | ConEmu, Ghostty 1.2 renders it | Core done (parse + per-session state, all five states, clamping); renderer surfacing follows per backend | Medium — visible builds/downloads | Small |
| 4 | Broadcast input to all panes/splits | tmux synchronize-panes, WezTerm | Shared action + config + GTK fan-out done (per-tab toggle, per-pane encoding, releases included); Cocoa/Win32 deferred, pastes follow later | Medium — parallel deploys/logs | Small–Medium (pure app logic) |
| 5 | Undo close tab/pane (time-limited) | Ghostty 1.2 (10 s default) | Missing (only crash recovery) | Medium — saves accidental closes | Small |
| 6 | Background images (+opacity/fit/position) | Ghostty 1.2, Kitty | Done on GTK (file + opacity + cover/contain/stretch/tile, screenshot-verified with text over image); Cocoa/Win32 follow | Medium — most-requested look | Medium |
| 7 | Rich copy (HTML) on GTK/Win32 | Ghostty 1.3 (multi-MIME) | Done on GTK (union provider: `text/plain` + `text/html`); macOS/Win32 already had it | Medium — formatted paste everywhere | Small–Medium |
| 8 | Search UI overlay (highlight all matches + prev/next) | Ghostty 1.3 | Core done (cell-accurate matches, highlight state, wrap navigation) + GTK highlight rendering and F3/Shift+F3; Cocoa/Win32 follow, non-modal bar later | Medium — completes the regex search | Medium |
| 9 | APNG/WebP animation + ICC profiles in image decode | kitty icat 0.45 | WebP + NetPBM decode with magic routing (APNG default image via PNG path); frame animation + ICC conversion follow | Medium — animations/faithful color via graphics paths | Medium |
| 10 | Command-finished notifications (slow/unfocused commands) | Ghostty 1.3 (`notify-on-command-finish`) | Done (OSC 133 C/D status+cmdline, policy in shared core, all three frontends install it from config; off by default) | Medium — builds/tests on the side | Small–Medium |
| 11 | Key tables / chained keybinds | Ghostty 1.3 (tmux-style modal) | Flat shortcuts only | Medium (power users) | Medium |
| 12 | In-band window-resize notifications (DEC 2048) | Kitty 0.36 | Done (mode + DECRQM + save/restore + `CSI 48` resize reports) | Low–Medium — completeness | Small |
| 13 | Working-directory inheritance (tabs/splits/windows) | Ghostty 1.3 | Tabs/splits independently configurable on GTK (default: inherit); new windows and Cocoa/Win32 follow later | Low–Medium | Small |
| 14 | Unicode tables current | Unicode 17 | Verified current (`unicode-width` 0.2.2 is the newest compatible release) | — | Done |
| 15 | Cursor trail / smooth / pixel scroll | Kitty, Ghostty 1.3 | Missing | Low — cosmetic | Small–Medium |
| 16 | Bell features (configurable, not just a signal) | Ghostty 1.2 (`bell-features`) | Basis only (visual-bell slot) | Low | Small |
| 17 | CVE hardening audit (invalid PNG-direct, invalid compose offsets) | Kitty CVE-2026-33633/33642 | Done (garbage-as-PNG, lying IHDR dimensions, hostile compose offsets incl. unknown images: all fail closed, no panic, image store stays consistent) | Security — proven explicitly | Small |

Deliberately excluded: remote-control execution (fail-closed by security
decision), a WezTerm-style Lua multiplexer layer (cterm's daemon
architecture already owns session persistence; Lua scripting itself is
deferred), AppleScript/platform-only integrations.

Suggested order, remaining: #1 overlay/actions and #8 non-modal bar
(medium UI pieces), #11 after the config-schema decision above, then
#15/#16 as filler. Everything else on this page is done or
explicitly parked.

## Needs decision (Julian)

These are investigated and ready to implement once decided. None is
blocked on hardware or providers.

- **#5 undo close tab/pane.** Tab close kills the daemon PTY today
  (`destroy_session`), so true undo (running process preserved, Ghostty
  semantics) needs either (a) daemon-side grace: delay the kill N seconds
  with reattach, or (b) UI-side soft close: detach instead of destroy and
  reap after the window, keeping the process alive without a visible tab.
  Both change process-lifetime semantics. Cheaper alternative: (c) respawn
  the same command+cwd (no running state). Recommendation: (b) with a
  short window, explicit in the ledger.
- **#11 key tables / chained keybinds.** Needs a config-schema decision:
  (a) new `[key-tables.<name>]` sections mapping key strings to existing
  actions plus `enter-table`/`exit-table` actions, or (b) inline sequence
  syntax in `[shortcuts]` (`"Ctrl+A>Ctrl+B"`). Either way the
  `ShortcutManager` gains a table stack (one-shot + sticky), all
  frontends inherit resolution, and a pending-state indicator follows per
  backend. Recommendation: (a), tables opt in per binding.
- **#6 follow-up: transparency rendering.** OSC slots and config opacity
  round-trip exactly; no backend applies them. GTK/Wayland has no client
  opacity protocol (document as platform limitation, like NumLock);
  Cocoa (`NSWindow.alphaValue`) and Win32 (layered windows) are
  implementable but need macOS/Windows verification. Open question: which
  slot drives the window (slot 1? active-slot convention?) — Kitty ties
  slots to its own compositor pipeline, so this needs a cterm-side
  decision, not just copying.
- **#9 follow-up: animated APNG/WebP playback + ICC conversion.**
  First frames decode today. Full animation needs multi-frame decode
  plumbed into the existing Kitty frame store; ICC needs a color-management
  library choice (pure-Rust `qcms` vs system lcms2) — dependency decision.
- **#1 follow-up: hints overlay + actions.** Core matcher/labels are in.
  Remaining: overlay rendering per backend plus open/copy/paste actions
  (open needs a URL handler policy: `xdg-open` equivalent per platform —
  mild security surface, decide the allowlist approach with the
  transfer-prompt philosophy in mind).
