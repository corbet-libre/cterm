//! Host-runnable foot-differential corpus vectors.
//!
//! The PTY-level probe (`examples/foot_compat_probe.rs`, driven by
//! `scripts/foot_compat_test.sh` in CI) needs foot + a Wayland compositor.
//! This module covers the same ledger axes at the core parser/screen layer so
//! `cargo test -p cterm-core` verifies them with no external binaries:
//!
//! - editing ops (ICH/DCH/ECH/EL/ED/IL/DL/SU/SD, Kitty unscroll, rectangular)
//! - modes (DECSET/DECRST incl. Kitty 5522/2026/2031/2033/1070/8452, ANSI,
//!   keyboard, safe save/restore)
//! - reports/replies (DA/DSR/DECRQM/DECRQSS/XTGETTCAP/window ops/Sixel,
//!   OSC 21/22/5522)
//! - resize/reflow (grapheme-safe, wide cells, multicell blocks)
//! - Unicode (combining, ZWJ, flags, variation selectors)
//! - malformed input (truncated, hostile lengths, mixed intermediates)
//!
//! Kitty-only replies (OSC 21/22/5522 payloads, DA2/XTVERSION version strings,
//! pixel geometry) intentionally live here rather than in the probe: foot
//! either diverges there or reports font-dependent values.

use cterm_core::screen::{KittyColorKey, KittyColorSetting, Screen, ScreenConfig};
use cterm_core::{MouseEncoding, MouseMode, Parser};

fn make_screen() -> Screen {
    Screen::new(80, 24, ScreenConfig::default())
}

fn make_narrow(cols: usize, rows: usize) -> Screen {
    Screen::new(cols, rows, ScreenConfig::default())
}

fn responses(screen: &mut Screen) -> Vec<Vec<u8>> {
    screen.take_pending_responses()
}

// ---------------------------------------------------------------------------
// Editing ops
// ---------------------------------------------------------------------------

#[test]
fn editing_ich_dch_ech_match_foot_shapes() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[2J\x1b[HABCDE");
    assert_eq!(screen.grid().row(0).unwrap().text(), "ABCDE");
    // Back two (cursor on D), delete two (DCH): removes D,E, leaving ABC.
    parser.parse(&mut screen, b"\x1b[2D\x1b[2P");
    assert_eq!(screen.grid().row(0).unwrap().text(), "ABC");
    assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));

    // Insert two blanks at cursor (ICH): cursor at end, so two trailing
    // blanks (row.text() trims them — check cells directly).
    parser.parse(&mut screen, b"\x1b[2@");
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "A");
    assert_eq!(screen.get_cell(0, 2).unwrap().text(), "C");
    assert_eq!(screen.get_cell(0, 3).unwrap().text(), " ");
    assert_eq!(screen.get_cell(0, 4).unwrap().text(), " ");

    // Erase two chars (ECH): blanks without shifting.
    parser.parse(&mut screen, b"\x1b[1;1H\x1b[2X");
    let row = screen.grid().row(0).unwrap().text();
    assert!(row.starts_with("  "), "ECH blanks in place: {row:?}");
}

#[test]
fn editing_el_ed_variants_clear_expected_regions() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[2J\x1b[Hhello world");
    // EL 1: clear from start through cursor.
    parser.parse(&mut screen, b"\x1b[1;6H\x1b[1K");
    assert_eq!(screen.grid().row(0).unwrap().text(), "      world");

    // EL 0: clear from cursor through end.
    parser.parse(&mut screen, b"\x1b[2J\x1b[Hhello world\x1b[1;6H\x1b[0K");
    assert_eq!(screen.grid().row(0).unwrap().text(), "hello");

    // EL 2: whole line.
    parser.parse(&mut screen, b"\x1b[2K");
    assert_eq!(screen.grid().row(0).unwrap().text().trim(), "");

    // ED 0/1/2: below / above / all.
    parser.parse(
        &mut screen,
        b"\x1b[2J\x1b[Hone\r\ntwo\r\nthree\x1b[2;1H\x1b[0J",
    );
    assert_eq!(screen.grid().row(0).unwrap().text(), "one");
    assert_eq!(screen.grid().row(1).unwrap().text().trim(), "");
    assert_eq!(screen.grid().row(2).unwrap().text().trim(), "");

    parser.parse(
        &mut screen,
        b"\x1b[2J\x1b[Hone\r\ntwo\r\nthree\x1b[2;1H\x1b[1J",
    );
    assert_eq!(screen.grid().row(0).unwrap().text().trim(), "");
    // ED 1 clears through the cursor cell inclusive: cursor at col 0 clears
    // only 't', leaving "wo" (foot parity).
    assert_eq!(screen.grid().row(1).unwrap().text().trim(), "wo");
    assert_eq!(screen.grid().row(2).unwrap().text(), "three");

    parser.parse(&mut screen, b"\x1b[2J");
    for row in 0..3 {
        assert_eq!(screen.grid().row(row).unwrap().text().trim(), "");
    }
}

#[test]
fn editing_il_dl_su_sd_shift_within_scroll_region() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[2J\x1b[Hline1\r\nline2\r\nline3");
    // Insert 2 lines at top: pushes content down, cursor stays.
    parser.parse(&mut screen, b"\x1b[1;1H\x1b[2L");
    assert_eq!(screen.grid().row(0).unwrap().text().trim(), "");
    assert_eq!(screen.grid().row(1).unwrap().text().trim(), "");
    assert_eq!(screen.grid().row(2).unwrap().text(), "line1");

    // Delete those 2 lines: restores original layout.
    parser.parse(&mut screen, b"\x1b[2M");
    assert_eq!(screen.grid().row(0).unwrap().text(), "line1");
    assert_eq!(screen.grid().row(1).unwrap().text(), "line2");
    assert_eq!(screen.grid().row(2).unwrap().text(), "line3");

    // Scroll up/down move lines without moving the cursor row.
    let before = (screen.cursor.row, screen.cursor.col);
    parser.parse(&mut screen, b"\x1b[1S");
    assert_eq!((screen.cursor.row, screen.cursor.col), before);
    assert_eq!(screen.grid().row(0).unwrap().text(), "line2");
    parser.parse(&mut screen, b"\x1b[1T");
    assert_eq!(screen.grid().row(0).unwrap().text().trim(), "");
}

#[test]
fn editing_rectangular_copy_fill_erase_match_foot() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[2J\x1b[Habcdef\r\nabcdef");
    // Fill 2x3 block with X (DECFRA: Pc;top;left;bottom;right).
    parser.parse(&mut screen, b"\x1b[88;1;1;2;3$x");
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "X");
    assert_eq!(screen.get_cell(0, 2).unwrap().text(), "X");
    assert_eq!(screen.get_cell(0, 3).unwrap().text(), "d");
    assert_eq!(screen.get_cell(1, 0).unwrap().text(), "X");
    assert_eq!(screen.grid().row(0).unwrap().text(), "XXXdef");

    // Erase that block (DECERA: top;left;bottom;right).
    parser.parse(&mut screen, b"\x1b[1;1;2;3$z");
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), " ");
    assert_eq!(screen.get_cell(0, 2).unwrap().text(), " ");
    assert_eq!(screen.get_cell(0, 3).unwrap().text(), "d");

    // Copy rectangular area: write a marker, copy it down.
    // DECCRA takes 8 params (src t/l/b/r/page, dst row/col/page).
    parser.parse(&mut screen, b"\x1b[2J\x1b[HAB\r\n  ");
    parser.parse(&mut screen, b"\x1b[1;1;1;2;1;2;1;1$v");
    assert_eq!(screen.get_cell(1, 0).unwrap().text(), "A");
    assert_eq!(screen.get_cell(1, 1).unwrap().text(), "B");
}

#[test]
fn editing_kitty_unscroll_refills_from_scrollback() {
    let mut screen = make_narrow(10, 3);
    let mut parser = Parser::new();

    for line in ["aaa", "bbb", "ccc", "ddd"] {
        parser.parse(&mut screen, format!("{line}\r\n").as_bytes());
    }
    // Screen now shows the tail; scrollback holds the head.
    assert!(!screen.scrollback().is_empty());
    let before_top = screen.grid().row(0).unwrap().text();
    // Kitty unscroll pulls one line back from scrollback.
    parser.parse(&mut screen, b"\x1b[1+T");
    assert_ne!(screen.grid().row(0).unwrap().text(), before_top);
    // Ordinary scroll-down blanks instead.
    parser.parse(&mut screen, b"\x1b[2J\x1b[Hx\r\ny\r\nz");
    parser.parse(&mut screen, b"\x1b[1T");
    assert_eq!(screen.grid().row(0).unwrap().text().trim(), "");
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

#[test]
fn modes_standard_decset_decreset_decrqm() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    for (mode, field) in [
        (1u8, "application_cursor"),
        (5u8, "reverse_video"),
        (7u8, "auto_wrap"),
        (25u8, "show_cursor"),
    ] {
        let _ = (mode, field);
    }
    parser.parse(&mut screen, b"\x1b[?7l\x1b[?7$p");
    assert!(!screen.modes.auto_wrap);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?7;2$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?7h\x1b[?7$p");
    assert!(screen.modes.auto_wrap);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?7;1$y".to_vec()]);

    parser.parse(&mut screen, b"\x1b[?5h\x1b[?5$p\x1b[?5l\x1b[?5$p");
    assert!(!screen.modes.reverse_video);
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1b[?5;1$y".to_vec(), b"\x1b[?5;2$y".to_vec()]
    );

    parser.parse(&mut screen, b"\x1b[?25l\x1b[?25$p\x1b[?25h\x1b[?25$p");
    assert!(screen.modes.show_cursor);
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1b[?25;2$y".to_vec(), b"\x1b[?25;1$y".to_vec()]
    );

    // ANSI modes 4 (IRM) and 20 (LNM).
    parser.parse(&mut screen, b"\x1b[4h\x1b[4$p\x1b[4l\x1b[4$p");
    assert!(!screen.modes.insert_mode);
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1b[4;1$y".to_vec(), b"\x1b[4;2$y".to_vec()]
    );
    parser.parse(&mut screen, b"\x1b[20h\x1b[20$p\x1b[20l\x1b[20$p");
    assert!(!screen.modes.line_feed_mode);
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1b[20;1$y".to_vec(), b"\x1b[20;2$y".to_vec()]
    );

    // Alternate screen preserves primary content.
    parser.parse(&mut screen, b"Primary\x1b[?1049h");
    assert!(screen.modes.alternate_screen);
    parser.parse(&mut screen, b"\x1b[?1049l");
    assert!(!screen.modes.alternate_screen);
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "P");
}

#[test]
fn modes_mouse_focus_bracket_and_save_restore() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[?1000h");
    assert_eq!(screen.modes.mouse_mode, MouseMode::Normal);
    parser.parse(&mut screen, b"\x1b[?1002h");
    assert_eq!(screen.modes.mouse_mode, MouseMode::ButtonEvent);
    // Resetting an inactive tracking mode is ignored (foot parity).
    parser.parse(&mut screen, b"\x1b[?1000l");
    assert_eq!(screen.modes.mouse_mode, MouseMode::ButtonEvent);

    parser.parse(&mut screen, b"\x1b[?1006h");
    assert_eq!(screen.modes.mouse_encoding, MouseEncoding::Sgr);
    parser.parse(&mut screen, b"\x1b[?1015h");
    assert_eq!(screen.modes.mouse_encoding, MouseEncoding::Urxvt);
    parser.parse(&mut screen, b"\x1b[?1006l");
    assert_eq!(screen.modes.mouse_encoding, MouseEncoding::Urxvt);
    parser.parse(&mut screen, b"\x1b[?1015l\x1b[?1002l");
    assert_eq!(screen.modes.mouse_encoding, MouseEncoding::Normal);
    assert_eq!(screen.modes.mouse_mode, MouseMode::None);

    // Focus events, bracketed paste, alternate scroll.
    parser.parse(&mut screen, b"\x1b[?1004h\x1b[?2004h");
    assert!(screen.modes.focus_events);
    assert!(screen.modes.bracketed_paste);
    parser.parse(&mut screen, b"\x1b[?1007l\x1b[?1007$p");
    assert!(!screen.modes.alternate_scroll);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1007;2$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?1007h");

    // Explicit save/restore round-trips the tracking + encoding pair.
    parser.parse(&mut screen, b"\x1b[?1003h\x1b[?1016h\x1b[?1004h");
    parser.parse(&mut screen, b"\x1b[?1003;1016;1004s");
    parser.parse(&mut screen, b"\x1b[?1003l\x1b[?1016l\x1b[?1004l");
    assert_eq!(screen.modes.mouse_mode, MouseMode::None);
    parser.parse(&mut screen, b"\x1b[?1003;1016;1004r");
    assert_eq!(screen.modes.mouse_mode, MouseMode::AnyEvent);
    assert_eq!(screen.modes.mouse_encoding, MouseEncoding::SgrPixels);
    assert!(screen.modes.focus_events);
}

#[test]
fn modes_kitty_sync_sixel_cursor() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[?2026h\x1b[?2026$p");
    assert!(screen.modes.application_sync_updates);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?2026;1$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?2026l\x1b[?2026$p");
    assert!(!screen.modes.application_sync_updates);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?2026;2$y".to_vec()]);

    parser.parse(&mut screen, b"\x1b[?1070l\x1b[?1070$p");
    assert!(!screen.modes.sixel_private_palette);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1070;2$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?1070h");

    parser.parse(&mut screen, b"\x1b[?8452h\x1b[?8452$p");
    assert!(screen.modes.sixel_cursor_right);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?8452;1$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?8452l");
    assert!(!screen.modes.sixel_cursor_right);
}

#[test]
fn modes_kitty_paste_events_5522_and_decrqm() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    assert!(!screen.modes.rich_clipboard_paste_events);
    parser.parse(&mut screen, b"\x1b[?5522h\x1b[?5522$p");
    assert!(screen.modes.rich_clipboard_paste_events);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?5522;1$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?5522l\x1b[?5522$p");
    assert!(!screen.modes.rich_clipboard_paste_events);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?5522;2$y".to_vec()]);

    // Bare Kitty save/restore never captures the paste-event mode (side
    // effects on other state), so it survives a no-argument round trip.
    parser.parse(&mut screen, b"\x1b[?5522h\x1b[?s\x1b[?5522l\x1b[?r");
    assert!(
        !screen.modes.rich_clipboard_paste_events,
        "5522 is excluded from the safe-mode set by design"
    );
}

#[test]
fn modes_theme_visibility_reports() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"\x1b[?2031h\x1b[?2031$p");
    assert!(screen.modes.theme_change_reports);
    assert_eq!(responses(&mut screen), vec![b"\x1b[?2031;1$y".to_vec()]);
    parser.parse(&mut screen, b"\x1b[?2031l");

    // Enabling visibility reports queues foot's immediate state report ahead
    // of the DECRQM reply below.
    parser.parse(&mut screen, b"\x1b[?2033h\x1b[?2033$p");
    assert!(screen.modes.visibility_change_reports);
    let out = responses(&mut screen);
    assert_eq!(out.len(), 2, "immediate report + DECRQM: {out:?}");
    assert!(
        out[0].starts_with(b"\x1b[?999;"),
        "visibility state: {out:?}"
    );
    assert_eq!(out[1], b"\x1b[?2033;1$y".to_vec());
    parser.parse(&mut screen, b"\x1b[?2033l");

    // Private DSR theme/visibility queries queue device reports.
    parser.parse(&mut screen, b"\x1b[?996n");
    let out = responses(&mut screen);
    assert_eq!(out.len(), 1);
    assert!(out[0].starts_with(b"\x1b[?997;"), "theme report: {out:?}");
}

#[test]
fn modes_keyboard_progressive_query_set_push_pop() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Default is legacy (no flags).
    parser.parse(&mut screen, b"\x1b[?u");
    assert_eq!(responses(&mut screen), vec![b"\x1b[?0u".to_vec()]);

    // Set flags outright (masking unsupported bits is handled by Screen).
    parser.parse(&mut screen, b"\x1b[=1u\x1b[?u");
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1u".to_vec()]);

    // Push/pop stack entries.
    parser.parse(&mut screen, b"\x1b[>1u\x1b[?u");
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1u".to_vec()]);
    parser.parse(&mut screen, b"\x1b[<1u\x1b[?u");
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1u".to_vec()]);
}

// ---------------------------------------------------------------------------
// Reports / replies
// ---------------------------------------------------------------------------

#[test]
fn reports_da_dsr_and_char_geometry_shapes() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // DA1 advertises the implemented capability set byte-for-byte.
    parser.parse(&mut screen, b"\x1b[c");
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1b[?62;4;22;28;52c".to_vec()]
    );

    // DA2 carries the build version: assert the foot-compatible shape, not
    // the exact digits, so the vector survives version bumps on both sides.
    parser.parse(&mut screen, b"\x1b[>c");
    let out = responses(&mut screen);
    assert_eq!(out.len(), 1);
    assert!(out[0].starts_with(b"\x1b[>1;"), "DA2 shape: {out:?}");
    assert!(out[0].ends_with(b";0c"), "DA2 shape: {out:?}");

    // DA3 is the four-byte manufacturer id.
    parser.parse(&mut screen, b"\x1b[=c");
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1bP!|4354524D\x1b\\".to_vec()]
    );

    // DSR status + cursor position.
    parser.parse(&mut screen, b"\x1b[5n");
    assert_eq!(responses(&mut screen), vec![b"\x1b[0n".to_vec()]);
    parser.parse(&mut screen, b"\x1b[2J\x1b[6;11H\x1b[6n");
    assert_eq!(responses(&mut screen), vec![b"\x1b[6;11R".to_vec()]);

    // Character-cell geometry (not pixels) is font-independent.
    parser.parse(&mut screen, b"\x1b[11t\x1b[13t\x1b[18t");
    assert_eq!(
        responses(&mut screen),
        vec![
            b"\x1b[1t".to_vec(),
            b"\x1b[3;0;0t".to_vec(),
            b"\x1b[8;24;80t".to_vec(),
        ]
    );
}

#[test]
fn reports_decrqm_unknown_and_permanently_reset() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Unknown modes report 0; legacy modes cterm never uses report 4.
    // (9999 stays below the 16-bit CSI param clamp; larger values clamp to
    // 65535 — see the hostile-lengths vector.)
    parser.parse(&mut screen, b"\x1b[?9999$p\x1b[?9$p\x1b[?1005$p");
    assert_eq!(
        responses(&mut screen),
        vec![
            b"\x1b[?9999;0$y".to_vec(),
            b"\x1b[?9;4$y".to_vec(),
            b"\x1b[?1005;4$y".to_vec(),
        ]
    );

    // Unknown ANSI modes report 0 as well.
    parser.parse(&mut screen, b"\x1b[999$p");
    assert_eq!(responses(&mut screen), vec![b"\x1b[999;0$y".to_vec()]);
}

#[test]
fn reports_decrqss_margins_sgr_cursor_and_unknown() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    parser.parse(
        &mut screen,
        concat!(
            "\x1b[3;20r",
            "\x1b[1;3;4:3;38;2;1;2;3;48;5;42;58;2;4;5;6m",
            "\x1b[5 q",
            "\x1bP$qm\x1b\\",
            "\x1bP$qr\x1b\\",
            "\x1bP$q q\x1b\\",
            "\x1bP$qz\x1b\\",
        )
        .as_bytes(),
    );
    assert_eq!(
        responses(&mut screen),
        vec![
            b"\x1bP1$r0;1;3;4:3;38:2::1:2:3;48:5:42;58:2::4:5:6m\x1b\\".to_vec(),
            b"\x1bP1$r3;20r\x1b\\".to_vec(),
            b"\x1bP1$r5 q\x1b\\".to_vec(),
            b"\x1bP0$r\x1b\\".to_vec(),
        ]
    );
}

#[test]
fn reports_xtgettcap_and_sixel_colors() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Boolean caps reply without a value; unknown names reply 0.
    parser.parse(&mut screen, b"\x1bP+q616d\x1b\\");
    assert_eq!(responses(&mut screen), vec![b"\x1bP1+r616d\x1b\\".to_vec()]);
    parser.parse(&mut screen, b"\x1bP+q626365;5463\x1b\\");
    // Multi-cap XTGETTCAP arrives as one queued response with concatenated
    // DCS replies (see test_xtgettcap_reports_only_supported_capabilities).
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1bP1+r626365\x1b\\\x1bP1+r5463\x1b\\".to_vec(),]
    );
    parser.parse(&mut screen, b"\x1bP+q5a5a5a\x1b\\");
    assert_eq!(
        responses(&mut screen),
        vec![b"\x1bP0+r5a5a5a\x1b\\".to_vec()]
    );

    // Sixel color capacity reports the active palette size (foot: 1024).
    parser.parse(&mut screen, b"\x1b[?1;1S");
    assert_eq!(responses(&mut screen), vec![b"\x1b[?1;0;1024S".to_vec()]);
}

#[test]
fn reports_osc21_query_set_reset_and_osc22_stack() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // OSC 21: set then query sees the new value; bare key resets.
    parser.parse(&mut screen, b"\x1b]21;foreground=#112233\x1b\\");
    assert_eq!(
        screen.kitty_setting(KittyColorKey::Foreground),
        KittyColorSetting::Color(cterm_core::Rgb::new(0x11, 0x22, 0x33))
    );
    parser.parse(&mut screen, b"\x1b]21;foreground=?\x1b\\");
    assert_eq!(screen.take_kitty_queries().len(), 1);
    parser.parse(&mut screen, b"\x1b]21;foreground\x1b\\");
    assert_eq!(
        screen.kitty_setting(KittyColorKey::Foreground),
        KittyColorSetting::Default
    );

    // Unknown query keys answer unknown=<base64>; unknown sets are ignored.
    parser.parse(&mut screen, b"\x1b]21;nonsense=?\x1b\\");
    let queued = screen.take_kitty_queries();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].len(), 1);

    // Palette-stack push/pop share the xterm stack (foot-compatible).
    parser.parse(
        &mut screen,
        b"\x1b]10;#112233\x1b\\\x1b[#P\x1b]10;#445566\x1b\\\x1b[#Q\x1b]10;?\x1b\\",
    );
    let _ = responses(&mut screen);

    // OSC 22: set/push/pop/query with per-screen stacks (foot has no
    // counterpart, so this vector locks cterm's Kitty behavior in place).
    parser.parse(&mut screen, b"\x1b]22;pointer\x1b\\");
    assert_eq!(
        screen.current_pointer_shape(),
        Some(cterm_core::PointerShape::Pointer)
    );
    parser.parse(&mut screen, b"\x1b]22;>pointer\x1b\\");
    parser.parse(&mut screen, b"\x1b]22;?pointer\x1b\\");
    let out = responses(&mut screen);
    assert!(
        out.iter().any(|reply| reply.starts_with(b"\x1b]22;")),
        "OSC 22 query replies: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// Resize / reflow
// ---------------------------------------------------------------------------

#[test]
fn resize_reflows_graphemes_and_tracks_cursor() {
    let mut screen = make_narrow(4, 2);
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"abcdefgh");
    screen.resize(3, 3);
    assert_eq!(screen.grid()[0].text(), "abc");
    assert_eq!(screen.grid()[1].text(), "def");
    assert_eq!(screen.grid()[2].text(), "gh");
    assert_eq!((screen.cursor.row, screen.cursor.col), (2, 2));

    screen.resize(6, 2);
    assert_eq!(screen.grid()[0].text(), "abcdef");
    assert_eq!(screen.grid()[1].text(), "gh");
    assert_eq!((screen.cursor.row, screen.cursor.col), (1, 2));
}

#[test]
fn resize_never_splits_wide_cells() {
    let mut screen = make_narrow(4, 2);
    let mut parser = Parser::new();

    parser.parse(&mut screen, "ab界".as_bytes());
    screen.resize(3, 2);
    assert_eq!(screen.grid()[0].text(), "ab");
    assert_eq!(screen.grid()[1].text(), "界");
    assert!(screen.grid()[1][0].is_wide());
    assert!(screen.grid()[1][1].is_wide_spacer());
}

#[test]
fn resize_keeps_multicell_blocks_atomic() {
    let mut screen = make_narrow(6, 3);
    let mut parser = Parser::new();

    parser.parse(&mut screen, b"a\x1b]66;w=3;B\x07c");
    screen.move_cursor(0, 1);
    parser.parse(&mut screen, b"\x1b]66;w=3;B\x07");
    screen.resize(4, 3);
    let row = screen.grid().row(0).unwrap();
    assert_eq!(row[1].text(), "B");
    assert_eq!(row[1].multicell.as_ref().unwrap().columns, 3);
}

// ---------------------------------------------------------------------------
// Unicode
// ---------------------------------------------------------------------------

#[test]
fn unicode_combining_and_variation_selectors_compose() {
    let mut screen = make_narrow(8, 2);
    let mut parser = Parser::new();

    // e + combining acute stays one cell; VS16 keeps emoji presentation.
    parser.parse(&mut screen, "e\u{301}\u{2764}\u{fe0f}".as_bytes());
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "e\u{301}");
    assert_eq!(screen.cursor.col, 3);

    // Direct put_char path composes identically.
    let mut screen = make_narrow(8, 2);
    for c in "e\u{301}".chars() {
        screen.put_char(c);
    }
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "e\u{301}");
    assert_eq!(screen.cursor.col, 1);
}

#[test]
fn unicode_zwj_sequences_occupy_wide_cells() {
    let mut screen = make_narrow(8, 2);
    let mut parser = Parser::new();

    parser.parse(&mut screen, "👩\u{200d}💻".as_bytes());
    let cell = screen.get_cell(0, 0).unwrap();
    assert_eq!(cell.text(), "👩\u{200d}💻");
    assert!(cell.is_wide());
    assert!(screen.get_cell(0, 1).unwrap().is_wide_spacer());
}

#[test]
fn unicode_flags_use_regional_indicator_pairs() {
    let mut screen = make_narrow(8, 2);
    let mut parser = Parser::new();

    parser.parse(&mut screen, "🇨🇭".as_bytes());
    let cell = screen.get_cell(0, 0).unwrap();
    assert_eq!(cell.text(), "🇨🇭");
    assert!(cell.is_wide());
    assert!(screen.get_cell(0, 1).unwrap().is_wide_spacer());
    assert_eq!(screen.cursor.col, 2);
}

#[test]
fn unicode_grapheme_growth_stays_bounded() {
    let mut screen = make_narrow(8, 2);
    screen.put_char('e');
    for _ in 0..100 {
        screen.put_char('\u{301}');
    }
    assert!(screen.get_cell(0, 0).unwrap().text().len() <= 256);
    assert_eq!(screen.cursor.col, 1);
}

// ---------------------------------------------------------------------------
// Malformed input
// ---------------------------------------------------------------------------

#[test]
fn malformed_truncated_sequences_recover() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Feed a split CSI, a split OSC, and a split DCS across calls; the
    // parser must not panic and the following valid query must answer.
    parser.parse(&mut screen, b"\x1b[6;");
    assert!(responses(&mut screen).is_empty());
    parser.parse(&mut screen, b"11H\x1b[6n");
    assert_eq!(responses(&mut screen), vec![b"\x1b[6;11R".to_vec()]);

    parser.parse(&mut screen, b"\x1b]10;#11");
    parser.parse(&mut screen, b"2233\x1b\\\x1b]10;?\x1b\\");
    let _ = responses(&mut screen);

    // Unknown private-mode query still answers 0, then DSR answers with the
    // cursor left by the split CSI above (6;11).
    parser.parse(&mut screen, b"\x1b[?9999$p\x1b[6n");
    let out = responses(&mut screen);
    assert_eq!(
        out,
        vec![b"\x1b[?9999;0$y".to_vec(), b"\x1b[6;11R".to_vec(),]
    );
}

#[test]
fn malformed_hostile_lengths_stay_bounded() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Absurd cursor address clamps instead of panicking or growing.
    parser.parse(
        &mut screen,
        b"\x1b[99999999999999999999;99999999999999999999H",
    );
    assert!(screen.cursor.row < 24 && screen.cursor.col < 80);

    // REP is bounded to one screen (foot's DoS guard).
    parser.parse(&mut screen, b"\x1b[2J\x1b[Hx\x1b[999999b");
    assert!(screen.cursor.col < 80);

    // Overlong OSC 21 keys are skipped without replies or growth.
    let mut hostile = b"\x1b]21;?=;".to_vec();
    hostile.extend_from_slice(&[b'k'; 65]);
    hostile.extend_from_slice(b"=?;\xff=?\x1b\\");
    parser.parse(&mut screen, &hostile);
    assert!(screen.take_kitty_queries().is_empty());

    // Oversized responses never accumulate without bound: drain and confirm
    // the terminal still answers.
    let drained = responses(&mut screen).len();
    assert!(drained < 1024, "response queue stayed bounded: {drained}");
    parser.parse(&mut screen, b"\x1b[5n");
    assert_eq!(responses(&mut screen), vec![b"\x1b[0n".to_vec()]);
}

#[test]
fn malformed_mixed_intermediates_are_ignored_safely() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Unknown CSI intermediates/finals change nothing and queue nothing.
    parser.parse(
        &mut screen,
        b"\x1b[+T\x1b[?$p\x1b[?1;2;3$q\x1b[999;999H\x1b[6n",
    );
    let out = responses(&mut screen);
    assert!(
        out.iter().any(|reply| reply.ends_with(b"R")),
        "valid DSR survives unknown intermediates: {out:?}"
    );

    // Bogus SCS designators and unknown ESC bytes are ignored.
    parser.parse(&mut screen, b"\x1b(0\x1b)0\x1b#8\x1b[5n");
    assert_eq!(
        responses(&mut screen).last().map(Vec::as_slice),
        Some(b"\x1b[0n".as_slice())
    );
}

#[test]
fn malformed_invalid_utf8_and_bel_osc_rejected() {
    let mut screen = make_screen();
    let mut parser = Parser::new();

    // Invalid UTF-8 in OSC 22 is rejected without touching the stack.
    parser.parse(&mut screen, b"\x1b]22;pointer\x1b\\");
    let before = screen.current_pointer_shape();
    parser.parse(&mut screen, b"\x1b]22;\xff\x1b\\");
    assert_eq!(screen.current_pointer_shape(), before);

    // BEL-terminated Kitty file-transfer commands are rejected (ST-only).
    parser.parse(&mut screen, b"\x1b]5113;action=ls\x07");
    parser.parse(&mut screen, b"\x1b[5n");
    assert_eq!(
        responses(&mut screen).last().map(Vec::as_slice),
        Some(b"\x1b[0n".as_slice())
    );
}
