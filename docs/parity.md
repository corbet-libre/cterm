# Compatibility and parity inventory

This is the living acceptance ledger for cterm's declared compatibility scope.
A feature is not complete merely because its parser or model exists:

- **Implemented** means the behavior exists in every applicable production backend.
- **Partial** means a useful subset exists but protocol behavior or a backend is missing.
- **Missing** means no production implementation exists.
- **Verified** requires automated behavioral evidence at the layer where users observe it.

Kitty and foot are behavioral references. Their non-Rust implementations are
not copied into cterm. Source adaptations must remain bounded, license-compatible,
and recorded in `THIRD_PARTY_LICENSES.md` at an exact upstream revision.

## Kitty protocol extensions

The authoritative catalog is Kitty's current
[terminal protocol extensions](https://sw.kovidgoyal.net/kitty/protocol-extensions/).

| Extension | Implementation | Current evidence | Outstanding acceptance work |
|---|---|---|---|
| Colored and styled underlines | Implemented | `CellAttributes`, SGR parsing, and native Cocoa/GTK/Direct2D underline renderers | Add pixel-level native visual assertions for style, color, and position |
| Graphics protocol | Implemented, conformance partial | Bounded APC protocol, quota-aware image/placement/animation store, 45 focused tests, and native shared-RGBA rendering | Differential protocol suite and semantic native image assertions |
| Keyboard protocol | Implemented | All five progressive flags, independent screen stacks, and native layout/physical-key inputs | Cross-platform differential fixtures for dead keys, IME, alternate keys, and release events |
| Text sizing | Implemented | OSC 66 model, atomic multicell editing/reflow behavior, snapshots, and native rendering | Pixel-level alignment and fractional-scaling assertions |
| Drag and drop | Partial | Complete OSC 72 vocabulary and local destination drops on GTK/Wayland, Cocoa, and Win32/OLE, plus source-side negotiation in shared core (arm/gesture/offer, chunked MIME offers, pre-sent data, drag images, copy/move/either mapping, `t=e` progress events) with fail-closed remote (`t=k`) reads behind exact-token approval, routed on all three frontends; 24 new core behavior tests + GTK provider-seam test | Native OS drag sessions (GTK gesture arbitration vs text selection + DragSource, Cocoa NSDraggingSession/file promises, Win32 OLE DoDragDrop), native consent prompts for remote reads, local-direct file reads (terminal never opens application-supplied paths) |
| Multiple cursors | Implemented | Shared shape/color model, queries, snapshots, and native overlays | Pixel-level native overlay assertions |
| File transfer over the TTY | Partial | Kitty-compatible unpadded codec, lossless bounded daemon actor, exact replayable consent tokens, owned deny-default Cocoa/GTK/Win32 prompts with expiry and lifecycle cancellation, relaunch-safe cleanup, private handle-relative send staging with strict zlib, cumulative limits, metadata, directory/symlink/hardlink reconstruction, missing-parent creation, and per-entry atomic commit, plus recursive consent-listed receive trees with retained directory/file handles, symlink and hardlink correlation, bounded plain/zlib streaming and PTY backpressure; bidirectional rsync uses strict bounded signatures/deltas, rolling matching, XXH3-64 blocks, XXH3-128 output verification, and optional zlib | Native prompt interaction automation, bypass policy, and Kitty differential tests |
| Desktop notifications | Partial | Native delivery and the advertised title/body/focus/close common subset | Activation/close reporting, icons, buttons, sounds, expiry, filtering, and complete capability queries |
| Mouse pointer shapes | Partial | Kitty OSC 22 set/push/pop/query with per-screen stacks (cap 16), daemon snapshot persistence, and GTK cursor mapping; 12 new core/proto behavior tests | Cocoa/Win32 native cursor mappings and pixel-level native assertions |
| Unscrolling the screen | Implemented | Kitty `CSI Ps + T` scrolls down within the scroll region and refills the new top lines from scrollback (blank fallback on the alternate screen or empty scrollback), covered by focused `cterm-core` behavioral tests; ordinary `CSI Ps T` is unchanged and `CSI 22 J` viewport-to-scrollback remains separate | Differential check against Kitty for margin interaction; no native rendering change needed (grid content only) |
| Color control | Partial | Xterm palette stack plus OSC 4/10-12 dynamic colors, with Kitty OSC 21 query/set/reset for selection, cursor-text, visual-bell, and transparency keys, shared OSC 30001/30101 palette-stack push/pop, daemon-owned replies, snapshot persistence, and strict value validation (core behavioral tests) | Native renderer hooks consuming the new slots, named-color values, OSC 17/19 selection aliasing, and pixel-level native visual assertions |
| Miscellaneous extensions | Partial | Independent bold/faint reset, mouse-leave report, viewport-to-scrollback, no-argument save/restore of safe modes (ANSI 4/20 plus DEC 1/5/7/25, the 1000 tracking and 1005 encoding groups, 1004/2004/2031/2033; DECOM/DECCOLM and unimplemented modes excluded), and Kitty private DCS echo/print/restore-cursor-appearance with strict validation and fail-closed remote control; 8 new core behavior tests | Remote-control execution and overlay/kitten-result/ssh/ask/clone/edit handlers (no core counterpart; security-sensitive commands stay fail-closed) |

## foot and terminal-core parity

The audited foot baseline is release `1.27.0` plus current master revision
`765ca4070bb6f095fc58c030f2154ed03857701d`; that exact behavioral-reference
revision is also recorded in `THIRD_PARTY_LICENSES.md`.

| Area | Implementation | Current evidence | Outstanding acceptance work |
|---|---|---|---|
| VT/DEC/xterm core | Substantial | Parser/screen unit tests, real Neovim and tmux sessions, and a byte-for-byte foot probe in CI | Expand the differential corpus across editing, modes, reports, resize/reflow, Unicode, and malformed input |
| Sixel | Implemented | Bounded decoder tests and the shared image store render through Cocoa/CoreGraphics, GTK/Cairo, and Win32/Direct2D | Inject known Sixel frames and compare semantic pixels on every native backend |
| Dynamic colors and palette stack | Implemented | Daemon-owned replies and native palette state tests | Broaden differential color encodings and renderer assertions |
| Rectangular editing | Partial relative to Kitty, implemented for the DEC/foot subset plus the Kitty all-SGR DECCARA superset | Core behavioral tests cover erase, fill, copy, and full-SGR attribute changes | DECSACE stream extent and DECRARA color-reversal semantics |
| Shell and TUI behavior | Substantial | Hard real-session Neovim/tmux CI and OSC 133 integration | Add more shells, multiplexers, TUIs, keyboard modes, and long-running fuzz/differential workloads |
| Performance | Measured, not gated | Criterion parser/render/reflow/scrollback reports run on relevant changes and weekly | Establish stable hardware-normalized regression thresholds before making performance a hard gate |

## Platform and delivery contract

| Target | Current hard evidence | Open work |
|---|---|---|
| macOS Intel/Apple Silicon | Native builds, library/integration tests, Cocoa UI automation, universal packages | Broader protocol-specific semantic rendering and signed/notarized release evidence |
| Windows x64 | Native builds, library/integration tests, Win32 UI automation, ZIP and NSIS packages | Broader protocol-specific semantic rendering and ARM64 evaluation |
| Linux x86_64/ARM64 | Wayland-only builds, tests, compositor-backed GTK UI smoke, release archives | Distribution-native packages and broader semantic rendering |
| FreeBSD 14.4 | Native library/daemon tests and compositor-backed GTK/Wayland UI smoke | Reproducible release package |
| Software/headless | Headless daemon and core behavioral suites | Keep protocol parity independent of a GPU or window system |
| Android/iOS | Missing, distant target | Keyboard-driven local terminal frontends after desktop parity |

The broader network stack remains intentionally behind local-terminal parity.
It stays in scope, but it is not allowed to delay completion of the rows above.
