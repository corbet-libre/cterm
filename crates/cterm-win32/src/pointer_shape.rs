//! Kitty OSC 22 pointer-shape → Win32 system-cursor mappings.
//!
//! Maps the shared [`PointerShape`](cterm_core::PointerShape) CSS vocabulary
//! (30 canonical names, `cterm-core/src/pointer_shape.rs`) to `LoadCursorW`
//! system cursors (`IDC_*`). Runtime verification is impossible on this host
//! (no Windows hardware here — only the `x86_64-pc-windows-msvc` type-check
//! target), so every uncertain mapping below chooses the conservative
//! fallback (`IDC_ARROW`) and is marked as such. The full table:
//!
//! | CSS shape     | system cursor    | exact? | note |
//! |---------------|------------------|--------|------|
//! | alias         | `IDC_ARROW`      | no     | no shortcut-arrow system cursor ships |
//! | cell          | `IDC_CROSS`      | no     | thick-plus cell glyph unavailable; thin crosshair stands in |
//! | copy          | `IDC_ARROW`      | no     | no copy-affordance system cursor ships |
//! | crosshair     | `IDC_CROSS`      | yes    | |
//! | default       | `IDC_ARROW`      | yes    | |
//! | e-resize      | `IDC_SIZEWE`     | yes    | horizontal pair covers the axis singles |
//! | ew-resize     | `IDC_SIZEWE`     | yes    | |
//! | grab          | `IDC_HAND`       | no     | open-hand glyph unavailable; pointing-hand stands in |
//! | grabbing      | `IDC_HAND`       | no     | closed-hand glyph unavailable; same stand-in |
//! | help          | `IDC_HELP`       | yes    | arrow plus question mark |
//! | move          | `IDC_SIZEALL`    | yes    | four-way move |
//! | n-resize      | `IDC_SIZENS`     | yes    | vertical pair covers the axis singles |
//! | ne-resize     | `IDC_SIZENESW`   | yes    | |
//! | nesw-resize   | `IDC_SIZENESW`   | yes    | |
//! | no-drop       | `IDC_NO`         | no     | shares the 🚫 glyph with `not-allowed`; drag nuance lost |
//! | not-allowed   | `IDC_NO`         | yes    | circle-slash |
//! | ns-resize     | `IDC_SIZENS`     | yes    | |
//! | nw-resize     | `IDC_SIZENWSE`   | yes    | |
//! | nwse-resize   | `IDC_SIZENWSE`   | yes    | |
//! | pointer       | `IDC_HAND`       | yes    | pointing hand |
//! | progress      | `IDC_APPSTARTING`| yes    | arrow plus hourglass |
//! | s-resize      | `IDC_SIZENS`     | yes    | |
//! | se-resize     | `IDC_SIZENWSE`   | yes    | |
//! | sw-resize     | `IDC_SIZENESW`   | yes    | |
//! | text          | `IDC_IBEAM`      | yes    | |
//! | vertical-text | `IDC_IBEAM`      | no     | no vertical I-beam system cursor ships |
//! | w-resize      | `IDC_SIZEWE`     | yes    | |
//! | wait          | `IDC_WAIT`       | yes    | hourglass |
//! | zoom-in       | `IDC_ARROW`      | no     | no magnifier system cursor ships |
//! | zoom-out      | `IDC_ARROW`      | no     | no magnifier system cursor ships |
//!
//! 21 exact, 9 documented fallbacks. Newer `IDC_PIN` / `IDC_PERSON` cursors
//! are deliberately unused (Windows 10+ only; the classic set above loads
//! everywhere). Fallback resolutions are debug-logged once per shape per
//! process (see [`note_fallback`]).

use std::collections::HashSet;
use std::sync::OnceLock;

use cterm_core::PointerShape;

/// Win32 system cursor selectable for one Kitty CSS pointer shape.
///
/// Variants name the `IDC_*` cursor loaded with `LoadCursorW(None, …)`; see
/// the module table for the exact-vs-fallback status of each shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Win32Cursor {
    Arrow,
    IBeam,
    Cross,
    Hand,
    Help,
    No,
    SizeAll,
    SizeNs,
    SizeWe,
    SizeNwse,
    SizeNesw,
    Wait,
    AppStarting,
}

/// Map one Kitty CSS shape to its Win32 system cursor.
///
/// The returned flag is `true` for an exact system counterpart and `false`
/// for the closest documented fallback (module table). Pure and host-safe:
/// no Win32 call happens here, so the mapping itself is unit-testable.
pub fn system_cursor_for_shape(shape: PointerShape) -> (Win32Cursor, bool) {
    use PointerShape as Shape;
    use Win32Cursor as Cursor;
    match shape {
        // No shortcut-arrow system cursor ships; the plain arrow is the
        // conservative stand-in.
        Shape::Alias => (Cursor::Arrow, false),
        Shape::Cell => (Cursor::Cross, false),
        Shape::Copy => (Cursor::Arrow, false),
        Shape::Crosshair => (Cursor::Cross, true),
        Shape::Default => (Cursor::Arrow, true),
        Shape::EResize | Shape::EwResize | Shape::WResize => (Cursor::SizeWe, true),
        // No open/closed-hand system cursors ship; the pointing hand at
        // least conveys "hand". It intentionally collides with `pointer`.
        Shape::Grab | Shape::Grabbing => (Cursor::Hand, false),
        Shape::Help => (Cursor::Help, true),
        Shape::Move => (Cursor::SizeAll, true),
        Shape::NResize | Shape::NsResize | Shape::SResize => (Cursor::SizeNs, true),
        Shape::NeResize | Shape::NeswResize | Shape::SwResize => (Cursor::SizeNesw, true),
        // `no-drop` shares the 🚫 glyph with `not-allowed`; only the
        // drag-context nuance is lost.
        Shape::NoDrop => (Cursor::No, false),
        Shape::NotAllowed => (Cursor::No, true),
        Shape::NwResize | Shape::NwseResize | Shape::SeResize => (Cursor::SizeNwse, true),
        Shape::Pointer => (Cursor::Hand, true),
        Shape::Progress => (Cursor::AppStarting, true),
        Shape::Text => (Cursor::IBeam, true),
        // No vertical I-beam system cursor ships.
        Shape::VerticalText => (Cursor::IBeam, false),
        Shape::Wait => (Cursor::Wait, true),
        // No magnifier system cursors ship.
        Shape::ZoomIn | Shape::ZoomOut => (Cursor::Arrow, false),
    }
}

/// Hover-cursor decision with GTK-matching precedence.
///
/// Pane-divider *hover* keeps its axis resize cursor via [`HoverDecision::Divider`]
/// (chrome owns that affordance, so this helper never names the axis here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverDecision {
    /// A drag owns the cursor (mouse-tracking drag or in-progress
    /// pane-divider drag): leave the current cursor alone, matching Kitty's
    /// drag exemption.
    Keep,
    /// Divider hover: the caller loads the axis resize cursor itself.
    Divider,
    /// Load this system cursor; the flag is `true` for an exact `IDC_*`
    /// counterpart and `false` for the closest documented fallback.
    Cursor(Win32Cursor, bool),
}

/// Resolve the hover cursor with GTK-matching precedence.
///
/// Otherwise the Kitty OSC 22 application shape wins over the local hyperlink
/// cursor, and the hyperlink cursor wins over the default I-beam — the same
/// order the GTK frontend applies in `terminal_widget.rs`.
pub fn hover_cursor(
    dragging: bool,
    divider_hover: bool,
    osc_shape: Option<PointerShape>,
    has_link: bool,
) -> HoverDecision {
    if dragging {
        return HoverDecision::Keep;
    }
    if divider_hover {
        return HoverDecision::Divider;
    }
    if let Some(shape) = osc_shape {
        let (cursor, exact) = system_cursor_for_shape(shape);
        return HoverDecision::Cursor(cursor, exact);
    }
    if has_link {
        return HoverDecision::Cursor(Win32Cursor::Hand, true);
    }
    HoverDecision::Cursor(Win32Cursor::IBeam, true)
}

/// Debug-log a fallback resolution once per shape per process.
///
/// Hover fires continuously, so an unconditional log would spam; one line
/// per fallback shape still records every conservative choice.
pub fn note_fallback(shape: PointerShape, exact: bool) {
    if exact {
        return;
    }
    static WARNED: OnceLock<parking_lot::Mutex<HashSet<&'static str>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| parking_lot::Mutex::new(HashSet::new()));
    if warned.lock().insert(shape.css_name()) {
        let (cursor, _) = system_cursor_for_shape(shape);
        log::debug!(
            "OSC 22 shape '{}' has no exact IDC_* cursor; falling back to {cursor:?} (see pointer_shape.rs table)",
            shape.css_name(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_css_shape_maps() {
        assert_eq!(PointerShape::ALL_CSS.len(), 30);
        for name in PointerShape::ALL_CSS {
            let shape = PointerShape::from_css_name(name).expect("CSS name must parse");
            let (cursor, _) = system_cursor_for_shape(shape);
            let _ = format!("{cursor:?} for {name}");
        }
    }

    #[test]
    fn exact_count_matches_documented_table() {
        let mut exact = 0;
        let mut fallback = 0;
        for name in PointerShape::ALL_CSS {
            let shape = PointerShape::from_css_name(name).expect("CSS name must parse");
            if system_cursor_for_shape(shape).1 {
                exact += 1;
            } else {
                fallback += 1;
            }
        }
        assert_eq!(exact, 21, "exact IDC_* matches");
        assert_eq!(fallback, 9, "documented fallbacks");
    }

    #[test]
    fn hover_precedence_matches_gtk() {
        // Drags keep their cursor.
        assert_eq!(
            hover_cursor(true, false, Some(PointerShape::Text), true),
            HoverDecision::Keep
        );
        // Divider hover keeps the chrome resize cursor.
        assert_eq!(
            hover_cursor(false, true, Some(PointerShape::Text), true),
            HoverDecision::Divider
        );
        // OSC 22 wins over the hyperlink cursor.
        assert_eq!(
            hover_cursor(false, false, Some(PointerShape::Text), true),
            HoverDecision::Cursor(Win32Cursor::IBeam, true),
        );
        // Hyperlink wins over the default.
        assert_eq!(
            hover_cursor(false, false, None, true),
            HoverDecision::Cursor(Win32Cursor::Hand, true),
        );
        assert_eq!(
            hover_cursor(false, false, None, false),
            HoverDecision::Cursor(Win32Cursor::IBeam, true),
        );
    }
}
