//! Kitty keyboard-protocol differential fixtures — compositor-free half.
//!
//! Scope: the shared Rust encoder that GTK/Cocoa/Win32 feed
//! (`cterm_core::term::Terminal::handle_key_event_with_metadata`,
//! `handle_text_input`, `handle_reported_key_release_with_metadata`) plus the
//! progressive-flag stack semantics (`Screen` per-screen stacks, parser
//! `CSI ?/>/</= u`). This runs with `cargo test -p cterm-core` and needs no
//! compositor, macOS, or Windows host.
//!
//! Still open (native passes, NOT covered here): replaying these vectors
//! against real dead-key/IME/alternate-key/release OS input on each platform —
//! GTK needs a compositor for GDK/IBus, Cocoa needs macOS for NSEvent,
//! Win32 needs Windows for ToUnicode/TSF. See `docs/parity.md` keyboard row.
//!
//! # Schema
//!
//! Every vector is `(setup, input, expected)` where:
//!
//! - `setup` is literal PTY bytes (usually `CSI = n u` to select a flag set,
//!   `CSI ? 1049 h/l` to switch screens) applied via `process_collecting`.
//!   Flag bits: 1 disambiguate, 2 event-types, 4 alternate-keys, 8 all-keys,
//!   16 associated-text.
//! - `input` is one encoder call: a `Key` + `Modifiers` + `KeyEventKind` (+ an
//!   optional `Meta` triple), a `handle_text_input` commit string, or a
//!   `handle_reported_key_release` drain call.
//! - `expected` is the exact byte sequence (`None` = encoder emits nothing).
//!
//! # Replay recipe for macOS/Windows contributors
//!
//! 1. Build the setup: `let mut term = Terminal::new(80, 24,
//!    ScreenConfig::default()); term.process_collecting(setup);`
//! 2. Derive the native input with the platform adapter, then call the same
//!    encoder entry point and compare bytes (per-platform derivation):
//!
//! - Cocoa: `terminal_key_for_keycode` / `characters_from_event` + `handle_key_event_with_metadata`; `insertText:` commits go through `handle_text_input`.
//! - GTK: `reported_key_from_gdk` + `associated_text_for_gdk_key` + `handle_key_event_with_metadata`; IM commits go through `handle_text_input`.
//! - Win32: `mapped_terminal_key_with_layout` + `translated_key_char` / `associated_key_text` + `handle_key_event_with_metadata`; `WM_CHAR` goes through `handle_text_input`.
//! 3. A mismatch against a vector below is either a frontend derivation bug
//!    (wrong Key/Modifiers/Meta) or — if the derivation matches the table —
//!    a shared-encoder bug. Fix whichever side disagrees with the Kitty
//!    specification (sw.kovidgoyal.net/kitty/protocol-extensions).
//!
//! # Frontend derivation contract (what each platform must supply)
//!
//! - `shifted_key`: the Shift-shifted char at the position, sent only while
//!   Shift is held (the encoder drops it otherwise). GTK derives it with
//!   `translate_key(..., SHIFT_MASK)`; Cocoa uses the event text; Win32 uses
//!   `translated_key_char(vk, scan, true)`.
//! - `base_layout_key`: the US-position identity of the physical key, used for
//!   the alternate `::base` field. This is a *position label*, not the typed
//!   char. Derivations differ by platform (MAC-layout vs PC-layout):
//!   Cocoa feeds `KeyCode::to_char` (fixed US-ANSI position names from the
//!   macOS virtual keycode, `cocoa/keycode.rs::keycode_from_event`); GTK feeds
//!   the XKB-evdev-to-PC-101 map (`gtk/keyboard.rs::pc101_key_for_gdk_keycode`);
//!   Win32 feeds the scan-code-set-1-to-PC-101 map
//!   (`win32/keycode.rs::pc101_key_for_scan_code`). All three agree on US
//!   layouts; all three return `None` outside their tables, and `None` must be
//!   forwarded as-is — inventing a US identity for an unknown position is a
//!   bug (`keyboard.rs::KeyEventMetadata` docs).
//! - `associated_text`: post-layout/IME text for this event. GTK sends `None`
//!   while Ctrl/Super/Hyper/Meta is held; Cocoa sends the event text; Win32
//!   sends live-state `associated_key_text`. The encoder emits it only for
//!   non-release events while all-keys + associated-text are both set, with
//!   C0/C1 controls stripped.
//! - Modifiers share one bit layout (`Modifiers`); lock-bit sources differ:
//!   Win32 wires CapsLock + NumLock toggle state (`get_modifiers`), GTK wires
//!   CapsLock via `LOCK_MASK`, Cocoa wires CapsLock via NSEvent flags. NumLock
//!   (+128) has no GTK/Cocoa source by platform limitation (see Q3): GDK4
//!   exposes no Mod2 mask, NSEvent no toggle API — no approximate bit is
//!   emitted instead.
//!
//! # Seams with no encoder traffic (pinned by contract, not bytes)
//!
//! - IME preedit: the encoder has no preedit entry point. Frontends must never
//!   forward composition-in-progress (GTK renders its `PreeditState` overlay
//!   only; Cocoa keeps `marked_text` display-only; Win32 keeps TSF composition
//!   out of `on_key_event`). Only the final commit crosses
//!   `handle_text_input`.
//! - Dead keys: the dead press itself sends nothing (GTK's IM filter consumes
//!   it; Cocoa keeps dead keys on `interpretKeyEvents`; Win32 stays on
//!   `WM_CHAR` for the composed result). The commit crosses
//!   `handle_text_input`; a cancelled composition sends nothing at all.
//! - Focus: `CSI I` / `CSI O` bytes are emitted by each frontend's
//!   `send_focus_event` when DEC 1004 is set — core owns no focus API and
//!   tracks no held keys. Focus-out draining is the frontend replaying
//!   `handle_reported_key_release_with_metadata` once per key it reported,
//!   using release-time flags/modifiers; without event-types there is nothing
//!   to drain (GTK clears its pressed map silently for the same reason).
//!
//! # Remaining open questions (need a native pass — do NOT "fix" blindly)
//!
//! - Q1 (verified live against Kitty 0.48.2 under Xvfb, same bytes):
//!   Enter/Tab/Backspace stay legacy (`0d`/`09`/`7f`) under event-types
//!   alone (flag 2) — Kitty replies `CSI ? 2 u` to the mode query, emits no
//!   release sequences for these keys, and Escape/cursors/tilde/function
//!   keys are promoted. Pinned behavior confirmed, not changed.
//! - Q2 (decided: strip — constructor audit of all three frontends proves
//!   shifted/base metadata only ever accompanies `Key::Char`; live Kitty
//!   0.48.2 confirms the wire side too (flags=13: F5/Shift+F5/Menu report
//!   `15~`, `57441;2u`, `15;2~`, `57363u`, none with alternate fields).
//!   The encoder now drops alternates for functional keys instead of
//!   passing them through).
//! - Q3 (resolved: platform limitation, not a wiring gap — only Win32 wires
//!   the source bit via `GetKeyState(VK_NUMLOCK)`. GTK has no source: GDK4
//!   `ModifierType` exposes no Mod2 mask on Wayland, and local toggle
//!   tracking would desync against the seat-global lock, so no approximate
//!   bit is emitted. Cocoa has no public NumLock-toggle API either. The
//!   wire format itself is confirmed live against Kitty 0.48.2 (Xvfb):
//!   NumLock key `57360`, modifier +128, keypad digits with associated
//!   text; the lock is a toggle, matching the level-read model).

use cterm_core::screen::ScreenConfig;
use cterm_core::term::{Key, Modifiers, NamedKey, Terminal};
use cterm_core::{KeyEventKind, KeyEventMetadata};

// ---------------------------------------------------------------------------
// Table shapes
// ---------------------------------------------------------------------------

/// Optional native layout/text data attached to a physical key event.
/// `shifted` = Shift-shifted char at the position (only meaningful with Shift
/// held); `base` = US-position identity (MAC ANSI vs PC-101 derivations, see
/// header); `text` = post-layout/IME text for this event.
#[derive(Clone, Copy)]
struct Meta {
    shifted: Option<char>,
    base: Option<char>,
    text: Option<&'static str>,
}

impl Meta {
    const fn build(self) -> KeyEventMetadata<'static> {
        KeyEventMetadata::new()
            .with_shifted_key(self.shifted)
            .with_base_layout_key(self.base)
            .with_associated_text(self.text)
    }
}

const NO_META: Meta = Meta {
    shifted: None,
    base: None,
    text: None,
};

/// One physical key event: literal PTY setup bytes, then a single
/// `handle_key_event_with_metadata` call and its exact expected bytes
/// (`None` = the encoder must emit nothing).
struct KeyVector {
    name: &'static str,
    setup: &'static str,
    key: Key,
    mods: Modifiers,
    kind: KeyEventKind,
    meta: Meta,
    expected: Option<&'static str>,
}

/// One commit across the text seam (`handle_text_input`): the only path for
/// dead-key completions, IME commits, pasted-while-enhanced chars, and
/// platform text input while all-keys mode is active.
struct TextVector {
    name: &'static str,
    setup: &'static str,
    input: &'static str,
    expected: &'static str,
}

fn term_with_setup(setup: &str) -> Terminal {
    let mut term = Terminal::new(80, 24, ScreenConfig::default());
    if !setup.is_empty() {
        term.process_collecting(setup.as_bytes());
    }
    term
}

fn check_key_vector(row: &KeyVector) {
    let term = term_with_setup(row.setup);
    let got = term.handle_key_event_with_metadata(row.key, row.mods, row.kind, row.meta.build());
    assert_eq!(
        got.as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        row.expected.map(str::to_owned),
        "key vector failed: {}",
        row.name,
    );
}

fn check_text_vector(row: &TextVector) {
    let term = term_with_setup(row.setup);
    let got = term.handle_text_input(row.input);
    assert_eq!(
        String::from_utf8_lossy(&got),
        row.expected,
        "text vector failed: {}",
        row.name,
    );
}

fn query_flags(term: &mut Terminal) -> Vec<u8> {
    let (_, responses) = term.process_collecting(b"\x1b[?u");
    assert_eq!(responses.len(), 1, "keyboard query must yield one reply");
    responses.into_iter().next().unwrap()
}

// ---------------------------------------------------------------------------
// 1. Full 5-flag matrix for one modified key.
//
// Fixed input: Char('c') + Ctrl, press/repeat/release, metadata text "c" so
// the associated-text gate (needs all-keys AND text flag AND non-release AND
// surviving codepoints) is exercised. Derivation rule pinned here:
// - legacy group (no disambiguate bit, no all-keys bit): Ctrl+C is a legacy
//   control char (0x03); releases are always None.
// - CSI-u group: `ESC[99;5` + `:event` iff event-types + `;99` (the text "c")
//   iff all-keys + text flag on a non-release event.
// ---------------------------------------------------------------------------

/// One full flag-combination row: flag bits plus the expected press, repeat,
/// and release encodings for the fixed Ctrl+C probe input (`None` = silent).
type FlagRow = (
    u8,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

#[test]
fn flag_matrix_ctrl_c_press_repeat_release() {
    const MATRIX: [FlagRow; 32] = [
        (0, Some("\x03"), Some("\x03"), None),
        (1, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (2, Some("\x03"), Some("\x03"), None),
        (
            3,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (4, Some("\x03"), Some("\x03"), None),
        (5, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (6, Some("\x03"), Some("\x03"), None),
        (
            7,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (8, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (9, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (
            10,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (
            11,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (12, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (13, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (
            14,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (
            15,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (16, Some("\x03"), Some("\x03"), None),
        (17, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (18, Some("\x03"), Some("\x03"), None),
        (
            19,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (20, Some("\x03"), Some("\x03"), None),
        (21, Some("\x1b[99;5u"), Some("\x1b[99;5u"), None),
        (22, Some("\x03"), Some("\x03"), None),
        (
            23,
            Some("\x1b[99;5:1u"),
            Some("\x1b[99;5:2u"),
            Some("\x1b[99;5:3u"),
        ),
        (24, Some("\x1b[99;5;99u"), Some("\x1b[99;5;99u"), None),
        (25, Some("\x1b[99;5;99u"), Some("\x1b[99;5;99u"), None),
        (
            26,
            Some("\x1b[99;5:1;99u"),
            Some("\x1b[99;5:2;99u"),
            Some("\x1b[99;5:3u"),
        ),
        (
            27,
            Some("\x1b[99;5:1;99u"),
            Some("\x1b[99;5:2;99u"),
            Some("\x1b[99;5:3u"),
        ),
        (28, Some("\x1b[99;5;99u"), Some("\x1b[99;5;99u"), None),
        (29, Some("\x1b[99;5;99u"), Some("\x1b[99;5;99u"), None),
        (
            30,
            Some("\x1b[99;5:1;99u"),
            Some("\x1b[99;5:2;99u"),
            Some("\x1b[99;5:3u"),
        ),
        (
            31,
            Some("\x1b[99;5:1;99u"),
            Some("\x1b[99;5:2;99u"),
            Some("\x1b[99;5:3u"),
        ),
    ];

    let meta = Meta {
        shifted: None,
        base: None,
        text: Some("c"),
    };
    for (bits, press, repeat, release) in MATRIX {
        let setup = format!("\x1b[={bits}u");
        let mut term = term_with_setup(&setup);
        // The setup itself must be observable through the query reply.
        assert_eq!(
            query_flags(&mut term),
            format!("\x1b[?{bits}u").into_bytes(),
            "flag setup {bits} not observable via query",
        );
        for (kind, expected) in [
            (KeyEventKind::Press, press),
            (KeyEventKind::Repeat, repeat),
            (KeyEventKind::Release, release),
        ] {
            let got = term.handle_key_event_with_metadata(
                Key::Char('c'),
                Modifiers::CTRL,
                kind,
                meta.build(),
            );
            assert_eq!(
                got.as_deref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                expected.map(str::to_owned),
                "flags={bits} kind={kind:?}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Press/repeat/release gating: releases need event-types, even in
// all-keys mode ("release-only-in-all-keys" is NOT a thing — all-keys alone
// reports press/repeat and suppresses release).
// ---------------------------------------------------------------------------

#[test]
fn release_events_need_event_types_even_in_all_keys_mode() {
    const ROWS: &[KeyVector] = &[
        KeyVector {
            name: "all-keys press without event-types",
            setup: "\x1b[=8u",
            key: Key::Char('a'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[97;1u"),
        },
        KeyVector {
            name: "all-keys repeat without event-types matches press",
            setup: "\x1b[=8u",
            key: Key::Char('a'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Repeat,
            meta: NO_META,
            expected: Some("\x1b[97;1u"),
        },
        KeyVector {
            name: "all-keys release without event-types suppressed",
            setup: "\x1b[=8u",
            key: Key::Char('a'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Release,
            meta: NO_META,
            expected: None,
        },
        KeyVector {
            name: "all-keys release with event-types",
            setup: "\x1b[=10u",
            key: Key::Char('a'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Release,
            meta: NO_META,
            expected: Some("\x1b[97;1:3u"),
        },
        KeyVector {
            name: "event-types cursor release",
            setup: "\x1b[=2u",
            key: Key::Up,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Release,
            meta: NO_META,
            expected: Some("\x1b[1;1:3A"),
        },
        // Q1 (live-verified, see header): Enter/Tab/Backspace stay legacy
        // under event-types alone.
        KeyVector {
            name: "event-types enter press stays legacy",
            setup: "\x1b[=2u",
            key: Key::Enter,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\r"),
        },
        KeyVector {
            name: "event-types enter release suppressed",
            setup: "\x1b[=2u",
            key: Key::Enter,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Release,
            meta: NO_META,
            expected: None,
        },
        KeyVector {
            name: "event-types tab press stays legacy",
            setup: "\x1b[=2u",
            key: Key::Tab,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\t"),
        },
        KeyVector {
            name: "event-types backspace press stays legacy",
            setup: "\x1b[=2u",
            key: Key::Backspace,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x7f"),
        },
        KeyVector {
            name: "disambiguate enter press stays legacy",
            setup: "\x1b[=1u",
            key: Key::Enter,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\r"),
        },
        KeyVector {
            name: "all-keys enter press promoted",
            setup: "\x1b[=8u",
            key: Key::Enter,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[13;1u"),
        },
        KeyVector {
            name: "all-keys tab press promoted",
            setup: "\x1b[=8u",
            key: Key::Tab,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[9;1u"),
        },
        KeyVector {
            name: "all-keys backspace press promoted",
            setup: "\x1b[=8u",
            key: Key::Backspace,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[127;1u"),
        },
        KeyVector {
            name: "modifier press suppressed without all-keys",
            setup: "\x1b[=2u",
            key: Key::Named(NamedKey::LeftShift),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: None,
        },
        KeyVector {
            name: "modifier press reported in all-keys",
            setup: "\x1b[=8u",
            key: Key::Named(NamedKey::LeftShift),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[57441;2u"),
        },
        KeyVector {
            name: "legacy modifier press suppressed",
            setup: "",
            key: Key::Named(NamedKey::LeftShift),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: None,
        },
        KeyVector {
            name: "all-keys F5 press without event-types stays tilde",
            setup: "\x1b[=8u",
            key: Key::F(5),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[15~"),
        },
        KeyVector {
            name: "all-keys F5 press with event-types",
            setup: "\x1b[=10u",
            key: Key::F(5),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[15;1:1~"),
        },
        KeyVector {
            name: "all-keys numpad press",
            setup: "\x1b[=8u",
            key: Key::NumpadDigit(0),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[57399;1u"),
        },
        KeyVector {
            name: "legacy numpad press",
            setup: "",
            key: Key::NumpadDigit(0),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("0"),
        },
        KeyVector {
            name: "escape promoted by event-types alone",
            setup: "\x1b[=2u",
            key: Key::Escape,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[27;1:1u"),
        },
        KeyVector {
            name: "escape release with event-types",
            setup: "\x1b[=2u",
            key: Key::Escape,
            mods: Modifiers::empty(),
            kind: KeyEventKind::Release,
            meta: NO_META,
            expected: Some("\x1b[27;1:3u"),
        },
    ];
    for row in ROWS {
        check_key_vector(row);
    }
}

// ---------------------------------------------------------------------------
// 3. Alternate keys for a representative set, incl. MAC-vs-PC base identity.
//
// `61` is '='. Rows pin the `key:shifted:base` field spelling, the rule that
// `shifted` is dropped unless Shift is held, and the rule that an unknown
// position must send no base at all rather than an invented US identity.
// The AZERTY pair shows the same typed key with a PC-101 base ('q') as GTK
// and Win32 derive it; Cocoa derives the same label from its ANSI position
// table. A macOS/Windows contributor replays these rows by feeding their
// native derivation into `base` — the encoder treats both spellings
// identically, which is exactly what these rows prove.
// ---------------------------------------------------------------------------

#[test]
fn alternate_keys_representative_set() {
    const ROWS: &[KeyVector] = &[
        KeyVector {
            name: "shifted and pc base",
            setup: "\x1b[=7u",
            key: Key::Char('='),
            mods: Modifiers::SHIFT.union(Modifiers::CTRL),
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('+'),
                base: Some('='),
                text: None,
            },
            expected: Some("\x1b[61:43:61;6:1u"),
        },
        KeyVector {
            name: "shifted only",
            setup: "\x1b[=7u",
            key: Key::Char('='),
            mods: Modifiers::SHIFT.union(Modifiers::CTRL),
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('+'),
                base: None,
                text: None,
            },
            expected: Some("\x1b[61:43;6:1u"),
        },
        KeyVector {
            name: "base only with shift held",
            setup: "\x1b[=7u",
            key: Key::Char('='),
            mods: Modifiers::SHIFT.union(Modifiers::CTRL),
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: None,
                base: Some('='),
                text: None,
            },
            expected: Some("\x1b[61::61;6:1u"),
        },
        KeyVector {
            name: "neither alternate present",
            setup: "\x1b[=7u",
            key: Key::Char('='),
            mods: Modifiers::SHIFT.union(Modifiers::CTRL),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[61;6:1u"),
        },
        KeyVector {
            name: "shifted dropped without shift modifier",
            setup: "\x1b[=7u",
            key: Key::Char('='),
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('+'),
                base: Some('='),
                text: None,
            },
            expected: Some("\x1b[61::61;5:1u"),
        },
        KeyVector {
            name: "azerty pc-base with shift held",
            setup: "\x1b[=31u",
            key: Key::Char('a'),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('A'),
                base: Some('q'),
                text: Some("A"),
            },
            expected: Some("\x1b[97:65:113;2:1;65u"),
        },
        KeyVector {
            name: "azerty pc-base without shift",
            setup: "\x1b[=31u",
            key: Key::Char('a'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('A'),
                base: Some('q'),
                text: Some("a"),
            },
            expected: Some("\x1b[97::113;1:1;97u"),
        },
        // Shift alone never promotes a printable to CSI-u (disambiguate only
        // covers Ctrl/Alt/Super/Hyper/Meta-modified chars): the encoder passes
        // the key's char through legacy-style. Frontends must therefore route
        // shift-only printables through the OS text path (GDK keyval unicode,
        // Cocoa interpretKeyEvents, Win32 WM_CHAR) — never as unshifted
        // `Key::Char` here, or the Shift case would be lost.
        KeyVector {
            name: "shift-only printable stays legacy in disambiguate mode",
            setup: "\x1b[=7u",
            key: Key::Char('a'),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: Some('A'),
                base: Some('q'),
                text: None,
            },
            expected: Some("a"),
        },
        KeyVector {
            name: "unmapped position sends no base",
            setup: "\x1b[=15u",
            key: Key::Char('§'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[167;1:1u"),
        },
        // Q2 (decided: strip — audit of GTK/Cocoa/Win32 proves no frontend
        // populates shifted/base metadata for functional keys by
        // construction, so the passthrough arm could never fire).
        KeyVector {
            name: "functional key alternate stripped",
            setup: "\x1b[=13u",
            key: Key::Named(NamedKey::Menu),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: None,
                base: Some('m'),
                text: None,
            },
            expected: Some("\x1b[57363;1u"),
        },
    ];
    for row in ROWS {
        check_key_vector(row);
    }
}

// ---------------------------------------------------------------------------
// 4. Modifier encoding. `Modifiers` bits are shared by all frontends; rows
// pin the Kitty `mods+1` parameter (shift+1, alt+2, ctrl+4, super+8,
// hyper+16, meta+32, caps-lock+64, num-lock+128). Native replay notes:
// Command->Super and Option->Alt on Cocoa, Win->Super on Win32, the GDK masks
// on GTK. Q3 resolved: num-lock rows prove the encoder half; only Win32 wires
// the source bit (GTK/Cocoa lack an OS source — documented limitation).
// ---------------------------------------------------------------------------

#[test]
fn modifier_encoding_shift_alt_ctrl_super_locks() {
    const ROWS: &[KeyVector] = &[
        KeyVector {
            name: "no modifiers",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::empty(),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;1u"),
        },
        KeyVector {
            name: "shift",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;2u"),
        },
        KeyVector {
            name: "alt",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::ALT,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;3u"),
        },
        KeyVector {
            name: "shift+alt",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::SHIFT.union(Modifiers::ALT),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;4u"),
        },
        KeyVector {
            name: "ctrl",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;5u"),
        },
        KeyVector {
            name: "shift+ctrl",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::SHIFT.union(Modifiers::CTRL),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;6u"),
        },
        KeyVector {
            name: "ctrl+alt",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::CTRL.union(Modifiers::ALT),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;7u"),
        },
        KeyVector {
            name: "super",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::SUPER,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;9u"),
        },
        KeyVector {
            name: "all six main modifiers",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::SHIFT
                .union(Modifiers::CTRL)
                .union(Modifiers::ALT)
                .union(Modifiers::SUPER)
                .union(Modifiers::HYPER)
                .union(Modifiers::META),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;64u"),
        },
        KeyVector {
            name: "caps-lock",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::CAPS_LOCK,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;65u"),
        },
        KeyVector {
            name: "num-lock",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::NUM_LOCK,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;129u"),
        },
        KeyVector {
            name: "caps-lock+num-lock",
            setup: "\x1b[=8u",
            key: Key::Char('z'),
            mods: Modifiers::CAPS_LOCK.union(Modifiers::NUM_LOCK),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[122;193u"),
        },
        KeyVector {
            name: "ctrl cursor under disambiguate",
            setup: "\x1b[=1u",
            key: Key::Up,
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[1;5A"),
        },
        KeyVector {
            name: "ctrl+alt cursor under disambiguate",
            setup: "\x1b[=1u",
            key: Key::Up,
            mods: Modifiers::CTRL.union(Modifiers::ALT),
            kind: KeyEventKind::Press,
            meta: NO_META,
            expected: Some("\x1b[1;7A"),
        },
    ];
    for row in ROWS {
        check_key_vector(row);
    }
}

// ---------------------------------------------------------------------------
// 5. Dead-key sequences at the encoder seam. The dead press itself never
// reaches the encoder (frontend consumes it); what the encoder must do is
// (a) emit no text field for a text-less press under all-keys+text, (b) route
// the later commit through `handle_text_input`, (c) emit nothing on cancel.
// US-International acute (U+00B4) + 'e' -> 'é' is the concrete scenario.
// ---------------------------------------------------------------------------

#[test]
fn dead_key_compose_and_cancel_paths() {
    // The dead press carries no text, so even with all five flags the CSI-u
    // event must have no `;text` section.
    check_key_vector(&KeyVector {
        name: "dead press carries no associated text",
        setup: "\x1b[=31u",
        key: Key::Char('\u{b4}'),
        mods: Modifiers::empty(),
        kind: KeyEventKind::Press,
        meta: NO_META,
        expected: Some("\x1b[180;1:1u"),
    });

    const ROWS: &[TextVector] = &[
        TextVector {
            name: "dead-key commit composes in legacy mode",
            setup: "",
            input: "é",
            expected: "é",
        },
        TextVector {
            name: "dead-key commit in all-keys+text uses key zero",
            setup: "\x1b[=24u",
            input: "é",
            expected: "\x1b[0;;233u",
        },
        TextVector {
            name: "dead-key commit suppressed in all-keys without text",
            setup: "\x1b[=8u",
            input: "é",
            expected: "",
        },
        TextVector {
            name: "dead+space commits the spacing mark",
            setup: "\x1b[=24u",
            input: "´",
            expected: "\x1b[0;;180u",
        },
        // Cancel path: the frontend sends nothing; even an empty flush must
        // produce no bytes in either mode.
        TextVector {
            name: "cancelled composition flushes nothing in legacy mode",
            setup: "",
            input: "",
            expected: "",
        },
        TextVector {
            name: "cancelled composition flushes nothing in all-keys+text",
            setup: "\x1b[=31u",
            input: "",
            expected: "",
        },
    ];
    for row in ROWS {
        check_text_vector(row);
    }
}

// ---------------------------------------------------------------------------
// 6. IME commits at the encoder seam. Preedit strings must never reach the
// encoder (contract, see header); commits arrive whole — possibly multi-char
// and non-BMP — via `handle_text_input`. Rows pin raw passthrough in legacy
// mode, key-zero wrapping in all-keys+text, suppression in all-keys without
// text, and control stripping.
// ---------------------------------------------------------------------------

#[test]
fn ime_commit_handling_at_encoder_seam() {
    const ROWS: &[TextVector] = &[
        TextVector {
            name: "cjk commit passes through raw in legacy mode",
            setup: "",
            input: "にほん",
            expected: "にほん",
        },
        TextVector {
            name: "cjk commit wraps as key zero in all-keys+text",
            setup: "\x1b[=24u",
            input: "にほん",
            expected: "\x1b[0;;12395:12411:12435u",
        },
        TextVector {
            name: "cjk commit suppressed in all-keys without text",
            setup: "\x1b[=8u",
            input: "にほん",
            expected: "",
        },
        TextVector {
            name: "commit strips embedded controls",
            setup: "\x1b[=24u",
            input: "a\u{7}b",
            expected: "\x1b[0;;97:98u",
        },
        TextVector {
            name: "controls-only commit emits nothing",
            setup: "\x1b[=24u",
            input: "\u{1b}",
            expected: "",
        },
        TextVector {
            name: "non-bmp commit passes through raw in legacy mode",
            setup: "",
            input: "😀",
            expected: "😀",
        },
        TextVector {
            name: "non-bmp commit wraps as key zero in all-keys+text",
            setup: "\x1b[=24u",
            input: "😀",
            expected: "\x1b[0;;128512u",
        },
    ];
    for row in ROWS {
        check_text_vector(row);
    }

    // The encoder is stateless across the text seam: a commit must not alter
    // the encoding of the next physical event (i.e. there is no lingering
    // "composition" state to leak into key reports).
    let term = term_with_setup("\x1b[=2u");
    assert_eq!(term.handle_text_input("に"), "に".as_bytes());
    assert_eq!(
        term.handle_key_event(Key::Up, Modifiers::empty(), KeyEventKind::Press),
        Some(b"\x1b[1;1:1A".to_vec()),
    );
}

// ---------------------------------------------------------------------------
// 7. Focus-in/out vs release-event draining. DEC 1004 arms the mode bit only;
// `CSI I`/`CSI O` are frontend-owned bytes (each platform's
// `send_focus_event`). The encoder side pins: (a) draining replays the
// reported-release call per tracked key, (b) without event-types there is
// nothing to drain, (c) release encoding uses release-time flags — an app
// that resets flags between press and focus-out gets no release, matching
// all three frontends which read flags at release time.
// ---------------------------------------------------------------------------

#[test]
fn focus_out_drain_uses_reported_release_semantics() {
    let mut term = term_with_setup("\x1b[?1004h\x1b[=2u");
    assert!(
        term.screen().modes.focus_events,
        "DEC 1004 must arm focus-event mode"
    );

    // Press while focused …
    assert_eq!(
        term.handle_key_event(Key::Up, Modifiers::empty(), KeyEventKind::Press),
        Some(b"\x1b[1;1:1A".to_vec()),
    );
    // … focus-out drains it through the reported-release path …
    assert_eq!(
        term.handle_reported_key_release(Key::Up, Modifiers::empty()),
        Some(b"\x1b[1;1:3A".to_vec()),
    );
    // … and neither step queues a core response: focus bytes belong to the
    // frontend `send_focus_event` path, not to the encoder.
    let (_, responses) = term.process_collecting(b"");
    assert!(responses.is_empty(), "encoder must not emit focus bytes");

    // Without event-types there is nothing to drain: the frontend must stay
    // silent (GTK clears its pressed map for the same reason).
    let term = term_with_setup("\x1b[?1004h");
    assert_eq!(
        term.handle_reported_key_release(Key::Up, Modifiers::empty()),
        None,
    );
    assert_eq!(
        term.handle_reported_key_release_with_metadata(
            Key::Char('x'),
            Modifiers::CTRL,
            NO_META.build(),
        ),
        None,
    );
}

#[test]
fn release_encoding_uses_release_time_flags() {
    let mut term = term_with_setup("\x1b[=3u");
    assert_eq!(
        term.handle_key_event(Key::Char('c'), Modifiers::CTRL, KeyEventKind::Press),
        Some(b"\x1b[99;5:1u".to_vec()),
    );
    // The application resets flags before focus-out: the release can no
    // longer be expressed, so the encoder emits nothing rather than a stale
    // or legacy release.
    term.process_collecting(b"\x1b[=0u");
    assert_eq!(
        term.handle_reported_key_release(Key::Char('c'), Modifiers::CTRL),
        None,
    );
}

// ---------------------------------------------------------------------------
// 8. Flag-stack push/pop imbalance and alternate-screen independence.
// ---------------------------------------------------------------------------

#[test]
fn flag_stack_pop_imbalance_fails_closed_to_legacy() {
    let mut term = term_with_setup("");
    // Popping an empty stack is a no-op, not a panic or wrap-around.
    term.process_collecting(b"\x1b[<5u");
    assert_eq!(query_flags(&mut term), b"\x1b[?0u".to_vec());

    // Bare `< u` pops one level (default count 1).
    term.process_collecting(b"\x1b[>3u");
    assert_eq!(query_flags(&mut term), b"\x1b[?3u".to_vec());
    term.process_collecting(b"\x1b[<u");
    assert_eq!(query_flags(&mut term), b"\x1b[?0u".to_vec());

    // Over-popping clamps to empty instead of underflowing.
    term.process_collecting(b"\x1b[>31u\x1b[<999u");
    assert_eq!(query_flags(&mut term), b"\x1b[?0u".to_vec());
}

#[test]
fn flag_stack_push_overflow_evicts_oldest() {
    let mut term = term_with_setup("");
    term.process_collecting(b"\x1b[>1u");
    for _ in 0..16 {
        term.process_collecting(b"\x1b[>2u");
    }
    // 17 pushes against a depth-16 stack: the bottom `1` was evicted.
    assert_eq!(query_flags(&mut term), b"\x1b[?2u".to_vec());
    term.process_collecting(b"\x1b[<16u");
    assert_eq!(query_flags(&mut term), b"\x1b[?0u".to_vec());
}

#[test]
fn flag_stack_set_or_clear_and_mask_unsupported_bits() {
    let mut term = term_with_setup("");
    term.process_collecting(b"\x1b[=31u");
    assert_eq!(query_flags(&mut term), b"\x1b[?31u".to_vec());
    term.process_collecting(b"\x1b[=2:3u");
    assert_eq!(query_flags(&mut term), b"\x1b[?29u".to_vec());

    term.process_collecting(b"\x1b[=8u");
    term.process_collecting(b"\x1b[=3:2u");
    assert_eq!(query_flags(&mut term), b"\x1b[?11u".to_vec());
    term.process_collecting(b"\x1b[=8:3u");
    assert_eq!(query_flags(&mut term), b"\x1b[?3u".to_vec());

    // Bits outside the five protocol flags are masked on both spellings.
    term.process_collecting(b"\x1b[=255u");
    assert_eq!(query_flags(&mut term), b"\x1b[?31u".to_vec());
    let mut term = term_with_setup("");
    term.process_collecting(b"\x1b[>255u");
    assert_eq!(query_flags(&mut term), b"\x1b[?31u".to_vec());
}

#[test]
fn flag_stacks_are_independent_per_screen() {
    let mut term = term_with_setup("\x1b[=11u");
    assert_eq!(query_flags(&mut term), b"\x1b[?11u".to_vec());

    // Switching to the alternate screen starts from its own empty stack …
    term.process_collecting(b"\x1b[?1049h");
    assert_eq!(query_flags(&mut term), b"\x1b[?0u".to_vec());

    // … which can be pushed/popped without touching the main stack …
    term.process_collecting(b"\x1b[=1u\x1b[>7u");
    assert_eq!(query_flags(&mut term), b"\x1b[?7u".to_vec());
    term.process_collecting(b"\x1b[?1049l");
    assert_eq!(query_flags(&mut term), b"\x1b[?11u".to_vec());

    // … and a main-screen push in between leaves the alternate stack alone.
    term.process_collecting(b"\x1b[>4u");
    assert_eq!(query_flags(&mut term), b"\x1b[?4u".to_vec());
    term.process_collecting(b"\x1b[?1049h");
    assert_eq!(query_flags(&mut term), b"\x1b[?7u".to_vec());
    term.process_collecting(b"\x1b[<u");
    assert_eq!(query_flags(&mut term), b"\x1b[?1u".to_vec());
    term.process_collecting(b"\x1b[?1049l");
    assert_eq!(query_flags(&mut term), b"\x1b[?4u".to_vec());
    term.process_collecting(b"\x1b[<u");
    assert_eq!(query_flags(&mut term), b"\x1b[?11u".to_vec());
}

// ---------------------------------------------------------------------------
// 9. Hostile associated text: the encoder takes `&str`, so non-UTF8 can only
// arrive if a frontend skips validation — rows pin both the rejection
// requirement and the sanitized (lossy) path, plus control stripping in
// key-event metadata and the rule that releases never carry text.
// ---------------------------------------------------------------------------

#[test]
fn associated_text_rejects_or_sanitizes_non_utf8() {
    // Overlong UTF-8 is invalid and must be rejected at the frontend seam —
    // it can never be a `&str`, so it can never reach the encoder as-is.
    // (Built via `Vec` so the rejection itself stays a runtime check.)
    let overlong: Vec<u8> = vec![0xC0, 0xAF];
    assert!(str::from_utf8(&overlong).is_err());
    // The sanitized path: each invalid byte becomes one U+FFFD, and those
    // replacement chars encode as normal codepoints.
    let term = term_with_setup("\x1b[=24u");
    assert_eq!(
        term.handle_text_input(&String::from_utf8_lossy(b"\xC0\xAF")),
        b"\x1b[0;;65533:65533u".to_vec(),
    );

    // Lone surrogates (e.g. a split Win32 UTF-16 pair) cannot form a `str`
    // either — frontends must buffer or drop them before this seam.
    assert!(String::from_utf16(&[0xD800]).is_err());
}

#[test]
fn associated_text_hostile_key_event_vectors() {
    const ROWS: &[TextVector] = &[TextVector {
        name: "del and c1 controls stripped from commit",
        setup: "\x1b[=24u",
        input: "a\u{7f}\u{9f}b",
        expected: "\x1b[0;;97:98u",
    }];
    for row in ROWS {
        check_text_vector(row);
    }

    const KEY_ROWS: &[KeyVector] = &[
        KeyVector {
            name: "key-event text strips controls",
            setup: "\x1b[=26u",
            key: Key::Char('a'),
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Press,
            meta: Meta {
                shifted: None,
                base: None,
                text: Some("a\u{7}b"),
            },
            expected: Some("\x1b[97;5:1;97:98u"),
        },
        KeyVector {
            name: "repeat carries text",
            setup: "\x1b[=26u",
            key: Key::Char('a'),
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Repeat,
            meta: Meta {
                shifted: None,
                base: None,
                text: Some("ab"),
            },
            expected: Some("\x1b[97;5:2;97:98u"),
        },
        KeyVector {
            name: "release never carries text",
            setup: "\x1b[=26u",
            key: Key::Char('a'),
            mods: Modifiers::CTRL,
            kind: KeyEventKind::Release,
            meta: Meta {
                shifted: None,
                base: None,
                text: Some("ab"),
            },
            expected: Some("\x1b[97;5:3u"),
        },
    ];
    for row in KEY_ROWS {
        check_key_vector(row);
    }
}
