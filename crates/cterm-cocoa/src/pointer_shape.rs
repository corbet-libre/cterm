//! Kitty OSC 22 pointer-shape → `NSCursor` mappings (macOS).
//!
//! Maps the shared [`PointerShape`](cterm_core::PointerShape) CSS vocabulary
//! (30 canonical names, `cterm-core/src/pointer_shape.rs`) to AppKit cursors.
//! Runtime verification is impossible on this host (no macOS hardware here —
//! only the `aarch64-apple-darwin` / `x86_64-apple-darwin` type-check targets),
//! so every uncertain mapping below chooses the conservative fallback
//! (`arrowCursor`) and is marked as such. The full table:
//!
//! | CSS shape     | `NSCursor`                  | exact? | note |
//! |---------------|-----------------------------|--------|------|
//! | alias         | `dragLinkCursor`            | no     | drag-context link badge; closest link affordance AppKit ships |
//! | cell          | `crosshairCursor`           | no     | thick-plus cell glyph unavailable; thin crosshair stands in |
//! | copy          | `dragCopyCursor`            | no     | drag-context copy badge; closest copy affordance |
//! | crosshair     | `crosshairCursor`           | yes    | |
//! | default       | `arrowCursor`               | yes    | |
//! | e-resize      | `resizeRightCursor`         | yes    | directional family (10.15+) |
//! | ew-resize     | `resizeLeftRightCursor`     | yes    | |
//! | grab          | `openHandCursor`            | yes    | |
//! | grabbing      | `closedHandCursor`          | yes    | |
//! | help          | `arrowCursor`               | no     | AppKit ships no question-mark help cursor |
//! | move          | `arrowCursor`               | no     | AppKit ships no four-way move cursor outside drag sessions |
//! | n-resize      | `resizeUpCursor`            | yes    | directional family |
//! | ne-resize     | `arrowCursor`               | no     | AppKit ships no diagonal resize cursors |
//! | nesw-resize   | `arrowCursor`               | no     | diagonal resize unavailable (see above) |
//! | no-drop       | `operationNotAllowedCursor` | no     | shares the 🚫 glyph with `not-allowed`; drag nuance lost |
//! | not-allowed   | `operationNotAllowedCursor` | yes    | |
//! | ns-resize     | `resizeUpDownCursor`        | yes    | |
//! | nw-resize     | `arrowCursor`               | no     | diagonal resize unavailable |
//! | nwse-resize   | `arrowCursor`               | no     | diagonal resize unavailable |
//! | pointer       | `pointingHandCursor`        | yes    | |
//! | progress      | `arrowCursor`               | no     | AppKit ships no arrow-plus-hourglass busy cursor |
//! | s-resize      | `resizeDownCursor`          | yes    | directional family |
//! | se-resize     | `arrowCursor`               | no     | diagonal resize unavailable |
//! | sw-resize     | `arrowCursor`               | no     | diagonal resize unavailable |
//! | text          | `IBeamCursor`               | yes    | |
//! | vertical-text | `IBeamCursorForVerticalLayout` | yes | |
//! | w-resize      | `resizeLeftCursor`          | yes    | directional family |
//! | wait          | `arrowCursor`               | no     | AppKit ships no hourglass `NSCursor` (the spinning indicator is not a cursor) |
//! | zoom-in       | `zoomInCursor`              | yes    | |
//! | zoom-out      | `zoomOutCursor`             | yes    | |
//!
//! 16 exact, 14 documented fallbacks. Fallback resolutions are debug-logged
//! once per shape per process (see [`note_fallback`]).

use std::collections::HashSet;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use cterm_core::PointerShape;

/// AppKit cursor selectable for one Kitty CSS pointer shape.
///
/// Variants name the `NSCursor` class cursor; see the module table for the
/// exact-vs-fallback status of each shape that resolves to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CocoaCursor {
    Arrow,
    IBeam,
    IBeamVertical,
    PointingHand,
    Crosshair,
    OpenHand,
    ClosedHand,
    OperationNotAllowed,
    DragCopy,
    DragLink,
    ResizeLeft,
    ResizeRight,
    ResizeLeftRight,
    ResizeUp,
    ResizeDown,
    ResizeUpDown,
    ZoomIn,
    ZoomOut,
}

/// Map one Kitty CSS shape to its AppKit cursor.
///
/// The returned flag is `true` for an exact AppKit counterpart and `false`
/// for the closest documented fallback (module table). Pure and host-safe:
/// no AppKit call happens here, so the mapping itself is unit-testable.
pub fn cocoa_cursor_for_shape(shape: PointerShape) -> (CocoaCursor, bool) {
    use CocoaCursor as Cursor;
    use PointerShape as Shape;
    match shape {
        Shape::Alias => (Cursor::DragLink, false),
        Shape::Cell => (Cursor::Crosshair, false),
        Shape::Copy => (Cursor::DragCopy, false),
        Shape::Crosshair => (Cursor::Crosshair, true),
        Shape::Default => (Cursor::Arrow, true),
        Shape::EResize => (Cursor::ResizeRight, true),
        Shape::EwResize => (Cursor::ResizeLeftRight, true),
        Shape::Grab => (Cursor::OpenHand, true),
        Shape::Grabbing => (Cursor::ClosedHand, true),
        // AppKit ships no question-mark help cursor; the arrow is the
        // conservative stand-in (a wrong affordance beats a missing one).
        Shape::Help => (Cursor::Arrow, false),
        // AppKit ships no four-way move cursor outside drag sessions.
        Shape::Move => (Cursor::Arrow, false),
        Shape::NResize => (Cursor::ResizeUp, true),
        // AppKit ships no diagonal resize cursors (`resizeLeftCursor` and
        // friends cover only the four axes plus the two axis pairs).
        Shape::NeResize | Shape::NeswResize | Shape::NwResize | Shape::NwseResize => {
            (Cursor::Arrow, false)
        }
        // `no-drop` shares the 🚫 glyph with `not-allowed`; only the
        // drag-context nuance is lost.
        Shape::NoDrop => (Cursor::OperationNotAllowed, false),
        Shape::NotAllowed => (Cursor::OperationNotAllowed, true),
        Shape::NsResize => (Cursor::ResizeUpDown, true),
        Shape::Pointer => (Cursor::PointingHand, true),
        // AppKit ships no arrow-plus-hourglass busy cursor.
        Shape::Progress => (Cursor::Arrow, false),
        Shape::SResize => (Cursor::ResizeDown, true),
        Shape::SeResize | Shape::SwResize => (Cursor::Arrow, false),
        Shape::Text => (Cursor::IBeam, true),
        Shape::VerticalText => (Cursor::IBeamVertical, true),
        Shape::WResize => (Cursor::ResizeLeft, true),
        // The spinning progress indicator is not an `NSCursor` at all.
        Shape::Wait => (Cursor::Arrow, false),
        Shape::ZoomIn => (Cursor::ZoomIn, true),
        Shape::ZoomOut => (Cursor::ZoomOut, true),
    }
}

/// Resolve the hover cursor with GTK-matching precedence.
///
/// Returns `None` when a drag owns the cursor (selection drag or
/// mouse-tracking drag: the caller must leave the current cursor alone,
/// matching Kitty's drag exemption). Otherwise the Kitty OSC 22 application
/// shape wins over the local hyperlink cursor, and the hyperlink cursor wins
/// over the default I-beam — the same order the GTK frontend applies in
/// `terminal_widget.rs`.
pub fn hover_cursor(
    selecting: bool,
    osc_shape: Option<PointerShape>,
    has_link: bool,
) -> Option<(CocoaCursor, bool)> {
    if selecting {
        return None;
    }
    if let Some(shape) = osc_shape {
        return Some(cocoa_cursor_for_shape(shape));
    }
    if has_link {
        return Some((CocoaCursor::PointingHand, true));
    }
    Some((CocoaCursor::IBeam, true))
}

/// Debug-log a fallback resolution once per shape per process.
///
/// `mouseMoved:` fires continuously, so an unconditional log would spam;
/// one line per fallback shape still records every conservative choice.
pub fn note_fallback(shape: PointerShape, exact: bool) {
    if exact {
        return;
    }
    static WARNED: OnceLock<parking_lot::Mutex<HashSet<&'static str>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| parking_lot::Mutex::new(HashSet::new()));
    if warned.lock().insert(shape.css_name()) {
        let (cursor, _) = cocoa_cursor_for_shape(shape);
        log::debug!(
            "OSC 22 shape '{}' has no exact NSCursor; falling back to {cursor:?} (see pointer_shape.rs table)",
            shape.css_name(),
        );
    }
}

/// Push the mapped cursor. Must be called on AppKit's main thread.
pub fn push_cocoa_cursor(cursor: CocoaCursor) {
    unsafe {
        let pushed: Retained<AnyObject> = match cursor {
            CocoaCursor::Arrow => msg_send![class!(NSCursor), arrowCursor],
            CocoaCursor::IBeam => msg_send![class!(NSCursor), IBeamCursor],
            CocoaCursor::IBeamVertical => {
                msg_send![class!(NSCursor), IBeamCursorForVerticalLayout]
            }
            CocoaCursor::PointingHand => msg_send![class!(NSCursor), pointingHandCursor],
            CocoaCursor::Crosshair => msg_send![class!(NSCursor), crosshairCursor],
            CocoaCursor::OpenHand => msg_send![class!(NSCursor), openHandCursor],
            CocoaCursor::ClosedHand => msg_send![class!(NSCursor), closedHandCursor],
            CocoaCursor::OperationNotAllowed => {
                msg_send![class!(NSCursor), operationNotAllowedCursor]
            }
            CocoaCursor::DragCopy => msg_send![class!(NSCursor), dragCopyCursor],
            CocoaCursor::DragLink => msg_send![class!(NSCursor), dragLinkCursor],
            CocoaCursor::ResizeLeft => msg_send![class!(NSCursor), resizeLeftCursor],
            CocoaCursor::ResizeRight => msg_send![class!(NSCursor), resizeRightCursor],
            CocoaCursor::ResizeLeftRight => msg_send![class!(NSCursor), resizeLeftRightCursor],
            CocoaCursor::ResizeUp => msg_send![class!(NSCursor), resizeUpCursor],
            CocoaCursor::ResizeDown => msg_send![class!(NSCursor), resizeDownCursor],
            CocoaCursor::ResizeUpDown => msg_send![class!(NSCursor), resizeUpDownCursor],
            CocoaCursor::ZoomIn => msg_send![class!(NSCursor), zoomInCursor],
            CocoaCursor::ZoomOut => msg_send![class!(NSCursor), zoomOutCursor],
        };
        let _: () = msg_send![&*pushed, set];
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
            let (cursor, _) = cocoa_cursor_for_shape(shape);
            // The mapping round-trips through the enum without panicking and
            // stays total if the vocabulary ever grows (the match is
            // exhaustive, so a new variant fails to compile here first).
            let _ = format!("{cursor:?} for {name}");
        }
    }

    #[test]
    fn exact_count_matches_documented_table() {
        let mut exact = 0;
        let mut fallback = 0;
        for name in PointerShape::ALL_CSS {
            let shape = PointerShape::from_css_name(name).expect("CSS name must parse");
            if cocoa_cursor_for_shape(shape).1 {
                exact += 1;
            } else {
                fallback += 1;
            }
        }
        assert_eq!(exact, 16, "exact NSCursor matches");
        assert_eq!(fallback, 14, "documented fallbacks");
    }

    #[test]
    fn hover_precedence_matches_gtk() {
        // Drags keep their cursor.
        assert_eq!(hover_cursor(true, Some(PointerShape::Text), true), None);
        assert_eq!(hover_cursor(true, None, false), None);
        // OSC 22 wins over the hyperlink cursor.
        assert_eq!(
            hover_cursor(false, Some(PointerShape::Text), true),
            Some((CocoaCursor::IBeam, true)),
        );
        // Hyperlink wins over the default.
        assert_eq!(
            hover_cursor(false, None, true),
            Some((CocoaCursor::PointingHand, true)),
        );
        assert_eq!(
            hover_cursor(false, None, false),
            Some((CocoaCursor::IBeam, true)),
        );
    }
}
