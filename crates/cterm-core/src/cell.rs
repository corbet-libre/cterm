//! Terminal cell types
//!
//! A cell represents one display position in the terminal grid, including its
//! extended grapheme cluster, colors, and attributes.

use crate::color::{AnsiColor, Color};
use crate::text_sizing::{Multicell, MAX_TEXT_SIZE_PAYLOAD_BYTES};
use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::sync::Arc;

/// Kitty's private-use scalar for image placements that move with text.
pub const KITTY_IMAGE_PLACEHOLDER: char = '\u{10EEEE}';

/// Maximum UTF-8 payload retained in one terminal cell.
///
/// Real-world extended grapheme clusters are generally small. Bounding the
/// exceptional case prevents an untrusted PTY from growing a single cell
/// without limit by streaming combining characters.
pub const MAX_GRAPHEME_BYTES: usize = 64;

bitflags! {
    /// Cell rendering attributes
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct CellAttrs: u16 {
        /// Bold/bright text
        const BOLD = 1 << 0;
        /// Italic text
        const ITALIC = 1 << 1;
        /// Underlined text
        const UNDERLINE = 1 << 2;
        /// Double underline
        const DOUBLE_UNDERLINE = 1 << 3;
        /// Curly underline (undercurl)
        const CURLY_UNDERLINE = 1 << 4;
        /// Dotted underline
        const DOTTED_UNDERLINE = 1 << 5;
        /// Dashed underline
        const DASHED_UNDERLINE = 1 << 6;
        /// Slowly blinking text (SGR 5)
        const BLINK = 1 << 7;
        /// Reverse video (swap fg/bg)
        const INVERSE = 1 << 8;
        /// Hidden/invisible text
        const HIDDEN = 1 << 9;
        /// Strikethrough text
        const STRIKETHROUGH = 1 << 10;
        /// Dim/faint text
        const DIM = 1 << 11;
        /// Overline
        const OVERLINE = 1 << 12;
        /// Wide character (takes 2 cells)
        const WIDE = 1 << 13;
        /// Placeholder for second cell of wide char
        const WIDE_SPACER = 1 << 14;
        /// Rapidly blinking text (SGR 6)
        const RAPID_BLINK = 1 << 15;
    }
}

impl CellAttrs {
    /// Check if either slow or rapid blinking is selected.
    pub fn has_blink(&self) -> bool {
        self.intersects(Self::BLINK | Self::RAPID_BLINK)
    }

    /// Clear both mutually exclusive blink speeds.
    pub fn clear_blink(&mut self) {
        self.remove(Self::BLINK | Self::RAPID_BLINK);
    }

    /// Check if any underline style is set
    pub fn has_underline(&self) -> bool {
        self.intersects(
            Self::UNDERLINE
                | Self::DOUBLE_UNDERLINE
                | Self::CURLY_UNDERLINE
                | Self::DOTTED_UNDERLINE
                | Self::DASHED_UNDERLINE,
        )
    }

    /// Clear all underline styles
    pub fn clear_underline(&mut self) {
        self.remove(
            Self::UNDERLINE
                | Self::DOUBLE_UNDERLINE
                | Self::CURLY_UNDERLINE
                | Self::DOTTED_UNDERLINE
                | Self::DASHED_UNDERLINE,
        );
    }
}

/// Hyperlink information (OSC 8)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hyperlink {
    /// Unique ID for the hyperlink (optional)
    pub id: Option<String>,
    /// The URI target
    pub uri: String,
}

impl Hyperlink {
    pub fn new(uri: String) -> Self {
        Self { id: None, uri }
    }

    pub fn with_id(id: String, uri: String) -> Self {
        Self { id: Some(id), uri }
    }
}

/// A single terminal cell containing one extended grapheme cluster
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The extended grapheme cluster displayed in this cell.
    text: SmolStr,
    /// Foreground color
    pub fg: Color,
    /// Background color
    pub bg: Color,
    /// Underline color (if different from fg)
    pub underline_color: Option<Color>,
    /// Cell attributes (bold, italic, etc.)
    pub attrs: CellAttrs,
    /// Hyperlink if present (shared via Arc for efficiency)
    pub hyperlink: Option<Arc<Hyperlink>>,
    /// Kitty OSC 66 layout metadata for a fixed-width or scaled text block.
    pub multicell: Option<Multicell>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: SmolStr::new_static(" "),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            attrs: CellAttrs::empty(),
            hyperlink: None,
            multicell: None,
        }
    }
}

impl Cell {
    /// Create a new cell with the given character
    pub fn new(c: char) -> Self {
        Self {
            text: SmolStr::new(c.to_string()),
            ..Default::default()
        }
    }

    /// Return the full extended grapheme cluster in this cell.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the first scalar value, primarily for character classification.
    pub fn first_char(&self) -> char {
        self.text.chars().next().unwrap_or(' ')
    }

    /// Whether this cell carries Kitty's Unicode image-placeholder scalar.
    pub fn is_kitty_image_placeholder(&self) -> bool {
        self.first_char() == KITTY_IMAGE_PLACEHOLDER
    }

    /// Return the cell content when it consists of exactly one scalar value.
    pub fn single_char(&self) -> Option<char> {
        let mut chars = self.text.chars();
        let first = chars.next()?;
        chars.next().is_none().then_some(first)
    }

    /// Replace the cell content with one scalar value.
    pub fn set_char(&mut self, c: char) {
        self.text = SmolStr::new(c.to_string());
    }

    /// Replace the cell content with a bounded UTF-8 string.
    pub fn set_text(&mut self, text: &str) {
        if text.is_empty() {
            self.text = SmolStr::new_static(" ");
            return;
        }

        let mut end = text.len().min(MAX_GRAPHEME_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text = SmolStr::new(&text[..end]);
    }

    /// Store a pre-bounded OSC 66 payload in the anchor cell.
    pub fn set_text_size_payload(&mut self, text: &str) {
        let mut end = text.len().min(MAX_TEXT_SIZE_PAYLOAD_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text = SmolStr::new(&text[..end]);
    }

    /// Append a scalar value while enforcing the per-cell memory bound.
    pub(crate) fn append_char(&mut self, c: char) -> bool {
        if self.text.len() + c.len_utf8() > MAX_GRAPHEME_BYTES {
            return false;
        }

        let mut text = String::with_capacity(self.text.len() + c.len_utf8());
        text.push_str(&self.text);
        text.push(c);
        self.text = SmolStr::new(text);
        true
    }

    /// Create an empty (space) cell
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if this cell is empty (space with default colors and no attrs)
    pub fn is_empty(&self) -> bool {
        self.text == " "
            && self.fg == Color::Default
            && self.bg == Color::Default
            && self.attrs.is_empty()
            && self.hyperlink.is_none()
            && self.multicell.is_none()
    }

    /// Check if this cell is a wide character
    pub fn is_wide(&self) -> bool {
        self.attrs.contains(CellAttrs::WIDE)
    }

    /// Check if this cell is a spacer for a wide character
    pub fn is_wide_spacer(&self) -> bool {
        self.attrs.contains(CellAttrs::WIDE_SPACER)
    }

    /// Whether this is a non-anchor cell occupied by Kitty OSC 66 text.
    pub fn is_multicell_spacer(&self) -> bool {
        self.multicell
            .as_ref()
            .is_some_and(|multicell| !multicell.is_anchor())
    }

    /// Reset cell to empty state
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reset only the SGR-addressable style, preserving character content,
    /// hyperlink, multicell layout, and wide-cell structure.
    ///
    /// This is the rectangular-attribute (DECCARA `0`) and empty-`Ps` reset:
    /// every SGR flag, the foreground/background colors, and the underline
    /// color return to default. `WIDE`/`WIDE_SPACER` are structural grid
    /// markers, not SGR state, so they survive.
    pub(crate) fn reset_sgr(&mut self) {
        const STRUCTURAL: CellAttrs = CellAttrs::WIDE.union(CellAttrs::WIDE_SPACER);
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.underline_color = None;
        self.attrs = self.attrs.intersection(STRUCTURAL);
    }

    /// Copy attributes from another cell (colors and attrs, not character)
    pub fn copy_style_from(&mut self, other: &Cell) {
        self.fg = other.fg;
        self.bg = other.bg;
        self.underline_color = other.underline_color;
        self.attrs = other.attrs;
        self.hyperlink = other.hyperlink.clone();
    }
}

/// Current terminal styling state (used when writing new characters)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub attrs: CellAttrs,
    pub hyperlink: Option<Arc<Hyperlink>>,
}

impl CellStyle {
    /// Apply this style to a cell
    pub fn apply_to(&self, cell: &mut Cell) {
        cell.fg = self.fg;
        cell.bg = self.bg;
        cell.underline_color = self.underline_color;
        cell.attrs = self.attrs;
        cell.hyperlink = self.hyperlink.clone();
    }

    /// Create a cell with this style and the given character
    pub fn create_cell(&self, c: char) -> Cell {
        Cell {
            text: SmolStr::new(c.to_string()),
            fg: self.fg,
            bg: self.bg,
            underline_color: self.underline_color,
            attrs: self.attrs,
            hyperlink: self.hyperlink.clone(),
            multicell: None,
        }
    }

    /// Reset to default style
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Mutable view of the SGR-addressable fields shared by [`Cell`] and
/// [`CellStyle`], so normal SGR and rectangular attribute changes apply the
/// exact same repertoire.
pub(crate) struct SgrTarget<'a> {
    pub fg: &'a mut Color,
    pub bg: &'a mut Color,
    pub underline_color: &'a mut Option<Color>,
    pub attrs: &'a mut CellAttrs,
    /// Hyperlink slot, present only for [`CellStyle`]. Normal SGR `0` resets
    /// the pending hyperlink; rectangular changes never touch it because
    /// OSC 8 is not an SGR attribute (matching DECFRA).
    pub hyperlink: Option<&'a mut Option<Arc<Hyperlink>>>,
}

impl<'a> SgrTarget<'a> {
    pub fn from_cell(cell: &'a mut Cell) -> Self {
        Self {
            fg: &mut cell.fg,
            bg: &mut cell.bg,
            underline_color: &mut cell.underline_color,
            attrs: &mut cell.attrs,
            hyperlink: None,
        }
    }

    pub fn from_style(style: &'a mut CellStyle) -> Self {
        Self {
            fg: &mut style.fg,
            bg: &mut style.bg,
            underline_color: &mut style.underline_color,
            attrs: &mut style.attrs,
            hyperlink: Some(&mut style.hyperlink),
        }
    }
}

/// Apply one SGR parameter sequence to a style target.
///
/// Each iterator item is one CSI parameter group: the parameter followed by
/// any colon-separated subparameters (for example `[4, 3]` for `4:3`). This
/// is exactly the grouping `vte::Params::iter` yields, so callers pass
/// `params.iter()` (normal SGR) or `params.iter().skip(4)` (rectangular
/// operations, where the first four groups are coordinates). Callers handle
/// the empty-sequence reset themselves: normal SGR resets on zero parameters,
/// rectangular operations reset on zero SGR groups after the coordinates.
pub(crate) fn apply_sgr_sequence<'a>(
    mut target: SgrTarget<'_>,
    params: impl Iterator<Item = &'a [u16]>,
) {
    let mut iter = params.peekable();

    while let Some(param) = iter.next() {
        match param {
            // Reset
            [0] => {
                *target.fg = Color::Default;
                *target.bg = Color::Default;
                *target.underline_color = None;
                *target.attrs = CellAttrs::empty();
                if let Some(hyperlink) = target.hyperlink.as_mut() {
                    **hyperlink = None;
                }
            }
            // Bold
            [1] => target.attrs.insert(CellAttrs::BOLD),
            // Dim/faint
            [2] => target.attrs.insert(CellAttrs::DIM),
            // Italic
            [3] => target.attrs.insert(CellAttrs::ITALIC),
            // Underline
            [4, style @ ..] => {
                target.attrs.clear_underline();
                match style.first().copied().unwrap_or(1) {
                    0 => {}
                    2 => target.attrs.insert(CellAttrs::DOUBLE_UNDERLINE),
                    3 => target.attrs.insert(CellAttrs::CURLY_UNDERLINE),
                    4 => target.attrs.insert(CellAttrs::DOTTED_UNDERLINE),
                    5 => target.attrs.insert(CellAttrs::DASHED_UNDERLINE),
                    _ => target.attrs.insert(CellAttrs::UNDERLINE),
                }
            }
            // Slow and rapid blink are mutually exclusive.
            [5] => {
                target.attrs.clear_blink();
                target.attrs.insert(CellAttrs::BLINK);
            }
            [6] => {
                target.attrs.clear_blink();
                target.attrs.insert(CellAttrs::RAPID_BLINK);
            }
            // Inverse
            [7] => target.attrs.insert(CellAttrs::INVERSE),
            // Hidden
            [8] => target.attrs.insert(CellAttrs::HIDDEN),
            // Strikethrough
            [9] => target.attrs.insert(CellAttrs::STRIKETHROUGH),
            // Normal intensity (not bold or dim)
            [22] => {
                target.attrs.remove(CellAttrs::BOLD);
                target.attrs.remove(CellAttrs::DIM);
            }
            // Kitty extensions: reset bold and faint independently.
            [221] => target.attrs.remove(CellAttrs::BOLD),
            [222] => target.attrs.remove(CellAttrs::DIM),
            // Not italic
            [23] => target.attrs.remove(CellAttrs::ITALIC),
            // Not underlined
            [24] => target.attrs.clear_underline(),
            // Not blinking
            [25] => target.attrs.clear_blink(),
            // Not inverse
            [27] => target.attrs.remove(CellAttrs::INVERSE),
            // Not hidden
            [28] => target.attrs.remove(CellAttrs::HIDDEN),
            // Not strikethrough
            [29] => target.attrs.remove(CellAttrs::STRIKETHROUGH),
            // Foreground colors (30-37)
            [param @ 30..=37] => {
                if let Some(color) = AnsiColor::from_index((*param - 30) as u8) {
                    *target.fg = Color::Ansi(color);
                }
            }
            // Extended foreground color, semicolon form.
            [38] => {
                let mut color_params = iter.by_ref().map(|param| param[0]);
                if let Some(color) = parse_sgr_color(&mut color_params) {
                    *target.fg = color;
                }
            }
            // Extended foreground color, colon form.
            [38, color_params @ ..] => {
                if let Some(color) = parse_colon_sgr_color(color_params) {
                    *target.fg = color;
                }
            }
            // Default foreground
            [39] => *target.fg = Color::Default,
            // Background colors (40-47)
            [param @ 40..=47] => {
                if let Some(color) = AnsiColor::from_index((*param - 40) as u8) {
                    *target.bg = Color::Ansi(color);
                }
            }
            // Extended background color, semicolon form.
            [48] => {
                let mut color_params = iter.by_ref().map(|param| param[0]);
                if let Some(color) = parse_sgr_color(&mut color_params) {
                    *target.bg = color;
                }
            }
            // Extended background color, colon form.
            [48, color_params @ ..] => {
                if let Some(color) = parse_colon_sgr_color(color_params) {
                    *target.bg = color;
                }
            }
            // Default background
            [49] => *target.bg = Color::Default,
            // Overline
            [53] => target.attrs.insert(CellAttrs::OVERLINE),
            // Not overline
            [55] => target.attrs.remove(CellAttrs::OVERLINE),
            // Underline color, semicolon form.
            [58] => {
                let mut color_params = iter.by_ref().map(|param| param[0]);
                if let Some(color) = parse_sgr_color(&mut color_params) {
                    *target.underline_color = Some(color);
                }
            }
            // Underline color, colon form.
            [58, color_params @ ..] => {
                if let Some(color) = parse_colon_sgr_color(color_params) {
                    *target.underline_color = Some(color);
                }
            }
            // Default underline color
            [59] => *target.underline_color = None,
            // Bright foreground colors (90-97)
            [param @ 90..=97] => {
                if let Some(color) = AnsiColor::from_index((*param - 90 + 8) as u8) {
                    *target.fg = Color::Ansi(color);
                }
            }
            // Bright background colors (100-107)
            [param @ 100..=107] => {
                if let Some(color) = AnsiColor::from_index((*param - 100 + 8) as u8) {
                    *target.bg = Color::Ansi(color);
                }
            }
            _ => {
                log::trace!("Unknown SGR parameter: {:?}", param);
            }
        }
    }
}

/// Parse the colon form of an SGR color while tolerating the optional color
/// space and tolerance fields accepted by foot and VTE.
fn parse_colon_sgr_color(params: &[u16]) -> Option<Color> {
    let components_start = if params.len() > 4 { 2 } else { 1 };
    let mut params =
        std::iter::once(*params.first()?).chain(params.get(components_start..)?.iter().copied());
    parse_sgr_color(&mut params)
}

fn parse_sgr_color(params: &mut dyn Iterator<Item = u16>) -> Option<Color> {
    use crate::color::Rgb;
    match params.next()? {
        5 => Some(Color::Indexed(u8::try_from(params.next()?).ok()?)),
        2 => Some(Color::Rgb(Rgb::new(
            u8::try_from(params.next()?).ok()?,
            u8::try_from(params.next()?).ok()?,
            u8::try_from(params.next()?).ok()?,
        ))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_default() {
        let cell = Cell::default();
        assert_eq!(cell.text(), " ");
        assert!(cell.is_empty());
    }

    #[test]
    fn test_cell_not_empty() {
        let mut cell = Cell::new('A');
        assert!(!cell.is_empty());

        cell = Cell::default();
        cell.fg = Color::Ansi(crate::color::AnsiColor::Red);
        assert!(!cell.is_empty());
    }

    #[test]
    fn test_cell_attrs() {
        let mut attrs = CellAttrs::BOLD | CellAttrs::UNDERLINE;
        assert!(attrs.contains(CellAttrs::BOLD));
        assert!(attrs.has_underline());

        attrs.clear_underline();
        assert!(!attrs.has_underline());
        assert!(attrs.contains(CellAttrs::BOLD));
    }

    #[test]
    fn test_cell_style_apply() {
        let style = CellStyle {
            fg: Color::Ansi(crate::color::AnsiColor::Red),
            attrs: CellAttrs::BOLD,
            ..Default::default()
        };

        let cell = style.create_cell('X');
        assert_eq!(cell.text(), "X");
        assert_eq!(cell.fg, Color::Ansi(crate::color::AnsiColor::Red));
        assert!(cell.attrs.contains(CellAttrs::BOLD));
    }
}
