//! Kitty mouse-pointer-shape state (OSC 22).
//!
//! Implements the shape vocabulary from Kitty's
//! [pointer-shapes](https://sw.kovidgoyal.net/kitty/pointer-shapes/)
//! extension: CSS cursor names with `push`/`pop` stack semantics, `set`
//! replacement, and `?` queries. The per-screen stacks live on [`Screen`];
//! this module owns the shape vocabulary and its wire conversions.

use serde::{Deserialize, Serialize};

/// Maximum entries per pointer-shape stack.
///
/// Matches Kitty's `main_pointer_shape_stack` / `alternate_pointer_shape_stack`
/// (`uint8_t stack[16]` in `kitty/screen.h`).
pub const MAX_POINTER_SHAPE_STACK_DEPTH: usize = 16;

/// Kitty's default `__default__` answer.
///
/// Kitty's `default_pointer_shape` option defaults to `beam`, which maps to
/// the CSS `text` shape. cterm's GTK frontend likewise uses an I-beam over
/// terminal text, so `text` is the compatible default. Native frontends may
/// specialize this once they expose their own configured defaults.
pub const DEFAULT_POINTER_CSS: &str = "text";

/// Kitty's default `__grabbed__` answer.
///
/// Kitty's `pointer_shape_when_grabbed` option defaults to `arrow`, which
/// maps to the CSS `default` shape.
pub const GRABBED_POINTER_CSS: &str = "default";

/// Mouse pointer shapes from the CSS cursor vocabulary required by the Kitty
/// pointer-shapes extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PointerShape {
    Alias,
    Cell,
    Copy,
    Crosshair,
    Default,
    EResize,
    EwResize,
    Grab,
    Grabbing,
    Help,
    Move,
    NResize,
    NeResize,
    NeswResize,
    NoDrop,
    NotAllowed,
    NsResize,
    NwResize,
    NwseResize,
    Pointer,
    Progress,
    SResize,
    SeResize,
    SwResize,
    Text,
    VerticalText,
    WResize,
    Wait,
    ZoomIn,
    ZoomOut,
}

impl PointerShape {
    /// Canonical CSS cursor name used on the wire and by native frontends.
    pub const fn css_name(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Cell => "cell",
            Self::Copy => "copy",
            Self::Crosshair => "crosshair",
            Self::Default => "default",
            Self::EResize => "e-resize",
            Self::EwResize => "ew-resize",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::Help => "help",
            Self::Move => "move",
            Self::NResize => "n-resize",
            Self::NeResize => "ne-resize",
            Self::NeswResize => "nesw-resize",
            Self::NoDrop => "no-drop",
            Self::NotAllowed => "not-allowed",
            Self::NsResize => "ns-resize",
            Self::NwResize => "nw-resize",
            Self::NwseResize => "nwse-resize",
            Self::Pointer => "pointer",
            Self::Progress => "progress",
            Self::SResize => "s-resize",
            Self::SeResize => "se-resize",
            Self::SwResize => "sw-resize",
            Self::Text => "text",
            Self::VerticalText => "vertical-text",
            Self::WResize => "w-resize",
            Self::Wait => "wait",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
        }
    }

    /// Parse one of the 30 canonical CSS shape names. Anything else —
    /// including empty strings, whitespace, and mixed case — is rejected.
    pub fn from_css_name(name: &str) -> Option<Self> {
        Some(match name {
            "alias" => Self::Alias,
            "cell" => Self::Cell,
            "copy" => Self::Copy,
            "crosshair" => Self::Crosshair,
            "default" => Self::Default,
            "e-resize" => Self::EResize,
            "ew-resize" => Self::EwResize,
            "grab" => Self::Grab,
            "grabbing" => Self::Grabbing,
            "help" => Self::Help,
            "move" => Self::Move,
            "n-resize" => Self::NResize,
            "ne-resize" => Self::NeResize,
            "nesw-resize" => Self::NeswResize,
            "no-drop" => Self::NoDrop,
            "not-allowed" => Self::NotAllowed,
            "ns-resize" => Self::NsResize,
            "nw-resize" => Self::NwResize,
            "nwse-resize" => Self::NwseResize,
            "pointer" => Self::Pointer,
            "progress" => Self::Progress,
            "s-resize" => Self::SResize,
            "se-resize" => Self::SeResize,
            "sw-resize" => Self::SwResize,
            "text" => Self::Text,
            "vertical-text" => Self::VerticalText,
            "w-resize" => Self::WResize,
            "wait" => Self::Wait,
            "zoom-in" => Self::ZoomIn,
            "zoom-out" => Self::ZoomOut,
            _ => return None,
        })
    }

    /// Parse a set/push shape name: canonical CSS names plus Kitty's xterm
    /// compatibility aliases (X11 `cursorfont.h` names and Kitty's own
    /// `arrow`/`beam`/`hand` shorthands).
    pub fn from_name(name: &str) -> Option<Self> {
        if let Some(shape) = Self::from_css_name(name) {
            return Some(shape);
        }
        Some(match name {
            "arrow" | "left_ptr" => Self::Default,
            "beam" | "xterm" | "ibeam" => Self::Text,
            "hand" | "hand2" | "pointing_hand" => Self::Pointer,
            "whats_this" | "question_arrow" => Self::Help,
            "clock" | "watch" => Self::Wait,
            "half-busy" | "left_ptr_watch" => Self::Progress,
            "tcross" => Self::Crosshair,
            "plus" | "cross" => Self::Cell,
            "fleur" | "pointer-move" => Self::Move,
            "right_side" => Self::EResize,
            "top_right_corner" => Self::NeResize,
            "top_left_corner" => Self::NwResize,
            "top_side" => Self::NResize,
            "bottom_right_corner" => Self::SeResize,
            "bottom_left_corner" => Self::SwResize,
            "bottom_side" => Self::SResize,
            "left_side" => Self::WResize,
            "sb_h_double_arrow" | "split_h" => Self::EwResize,
            "sb_v_double_arrow" | "split_v" => Self::NsResize,
            "size_bdiag" | "size-bdiag" => Self::NeswResize,
            "size_fdiag" | "size-fdiag" => Self::NwseResize,
            "zoom_in" => Self::ZoomIn,
            "zoom_out" => Self::ZoomOut,
            "dnd-link" => Self::Alias,
            "dnd-copy" => Self::Copy,
            "forbidden" | "crossed_circle" => Self::NotAllowed,
            "dnd-no-drop" => Self::NoDrop,
            "openhand" | "hand1" => Self::Grab,
            "closedhand" | "dnd-none" => Self::Grabbing,
            _ => return None,
        })
    }

    /// Whether a query name is answered with `1` (supported).
    ///
    /// Only the 30 canonical CSS names report supported, matching Kitty
    /// (`kitty/window.py:2902-2916` via `is_css_pointer_name_valid`,
    /// `kitty/glfw.c:3093-3102`). Set-only xterm aliases such as `left_ptr`
    /// answer `0` even though setting them works (`kitty/screen.c:2251-2296`;
    /// spec `pointer-shapes.rst:79-85,165-172`).
    pub fn is_supported_name(name: &str) -> bool {
        Self::from_css_name(name).is_some()
    }

    /// All 30 canonical CSS shapes in sorted wire order.
    pub const ALL_CSS: [&'static str; 30] = [
        "alias",
        "cell",
        "copy",
        "crosshair",
        "default",
        "e-resize",
        "ew-resize",
        "grab",
        "grabbing",
        "help",
        "move",
        "n-resize",
        "ne-resize",
        "nesw-resize",
        "no-drop",
        "not-allowed",
        "ns-resize",
        "nw-resize",
        "nwse-resize",
        "pointer",
        "progress",
        "s-resize",
        "se-resize",
        "sw-resize",
        "text",
        "vertical-text",
        "w-resize",
        "wait",
        "zoom-in",
        "zoom-out",
    ];
}

/// Encode one stack entry for snapshots: `None` (Kitty's `INVALID_POINTER`,
/// set by an empty `OSC 22 ;`) becomes `""`, shapes become CSS names.
pub fn stack_entry_to_snapshot(entry: &Option<PointerShape>) -> String {
    entry.map_or_else(String::new, |shape| shape.css_name().to_string())
}

/// Decode one snapshot entry, rejecting unknown names.
pub fn stack_entry_from_snapshot(name: &str) -> Option<Option<PointerShape>> {
    if name.is_empty() {
        Some(None)
    } else {
        PointerShape::from_name(name).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_vocabulary_roundtrips() {
        assert_eq!(PointerShape::ALL_CSS.len(), 30);
        for name in PointerShape::ALL_CSS {
            let shape = PointerShape::from_css_name(name).expect("CSS name must parse");
            assert_eq!(shape.css_name(), name);
            assert!(PointerShape::is_supported_name(name));
        }
    }

    #[test]
    fn xterm_aliases_map_to_css_shapes() {
        let cases = [
            ("left_ptr", "default"),
            ("arrow", "default"),
            ("xterm", "text"),
            ("ibeam", "text"),
            ("beam", "text"),
            ("hand", "pointer"),
            ("hand2", "pointer"),
            ("pointing_hand", "pointer"),
            ("clock", "wait"),
            ("watch", "wait"),
            ("tcross", "crosshair"),
            ("plus", "cell"),
            ("cross", "cell"),
            ("fleur", "move"),
            ("right_side", "e-resize"),
            ("sb_h_double_arrow", "ew-resize"),
            ("sb_v_double_arrow", "ns-resize"),
            ("size_bdiag", "nesw-resize"),
            ("size-fdiag", "nwse-resize"),
            ("zoom_in", "zoom-in"),
            ("dnd-link", "alias"),
            ("forbidden", "not-allowed"),
            ("openhand", "grab"),
            ("closedhand", "grabbing"),
        ];
        for (alias, css) in cases {
            assert_eq!(
                PointerShape::from_name(alias).unwrap().css_name(),
                css,
                "alias {alias}"
            );
            // Aliases stay settable but probe as unsupported (CSS-only `?`,
            // matching Kitty): setting works, `?alias` answers `0`.
            assert!(!PointerShape::is_supported_name(alias), "alias {alias}");
        }
    }

    #[test]
    fn malformed_names_are_rejected() {
        for name in [
            "",
            " ",
            "pointer ",
            " Pointer",
            "POINTER",
            "no_such_shape",
            "text;",
        ] {
            assert_eq!(PointerShape::from_css_name(name), None, "{name:?}");
            assert_eq!(PointerShape::from_name(name), None, "{name:?}");
            assert!(!PointerShape::is_supported_name(name));
        }
    }

    #[test]
    fn snapshot_entries_preserve_empty_slots() {
        assert_eq!(stack_entry_to_snapshot(&None), "");
        assert_eq!(
            stack_entry_to_snapshot(&Some(PointerShape::Pointer)),
            "pointer"
        );
        assert_eq!(stack_entry_from_snapshot(""), Some(None));
        assert_eq!(
            stack_entry_from_snapshot("wait"),
            Some(Some(PointerShape::Wait))
        );
        assert_eq!(stack_entry_from_snapshot("bogus"), None);
    }
}
