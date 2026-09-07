# Changelog

All notable changes to cterm are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project loosely follows semantic versioning. The whole workspace shares
a single version via `[workspace.package]`. Pure CI, lint, and formatting churn
is omitted for readability.

## [Unreleased]

### Added
- Daemon event-bus integration evidence: `StreamDaemonEvents` lifecycle
  (created/destroyed) and metadata-change broadcasts now covered end to
  end over real gRPC (`cterm-headless` integration tests, incl. the
  zero-mask silent update); owned-session dedup sets prune on destroy so
  they track live sessions only (GTK unit-tested, Cocoa foreign-checked).
- Win32 `w=0` never-expire: no terminal timer is scheduled anymore, so the
  balloon persists until replaced/closed/evicted/exit like GTK/Cocoa
  (foreign-checked only; needs a Windows host to verify).
- Kitty transparency key parity: `transparent_background_color8` accepted
  (Kitty takes indices 1-8) and empty `=` values ignored instead of
  storing `Dynamic`, matching `kitty/window.py`.
- Notification `filter_rules`: Kitty `filter_notification` deny rules
  (`field:regexp` over title/body/app/type with and/or/not/parens/quotes
  and `all`, unanchored Rust-regexp search, invalid rules logged and
  ignored), enforced in the shared model on GTK alongside the allow/deny
  lists.
- DECSACE attribute-change extent (`CSI Ps * x`): DECCARA/DECRARA honor the
  exact rectangle (`2`) or the scan-order stream (`0`/`1`/missing, the DEC
  power-on default, matching Kitty), with reset on RIS/DECSTR, a
  Kitty-faithful DECRQSS report (`1`/`0`), daemon-snapshot round-trip, and
  xterm-faithful DECRARA color skipping pinned by tests.
- Merged upstream `KarpelesLab/cterm` tab-sync work (through `df1d883`):
  live tab sync across every cterm connected to the same daemon via a new
  daemon-wide event stream broadcasting session lifecycle (created/destroyed)
  and tab-metadata (custom title / color) changes, so a tab opened, closed,
  renamed, or recolored in one client now appears and updates in all of them.
  Works on macOS and GTK, for the local daemon and SSH-tunneled remote daemons
  alike.
- Notification hardening: pinned `w=0` never-expire semantics (no terminal
  timer; persists until replaced, closed, evicted, or frontend exit; daemon
  `p=alive` survives UI reconnects) with activation/close ordering guarantees,
  plus `[notifications]` user filtering (`allow_apps`/`deny_apps` over `f=`,
  `allow_types`/`deny_types` over `t=`; default allow-all; deny wins),
  enforced in the shared model before delivery. Cocoa/Win32 filtering and
  the Win32 `w=0` timer gap remain open.
- Deferred Cocoa/Win32 native mappings, foreign-target type-checked
  (`aarch64`/`x86_64-apple-darwin`, `x86_64-pc-windows-msvc`) but still
  awaiting macOS/Windows runtime verification: OSC 22 pointer-shape tables
  (`NSCursor`: 16 exact, 14 documented fallbacks; `IDC_*`: 21 exact, 9
  documented fallbacks) with GTK-matching hover precedence; Kitty
  selection/cursor-text/visual-bell renderer hooks on both backends
  (transparency slots stay stored-and-queried); and deny-by-default
  rich-clipboard (OSC 5522) consent prompts mirroring the GTK adapter
  (Allow once / Always allow with exact-token storage / Deny, exact-token
  display, 120 s frontend-local expiry, lifecycle cancellation through the
  existing consent seams).
- Daemon transfer-consent integration with defaults unchanged (still
  always-prompt out of the box; no policy decided): the operator `[transfer]`
  config plus one process-lifetime bypass keypair are plumbed from daemon
  startup into every session's transfer worker, `evaluate_transfer_bypass_with_policy`
  is consulted at session start before any prompt publishes (`Allow`
  auto-approves and `Deny` hard-refuses through the same manager calls a
  dialog decision uses, so PTY bytes match a clicked Allow/Deny; `Prompt`
  keeps today's path byte-identical), and `KITTY_PUBLIC_KEY` is advertised to
  every local PTY child exactly as Kitty does (per-process key,
  `kitty/boss.py:425`; env set for every child, `kitty/child.py:385`;
  client-supplied values overwritten; private half held in daemon memory
  only, never written to disk, rotating each restart with stale values
  failing closed to prompt). Headless regression tests pin allow/deny/ask,
  keypair generation + advertisement, and wrong-password-still-prompts.
- Live GTK drag-source sessions from the negotiated OSC 72 offer: the
  `GtkDragSource` serves pre-sent bytes via `ContentProvider` with the
  negotiated copy/move/either operation mapped to the matching `GdkDragAction`
  mask (`drag-begin` confirms `t=E;OK`, `drag-end`/`drag-cancel` report
  finish/abort), the gesture arbitrates Kitty-faithfully (left-press pixel
  anchor, `t=o` emitted once past 5 px while armed — `kitty/mouse.c:677-686`,
  `drag_threshold` default 5 — with text selection proceeding regardless),
  and remote (`t=k`) reads surface the clipboard adapter's consent semantics
  (modal Allow once / Always allow surface-scoped / Deny-default with
  exact-token display, drag-lifecycle expiry, and close-means-deny).
   Headless unit tests cover action mapping, arbitration, content selection,
   and prompt plumbing; real drag takeover, drop delivery, and dialog
   interaction still need the Wayland compositor smoke.
- Live Cocoa/Win32 drag-source sessions from the negotiated OSC 72 offer,
  foreign-target type-checked (`aarch64`/`x86_64-apple-darwin`,
  `x86_64-pc-windows-msvc`) but still awaiting macOS/Windows runtime
  verification: Cocoa builds the live `NSDraggingSession` (pre-sent bytes via
  `NSPasteboardItem` with `text/plain` also as `NSPasteboardTypeString`, PNG
  via `NSPasteboardTypePNG`, file content via `NSFilePromiseProvider` from
  retained ingress, copy/move/either mapped to the matching `NSDragOperation`
  mask, session creation confirms `t=E;OK`, `endedAtPoint:` reports finish,
  `CancelOsDrag` tears down) and Win32 runs OLE `DoDragDrop` with an
  `IDataObject` serving the negotiated formats (`CF_UNICODETEXT`/`CF_TEXT`
  plus registered MIME formats, `CF_HDROP` from staged temp files holding
  retained ingress bytes, copy/move/either mapped to `DROPEFFECT`,
  `DoDragDrop` return reports finish/abort, `CancelOsDrag` cleans staging);
  both arbitrate the gesture Kitty-faithfully (primary-press pixel anchor,
  `t=o` once past 5 px while armed, selection/tracking never suppressed) and
  prompt different-machine `t=k` reads with the rich-clipboard semantics
  (modal Allow once / Always allow surface-scoped / Deny-default with
  exact-token display, 120 s frontend-local expiry, drag-lifecycle expiry,
  close-means-deny; same-machine drags never prompt). Shared session-plan
  helpers (`classify_drag_mime`, `servable_drag_payloads`, copy/move matrix,
  bounded sanitized uri-list file entries) plus per-backend headless unit
  tests (gesture/effect/format/prompt, Cocoa 9 + Win32 9 green without OS);
  custom `t=p` drag images stay a documented gap (AppKit synthesizes from
  item contents; Win32 serves bytes without image mapping), as do live
  session takeover, file delivery, operation negotiation, dialog interaction,
  and staged-file cleanup timing.
- Kitty-precursor alignment fixes, verified against Kitty 0.48.2 sources:
  OSC 5522 malformed reads silently ignored with a log and no reply
  (EINVAL stays write-side only); drag-source `t=k` exact-token approval
  gated to different-machine/unknown-locality reads while same-machine drags
  proceed without prompting (Kitty has no drag-source prompt; remote gating
  is deliberate hardening); one shared saved-modes store for explicit
  (`CSI ? Pm s/r`) and bare (`CSI ? s/r`) save/restore, fully bidirectional;
  OSC 22 `?` probe answering 1 only for CSS-valid names (0 for set-only
  xterm aliases like `left_ptr`, which stay settable); and source-side `t=E`
  split semantics (valid index fails only that request, `y=-1`/missing/
  out-of-bounds aborts the drag) with finish-only `t=e:x=4:y` and `x=2:o=`
  as the action-changed hint (final copy/move lives destination-side
  `t=r:o=`).
- Compositor-free graphics-pipeline differential suite (tests and fixtures
  only): a 41-vector pipeline suite
  (`crates/cterm-core/tests/graphics_differential.rs`) pinning Sixel decode
  to RGBA (primaries, repeat, CR/LF geometry, raster hints, DEC mode 1070
  private/shared palettes, palette-overflow clamping, byte/dimension budgets),
  Kitty transfers (raw RGB/RGBA, PNG, zlib round-trip plus truncated-stream
  rejection, 2- and 3-chunk reassembly with byte-level split invariance,
  interleaved-delete abort, crop/scale/offset geometry, AlphaBlend vs
  Overwrite order, cursor and layer metadata), iTerm2 PNG/GIF inline display,
  cross-protocol equality (same 2x6 red/blue source through Sixel vs Kitty-raw
  vs Kitty-PNG vs iTerm2-PNG byte-equal; quality-95 JPEG documented with a max
  32/255 per-channel delta), and hostile bounds that fail closed within the
  documented budgets (64 MiB, 10,000 px Kitty/Sixel, 4096 px image-decode cap:
  zero/oversized dimensions, RGB-expansion bombs, truncated PNG/zlib/base64,
  bad chunk markers, palette-management clamping, oversized iTerm2 images);
  plus 7 quota/composition determinism vectors in
  `kitty_graphics/tests.rs` (transient N=1 priority regardless of insertion
  order, oldest-first within a hint class, placed images never evicted for
  unplaced data, deterministic victims, blend/overwrite order, zero/oversized
  raw dimensions, 3-chunk reassembly). Native/compositor pixel assertions on
  Cocoa, GTK, and Win32 remain open.
- Kitty keyboard differential fixtures, compositor-free half (tests and
  fixtures only, no encoder change): `crates/cterm-core/tests/keyboard_differential.rs`
  pins the shared-Rust encoder with table-driven input → escape-sequence
  vectors and a replay recipe for macOS/Windows contributors — the full
  32-combination progressive-flag matrix for press/repeat/release, release
  gating (releases require event-types even in all-keys mode), alternate-key
  vectors with the MAC-ANSI vs PC-101 base-identity contract, modifier vectors
  including CapsLock/NumLock, dead-key compose/cancel and IME commits at the
  `handle_text_input` seam, focus-in/out vs release-drain semantics, flag-stack
  pop-imbalance/overflow and per-screen isolation, and hostile associated-text
  handling. Native dead-key/IME/alternate-key/release verification per platform
  remains open (needs a compositor, macOS, and Windows hosts).
- Kitty OSC 99 desktop-notification completion over one shared
  `cterm-core::notifications` model: `a=report` activation reports (whole
  notification plus 1-based button numbers) and `c=1` close reports
  (`untracked` where the OS gives no dismiss signal, real closes on
  terminal-observed activation/expiry), `p=icon`/`n=`/`g=` icons with a
  bounded cache, `p=buttons` buttons, `o=` occasion filtering,
  `f=`/`t=` filter identity, named `s=` sounds, `w=` terminal-enforced
  expiry, `p=alive` liveness on a bounded live-ID set, and a capability
  reply advertising exactly the wired set. GTK delivery is fully wired
  (targeted actions, icons, expiry, reports via the mirror PTY path);
  Cocoa/Win32 reuse the same core seams (not compiled on Linux — see
  `docs/notifications.md` for the per-platform matrix and open questions).
- Foot differential corpus expansion (tests and fixtures only): a 27-vector
  host-side parser/screen corpus (`tests/foot_corpus.rs`) across editing,
  DECSET/DECRST incl. Kitty 5522/2026/2031/2033/1070/8452 plus keyboard and
  safe save/restore, DA/DSR/DECRQM/DECRQSS/XTGETTCAP plus OSC 21/22/5522,
  grapheme-safe/wide/multicell resize/reflow, combining/ZWJ/flags/variation
  selectors, and truncated/hostile/mixed malformed input; a bounded
  5000-iteration parser fuzzer (`tests/parser_fuzz.rs`, fixed seed, response
  and scrollback bounds, 30 s hang guard); a 31-case foot probe
  (`examples/foot_compat_probe.rs`, same CI script/harness) adding
  editing-via-DSR, standard/sync/sixel modes, keyboard query, boolean
  XTGETTCAP, char-cell window ops, Sixel colors, and malformed recovery;
  vim and less real-session TUI fixtures (`tests/tui_e2e.rs --ignored`).
  Version/pixel-dependent replies (DA2/XTVERSION, pixel geometry) stay
  host-side only; screen/emacs/htop remain documented skips.
- Kitty screen unscrolling (`CSI Ps + T`): scroll down within the scroll
  region, refilling the new top lines from scrollback instead of blanking
  them. Falls back to blank lines on the alternate screen or with empty
  scrollback; ordinary `CSI Ps T` behavior is unchanged.
- A living, evidence-backed foot and Kitty parity inventory that distinguishes
  implementation from user-observable verification.
- Kitty OSC 5113 decoding and exact-token consent, plus bidirectional
  consent-listed files and directory trees with symlink/hardlink identity,
  metadata, bounded plain/zlib streaming, Kitty-compatible rsync/XXH3 deltas,
  private handle-relative staging, cumulative resource limits, deterministic
  cancellation cleanup, and per-entry atomic commit at session finish.
- Kitty OSC 72 destination negotiation and local file drops on GTK/Wayland,
  Cocoa, and Win32/OLE.
- Kitty OSC 72 drag sources: shared-core outbound negotiation (application
  arm/disarm, user-gesture gating, chunked MIME offers, pre-sent data up to
  64 MiB, drag-image validation, copy/move/either mapping with change-only
  progress reports) with fail-closed remote (`t=k`) entry reads behind
  exact-token approval, routed on GTK/Wayland, Cocoa, and Win32. Native OS
  drag sessions, consent prompts, and local-direct reads remain open: session
  requests fail closed (`ENOSYS`/deny) instead of claiming `OK`.
- A fail-closed command-plugin foundation with fixed WebAssembly packages,
  strict manifests, content-addressed trust, exact per-action grants, and a
  bounded versioned protobuf ABI, plus a separate one-shot Wasmi/WASIp1 runner
  with digest revalidation, fuel, memory, stack, table, input, and output
  limits. The application-side broker adds canonical package-relative host and
  plugin resolution, deterministic command discovery, atomic machine-local
  grant persistence, stale-descriptor rejection, exact pre/post-execution
  grant enforcement, typed native-action conversion, bounded framed process
  I/O, a strict wall timeout, and Unix process-group / Windows Job Object
  termination. GTK/Wayland, Cocoa, and Win32 expose discovered commands in a
  native Tools → Plugins menu, show exact native grant prompts, execute on
  worker threads, and dispatch validated results through their existing native
  action policy. Managed mode disables the complete plugin surface. Release
  packages include the runner.
- Native FreeBSD builds now compile the GTK4/Wayland client and daemon and run
  their portable library and daemon-integration tests in a FreeBSD 14.4 VM.
- Foot-compatible independent cursor blink sources and native cursor/text
  blink rendering across GTK/Wayland, Cocoa, and Win32, including distinct
  slow and rapid SGR phases and daemon snapshot persistence.
- Kitty OSC 22 mouse pointer shapes: application set/push/pop/query with
  per-screen stacks, daemon snapshot persistence, and GTK cursor mapping
  (Cocoa/Win32 native mappings pending).
- Kitty OSC 21 color control (selection, cursor-text, visual-bell, and
  transparency keys with query/set/reset and strict value validation) plus
  OSC 30001/30101 palette-stack push/pop on the shared xterm stack, with
  daemon-owned replies and snapshot persistence.
- Kitty/X11 named colors for dynamic color specs: the 753-entry table from
  Kitty's color-control spec (X11 `rgb.txt` values as an independent static
  table, recorded in `THIRD_PARTY_LICENSES.md`) resolves case-insensitively
  in `OSC 21`, `OSC 10-12`, and `OSC 17/19` values, with an optional
  `@alpha` suffix on the Kitty spelling. Unknown names are still rejected.
- xterm highlight colors (`OSC 17`/`19` set/query, `OSC 117`/`119` reset,
  matching foot) aliased to the Kitty selection slots, so both spellings
  share state, snapshots, and the palette stack with cross-spelling
  round-trips.
- GTK rendering of the Kitty color slots: explicit selection
  background/foreground paint, cursor-text color, and a brief visual-bell
  flash tint through the existing bell path. Transparency slots remain
  stored-and-queried (GTK has no applied window-opacity mechanism);
  Cocoa/Win32 renderer mappings are still open.
- Kitty OSC 5522 rich clipboard: typed MIME reads/writes with strict
  validation, write sessions with alias resolution, MIME-list and DECRQM
  queries, paste-event mode with single-use tokens, exact-token consent
  reuse with deny-by-default prompts, and per-backend native adapters
  (GTK consent prompt + text/plain, Cocoa text/HTML, Win32 text/plain),
  with daemon snapshot persistence of the paste-event mode. Text OSC 52
  behavior is unchanged.
- Kitty all-SGR DECCARA: rectangular attribute changes now apply the full
  SGR repertoire (every boolean attribute, styled underlines, and
  fg/bg/underline colors in ANSI, indexed, and truecolor forms, including
  colon subparameters and empty-list reset) while honoring origin mode,
  scrolling margins, active-page clipping, and cursor preservation.
  DECRARA toggles the full boolean set and deliberately leaves colors
  untouched, since color reversal is undefined. No renderer change was
  needed: rectangle-styled cells are identical to SGR-styled ones.
- Kitty no-argument save/restore of safe terminal modes (bare `CSI ? s` /
  `CSI ? r` over ANSI 4/20 and the side-effect-free DEC set, excluding
  DECOM/DECCOLM), plus Kitty private DCS `echo`, `print`, and
  `restore-cursor-appearance` with strict validation. Remote control and
  the kitten/overlay/ssh/ask/clone/edit commands stay fail-closed.
- Headless scripted consent driver for Kitty OSC 5113 transfers: a
  compositor-free dialog-result seam (`DialogOutcome` with fail-closed
  cancel/expiry precedence over simultaneous approval plus exact-token
  approve/deny/expire/cancel/reconnect-replay coverage). The final native
  dialog hop on GTK/Cocoa/Win32 still needs UI automation.
- Default-deny transfer bypass hook (`TtyTransferBypassKind`,
  `evaluate_transfer_bypass` always resolving to prompt, with regression
  tests locking that in) plus a decision-pending bypass policy proposal in
  `docs/transfer-bypass-policy.md`. No enforcement was weakened.
- Kitty file-transfer differential vectors, verified against live Kitty
  0.48.2: a serialized codec corpus parsed to identical fields, byte-exact
  rsync signatures/deltas applied unmodified, Kitty-matched round(sqrt)
  signature block sizing, padded/unpadded base64 agreement, chunk-split
  invariance, and decompressed-byte session accounting
  (`scripts/kitty_transfer_vectors.sh`).
- Local performance regression gate for the deterministic cterm-core
  Criterion suite (no CI change): `scripts/check_perf_baseline.sh`
  (`mint`/`check`, same-host relative change, FAIL past +10% median with the
  95% confidence interval above zero, optional host-local absolute ceilings,
  `--quick` smoke mode) plus a bench inventory, threshold scheme, and
  proposed-not-applied CI wiring in `crates/cterm-core/benches/README.md`.
  Plain `cargo bench -p cterm-core` behavior is unchanged.
- Transfer consent-model mechanism with defaults unchanged (still
  always-prompt out of the box; no policy decided): Kitty-compatible `pw`
  bypass verification (`sha256(request_id + ";" + password)` with
  constant-time compare plus the `kitty-1:` AES-256-GCM JSON envelope with
  ±5-minute timestamps, checked against Kitty 0.48.2's `check_bypass`), an
  opt-in `[transfer]` config table (`bypass_password` unset by default;
  per-direction `send`/`receive` as `allow`|`ask`|`deny` in Kitty
  `clipboard_control` vocabulary, unset/invalid resolving to `ask`), a
  policy-gated evaluation seam (`deny` hard-refuses; `allow` additionally
  needs a configured password plus a verifying credential on session-start
  commands; everything else prompts, and the legacy always-`Prompt` entry
  point is untouched), and one audit row per decision (request id, transfer
  id, direction, decision, bypass kind, authorizing token hash only).
  The daemon worker does not consult the new seam yet, so live transfers
  still always prompt; flipping any default needs Julian (see
  `docs/transfer-bypass-policy.md`, whose 5 open questions stay open).

### Fixed
- Streamed OSC 1337 interception now replays incomplete, cancelled, and
  oversized sequences atomically instead of leaving the VTE parser inside a
  partial control string. Large replay and decoded-transfer spill files are
  unique and private, and failed receptions remove their temporary storage.
- Windows file-transfer hardlinks are created from retained source and
  destination handles, preserving private staging and ancestor-rename safety.

### Security
- Plugin package files are size-checked before allocation, read through a
  strict `limit + 1` bound, and rejected if their length changes during the
  read.
- Added a hard RustSec advisory gate, upgraded `anyhow`, `bytes`, `h2`, and
  `tar` to patched compatible releases, and removed the unused, unmaintained
  `bincode` dependency.

## [0.0.20] - 2026-08-31

### Added
- Direct command launch with exact argv, working directory, environment, title,
  and initial native window state, plus an isolated managed-product mode with
  authenticated local daemon handshakes and exact identity/version matching.
- Cross-platform inline Sixel and iTerm2 image rendering and persistence, plus
  font-independent box, block, Braille, and legacy-computing glyph rendering.
- Foot-compatible terminal behavior including palette stacks and dynamic
  colors, native theme/visibility reports, DEC rectangular editing,
  synchronized updates, reverse wrap/video, DEC special graphics, keypad
  identity, modified and Kitty keys, capability/status queries, pixel and
  URXVT mouse reporting, OSC 7 working-directory tracking, and colon-form SGR
  colors.
- OSC 133 shell integration with prompt navigation and native OSC 9/777 and
  Kitty OSC 99 desktop notifications on macOS, Wayland, and Windows.
- Foot-compatible Sixel cursor placement, aspect ratios, private/shared
  palettes, resource limits, geometry/color replies, and up to 1024 colors.
- Native split panes on AppKit, GTK4/Wayland, and Win32/Direct2D, including
  draggable dividers, directional focus and resize, pane zoom, and configurable
  shortcuts. New panes inherit the active pane's exact process or SSH launch
  context and its working directory where the target daemon can honor one;
  native SSH keeps the same target while the remote login shell selects its
  initial directory.
- Deterministic parser/render/reflow/scrollback benchmarks, real-TUI and foot
  differential compatibility tests, authoritative native UI automation, and
  FreeBSD core CI.

### Changed
- Establish FSL-1.1-ALv2 as cterm's product license, with Apache-2.0 becoming
  available automatically after two years per version. Source inherited from
  KarpelesLab/cterm and Rio/Sugarloaf retains its MIT grants and notices.
- Store complete extended grapheme clusters and safely reflow scrollback when
  the terminal resizes.
- Seamless-upgrade state now preserves every window, tab, complete split
  topology, pane session, and exact process/SSH launch context.
- Linux and Windows client packages now bundle the matching `ctermd`; every
  distributed client and daemon package carries the product and inherited-code
  license notices and a SHA-256 checksum sidecar.

### Fixed
- Render scrollback and terminal attributes consistently across Direct2D,
  CoreGraphics, and GTK/Cairo.
- Windows child processes now keep their standard streams attached to ConPTY,
  including when ctermd itself runs with redirected handles.
- Unix PTYs now use the platform login-terminal setup, removing intermittent
  controlling-terminal failures on FreeBSD.
- OSC 10/11/12 color queries now receive theme-accurate replies from the
  attached frontend. Dynamic foreground, background, and cursor colors support
  foot-compatible XParseColor forms, reset through OSC 110/111/112, render on
  every desktop backend, and survive daemon reconnection.
- Restoring pane snapshots no longer leaks daemon attachment counts, and
  independent windows connected to the same SSH host no longer overwrite or
  tear down each other's tunnel registry entries.
- GTK tab context menus activate reliably, and SSH dialogs remember and
  autocomplete earlier targets.
- Update checks and remote daemon bootstrapping use the `corbet-labs` release
  source. Client assets and checksums are matched by exact platform filenames,
  Linux archives are validated before relaunch, and daemon-only assets can no
  longer be mistaken for client updates.
- Cocoa split-pane layout releases mutable model borrows before re-entry, and
  obtains its CoreGraphics drawing context through the typed AppKit binding.
- Merged upstream: macOS no longer forces every remote tab green on connect —
  a tab keeps the color it was given (or none), matching the local reconnect
  path.
- Merged upstream: the GTK "SSH Remote" dialog closes immediately on Connect,
  like macOS.

## [0.0.19] - 2026-07-09

### Added
- Mouse-event forwarding parity on macOS: right/middle-button reports, drag
  motion, and alternate-scroll (DECSET 1007) so pagers (less/man/vim) scroll via
  the wheel even without mouse tracking. Holding Shift bypasses reporting so
  selection, scrollback, and context menus keep working under a tracking app.

### Fixed
- SSH-tunneled sessions no longer drop at the ~1h mark: the mid-session rekey
  fault (OpenSSH's default rekey interval), which surfaced as an opaque h2
  "error reading a body from connection" cascade, is fixed by puressh 0.1.3.
- The SSH tunnel's serve loop now logs its termination cause at `warn` (rekey
  fault, keepalive timeout, decrypt/MAC error, peer EOF) instead of `debug`, so
  the real reason for a mid-session disconnect is visible.

## [0.0.18] - 2026-07-07

### Added
- Native SSH via puressh, replacing the system `ssh` binary; the self-updater
  now uses rsurl instead of reqwest (MSRV 1.88).
- Interactive SSH auth for the remote tunnel: keyboard-interactive, plus native
  host-key / password / passphrase prompt dialogs on macOS, GTK, and Windows.
- Run gRPC directly over the SSH channel — no locally forwarded socket file.
- Jump-host chains via a `>` separator (`bastion:2222>10.0.0.5`), plus SSH
  connection history in the connect dialog.
- Default `~/.ssh/id_*` identity files are loaded automatically (including
  PKCS#1/SEC1/PKCS#8 and `id_xmss`); identities are offered lazily via their
  `.pub` and only decrypted on demand.
- zlib compression (`zlib@openssh.com`) on the gRPC daemon tunnel, cutting the
  transfer for screen snapshots and scrollback.
- Mouse-event forwarding and alternate-scroll in the GTK and Windows terminals.

### Fixed
- Reconnecting a window with many tabs no longer stalls: sessions attach
  concurrently, RPCs no longer serialize on the connection mutex, and each tab
  fetches its screen snapshot only once (no redundant scrollback transfer or
  placeholder resize).
- Detect a stale `ctermd` socket by connecting rather than trusting the PID
  file, and prevent a daemon deadlock from hanging cterm startup.
- Cross-platform SSH build fixes (gate ssh-agent to Unix; Windows
  `EM_SETPASSWORDCHAR` cast).

## [0.0.17] - 2026-06-22

### Added
- Hyperlink (OSC 8) rendering, hover, and interaction across GTK, macOS, and Windows.
- Streaming input RPC with batched fallback for low-latency typing.
- Custom SSH port in remote dialogs (`user@host:port`).
- Scrollbar overlay for the terminal view (macOS + GTK).
- Bell/alert state managed through the `ctermd` daemon; alerted tabs are visually distinct.
- Serialize DRCS soft fonts and charset state across gRPC reconnection.
- Confirm close when a foreground process is running in daemon sessions; auto-close tabs when the shell exits.
- New tabs inherit daemon context from the current tab (macOS); SSH Remote attaches to all existing sessions.
- Disconnect action in the remote tab right-click menu.
- Raise the gRPC message size limit to 64 MB for large scrollback snapshots.
- Enable SSH compression (`-C`) on remote tunnels by default.

### Fixed
- Keep word/line selection stable across scrollback wrap.
- Connect to the correct daemon for remote SSH sessions; keep the SSH tunnel alive across tokio runtimes.
- GTK4 tab bar styling, close button, auto-close on shell exit, and Ctrl+PageUp/PageDown navigation.
- Persist custom tab title to the daemon from the Set Title menu action.
- Double-borrow panic when closing a tab via the close button.

### Removed
- Experimental mosh, Latch, relay, and "unixshells" integrations (prototyped during this cycle, then removed before release).

## [0.0.16] - 2026-03-15

Daemon-centric architecture: all sessions now run through `ctermd`.

### Added
- Route all terminal sessions through the `ctermd` daemon, with attach/detach semantics so sessions survive UI restarts and seamless upgrades.
- New `cterm-client` library and `cterm-proto` crate for daemon communication over gRPC.
- Daemon session reconnection, lifecycle management, and graceful SIGTERM shutdown.
- Remote host management with automatic `ctermd` install; SSH remote support over stdio/socket forwarding.
- Incremental screen updates in `StreamScreenUpdates`.
- Persist tab metadata (color, title, template) in the daemon.
- macOS daemon-backed terminal view and session menu; GTK daemon-backed terminal widget.
- "Kill Local ctermd" and relaunch-in-place debug menu items across all frontends.

### Changed
- Simplified the upgrade protocol; removed standalone crash recovery in favor of daemon-backed sessions.

### Fixed
- Preserve screen state, custom tab titles, colors, template, window frame, and active tab across daemon relaunch/upgrade.
- Smarter daemon auto-shutdown by tracking active streams; destroy sessions on tab close.
- Raise the file descriptor limit at startup; restore it for child processes.

## [0.0.15] - 2026-03-10

### Added
- macOS: render bold, italic, and dim (SGR 2) text; `bold_is_bright` option.
- GTK4: input method (IM) support for Japanese/CJK input, Ctrl+PageUp/PageDown tab switching, and libadwaita menu styling with visible keyboard shortcuts.
- Include dots in word selection (e.g. version strings).

### Fixed
- Word selection across wrapped line boundaries.
- GTK4: reset scroll to bottom on input, scrollback rendering on mouse scroll, Ctrl+Shift shortcuts, menu display, and window title on tab switch.
- Close the PTY master FD on tab close (FD leak); set `FD_CLOEXEC` on PTY master and watchdog socket FDs.
- Capture the executable path at startup for reliable relaunch.

## [0.0.14] - 2026-02-21

### Added
- Auto-scroll when dragging a selection beyond the terminal bounds.
- Show open tabs with custom names in Quick Launch.
- Expand shell variables (`~`, `$HOME`, `${VAR}`) in config paths.
- Cmd+Shift+T shortcut for Set Title.

### Fixed
- Address security-audit findings for input bounds and file safety.
- Theme selection now persists across restarts.
- Draw full-width underline/strikethrough/overline and background for wide characters on macOS.
- Preserve word/line selection anchor across drag-direction changes.

## [0.0.13] - 2026-02-13

### Added
- Set Title and Set Tab Color in the native tab context menu.

### Fixed
- Preserve custom titles, tab colors, and cwd across upgrades.
- Guard against use-after-free by checking `view_invalid` inside dispatch blocks.

## [0.0.12] - 2026-02-08

### Added
- OS-specific icon templates per platform; macOS full-canvas app icon.

### Fixed
- Spill scrollback to temp files during upgrade to avoid the 64 MB buffer limit.

## [0.0.11] - 2026-02-08

### Added
- macOS code signing and notarization in CI; `workflow_dispatch` trigger for manual builds.

### Fixed
- Find the signing identity dynamically from the keychain.

## [0.0.10] - 2026-02-07

### Added
- Configurable Tools menu with external tool shortcuts.
- File drag-and-drop support with an options dialog.
- "Next Alerted Tab" shortcut to cycle through bell-active tabs (all platforms).
- GTK tab context menu: rename and set color.

### Fixed
- Preserve word/line selection on mouseUp instead of clearing it.
- Emit the Bell event from the terminal process loop.

## [0.0.9] - 2026-02-06

### Added
- Confirm before closing tabs or quitting with running processes.
- Bell/alert notifications: macOS dock badge with count, and Windows support.
- GTK cross-platform seamless upgrade support.
- Windows Quick Open dialog and upgrade receiver.
- UI screenshot tests for Linux and macOS; window positioning menu items.

### Fixed
- Async OSC 52 clipboard query (GTK); Docker tab creation from the picker (win32).
- Preserve word/line anchor when extending a selection backwards.
- Skip the close-confirmation dialog during relaunch.

## [0.0.8] - 2026-01-30

### Added
- Command+1–9 shortcuts for tab selection.
- Platform-specific default fonts.
- Windows UI integration test infrastructure (PowerShell automation).

### Fixed
- Windows rendering not updating after PTY data; DirectWrite `E_INVALIDARG` on startup.
- Windows UI freeze and double-input bugs.
- Draw the cursor at double width for wide (CJK) characters.
- Send readline-style sequences for Option+Arrow on macOS.
- Preserve tab order during relaunch; various Quick Open input fixes.

## [0.0.7] - 2026-01-27

### Added
- Quick Open Template overlay (Cmd+G / Ctrl+Shift+G).
- New tabs inherit the working directory from the active terminal.
- Right-click tab context menu for rename and color.
- Dedicated Git Sync tab in preferences with a Sync Now button (macOS + Windows).
- Dynamically generated app icons with the version number; macOS app icon.
- `ctermd --scrollback` option; macOS auto-update installs the full app bundle.

### Changed
- Upgrade protocol now uses JSON with a versioned, backward-compatible header.

### Fixed
- Maintain scroll position when viewing scrollback history.
- Clear selection when the selected text is deleted or modified.
- Use `modes.show_cursor` for DECTCEM cursor visibility.

## [0.0.6] - 2026-01-26

### Added
- `ctermd` headless terminal daemon with a gRPC API (plus integration tests).
- Git-backed configuration sync and git remote support for tab templates.
- Open-tab-from-template feature (GTK).
- Configurable `TERM`/`COLORTERM` and focus-event support; locked background color for templates.

### Fixed
- Save crash state before relaunch to preserve buffers.
- proto3 optional support for `tonic-build`; CI build improvements.

## [0.0.5] - 2026-01-25

### Added
- Native Windows UI (`cterm-win32`) with feature parity to GTK/macOS.
- Windows seamless upgrade protocol.
- File transfer support across all platforms; GTK file transfer and Docker status display.
- macOS Check for Updates menu item.

### Changed
- Consolidate shared dialog code into `cterm-app`; consolidate PTY ownership and fix Windows DLL bundling.

### Fixed
- Numerous win32 build, Direct2D, and API-alignment fixes for the `windows` crate 0.60/0.61.
- Restore all tabs during a macOS seamless upgrade.

## [0.0.4] - 2026-01-24

### Added
- SSH remote connection support for tab templates.
- Full `devcontainer.json` support with Dockerfile building; auto-detect `devcontainer.json`.
- Tab color picker and modifier-key support; snap window resize to the character grid.
- In-app log viewer for debugging; reorganized template UI (General/Docker/Remote tabs).
- `CLAUDE.md` guidance file and `run.sh` helper.

### Fixed
- Restore window position, size, and all tabs after relaunch.

## [0.0.3] - 2026-01-24

### Added
- Graphics: Sixel, DRCS soft fonts, iTerm2 inline images (OSC 1337), and streaming file transfer for large files.
- OSC 8 hyperlink support; block/rectangular selection (Option+drag); mouse reporting; IME for Japanese/CJK input.
- Crash recovery with an FD-passing watchdog, periodic state saving, and display restoration.
- Docker configuration in the Tab Templates UI; devcontainer support.

### Fixed
- Many macOS fixes: focus/activation, scrollback scroll wheel, view resize, and several segfaults.
- Resize the tab-stops array when terminal dimensions change.

## [0.0.2] - 2026-01-21

### Added
- Native macOS UI using AppKit with CoreGraphics text rendering.
- Text selection with mouse support; tab templates with unique tabs; preferences window.
- State-preserving debug relaunch; secret debug menu; native window tabbing and keyboard shortcuts.
- Copy/paste/select-all; warn when closing a terminal with a running process.

### Changed
- Unify the binary entry point with platform-specific backends; remove redundant backend binaries.

### Fixed
- Arrow and special key handling; segfault on Command+W; focus handling after tab switch/upgrade; selection color inversion.

## [0.0.1] - 2026-01-14

Initial pre-release.

### Added
- Initial cterm terminal emulator: VT parser, screen buffer with scrollback, and a native PTY implementation.
- Menu bar (File, Edit, Terminal, Tabs, Help); clipboard paste, zoom, tab stops, and DSR.
- Bell notification indicators; tab system with Ctrl+Shift shortcuts.
- Auto-update tab/window title from the terminal; Docker terminal tabs; hidden Debug submenu.
- Seamless upgrade system for live process updates; multi-platform GitHub Actions builds.

### Changed
- Replace `portable-pty` with a unified native PTY implementation.

[Unreleased]: https://github.com/corbet-labs/cterm/compare/v0.0.20...HEAD
[0.0.20]: https://github.com/corbet-labs/cterm/compare/v0.0.19...v0.0.20
[0.0.19]: https://github.com/corbet-labs/cterm/compare/v0.0.18...v0.0.19
[0.0.18]: https://github.com/corbet-labs/cterm/compare/v0.0.17...v0.0.18
[0.0.17]: https://github.com/corbet-labs/cterm/compare/v0.0.16...v0.0.17
[0.0.16]: https://github.com/corbet-labs/cterm/compare/v0.0.15...v0.0.16
[0.0.15]: https://github.com/corbet-labs/cterm/compare/v0.0.14...v0.0.15
[0.0.14]: https://github.com/corbet-labs/cterm/compare/v0.0.13...v0.0.14
[0.0.13]: https://github.com/corbet-labs/cterm/compare/v0.0.12...v0.0.13
[0.0.12]: https://github.com/corbet-labs/cterm/compare/v0.0.11...v0.0.12
[0.0.11]: https://github.com/corbet-labs/cterm/compare/v0.0.10...v0.0.11
[0.0.10]: https://github.com/corbet-labs/cterm/compare/v0.0.9...v0.0.10
[0.0.9]: https://github.com/corbet-labs/cterm/compare/v0.0.8...v0.0.9
[0.0.8]: https://github.com/corbet-labs/cterm/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/corbet-labs/cterm/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/corbet-labs/cterm/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/corbet-labs/cterm/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/corbet-labs/cterm/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/corbet-labs/cterm/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/corbet-labs/cterm/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/corbet-labs/cterm/releases/tag/v0.0.1
