//! Click-to-move-cursor in shell prompts (Kitty `click_events` mechanism).
//!
//! When the shell marks its prompt with OSC 133 `A` parameters, a click
//! inside the active prompt input moves the text cursor instead of
//! starting a selection:
//!
//! - `click_events=1|2`: the shell handles clicks itself; the terminal
//!   forwards an SGR press (`1` absolute rows, `2` rows relative to the
//!   prompt top, `kitty/mouse.c:785-795`, `kitty/screen.c:2767-2770`).
//! - otherwise: the terminal synthesizes Left/Right presses that walk the
//!   cursor to the click (`screen_fake_move_cursor_to_position`,
//!   `kitty/screen.c:2773-2813`), or a special key when the shell asked
//!   for `special_key=1`.
//! - `redraw=0` and `k=s` (secondary prompt on this line) are parsed and
//!   stored for resize handling; they do not affect clicking.
//!
//! Flags are sticky per screen (set-only tokens, cleared only by a full
//! reset), unknown `A` parameters are ignored, and a click at or above the
//! prompt top never acts (`kitty/mouse.c:781-783`). Everything here is
//! renderer-independent and headless-testable; wiring mouse presses into
//! this lives in the native frontends.

use crate::screen::Screen;

/// Shell-declared click behavior for OSC 133 prompt markers.
///
/// Mirrors Kitty's per-screen `prompt_settings` (`kitty/screen.c:231`,
/// `parse_prompt_mark`): set-only tokens, sticky until reset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShellPromptClickSettings {
    /// Shell handles clicks itself (`click_events=1|2`).
    pub supports_click_events: bool,
    /// Click rows are relative to the prompt top (`click_events=2`).
    pub relative_click_events: bool,
    /// Synthesize the shell-bound special key instead of arrows.
    pub use_special_key: bool,
    /// Shell redraws prompts itself on resize (`redraw=0` clears this).
    pub redraws_prompts: bool,
    /// Secondary (PS2) prompt starts on the marked line (`k=s`).
    pub secondary_on_current_line: bool,
}

impl ShellPromptClickSettings {
    /// Power-on defaults: the shell redraws prompts; nothing else is on.
    pub fn new() -> Self {
        Self {
            redraws_prompts: true,
            ..Self::default()
        }
    }

    /// Apply one `OSC 133;A` parameter token (`click_events=1|2`,
    /// `special_key=1`, `redraw=0`, `k=s`); unknown tokens are ignored.
    pub fn apply_token(&mut self, token: &str) {
        match token {
            "click_events=1" => {
                self.supports_click_events = true;
                self.relative_click_events = false;
            }
            "click_events=2" => {
                self.supports_click_events = true;
                self.relative_click_events = true;
            }
            "special_key=1" => self.use_special_key = true,
            "redraw=0" => self.redraws_prompts = false,
            "k=s" => self.secondary_on_current_line = true,
            _ => {}
        }
    }
}

/// What a click inside the active prompt input should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptClickAction {
    /// Forward an SGR left-press at the (possibly prompt-relative) cell.
    ForwardPress { col: usize, row: usize },
    /// Walk the cursor with arrow presses (at most one direction nonzero).
    MoveCursor { left: u32, right: u32 },
    /// Let the shell-bound special key move the cursor.
    SpecialKey,
}

impl Screen {
    /// Row of the active shell prompt, mirroring
    /// `screen_cursor_at_a_shell_prompt` (`kitty/screen.c:2753-2764`):
    /// alternate screen, hidden cursor, and cursor past the last row fail;
    /// scanning up, a prompt marker wins, a closed command-output boundary
    /// fails, anything else is skipped.
    pub fn cursor_at_shell_prompt(&self) -> Option<usize> {
        if self.modes.alternate_screen || !self.modes.show_cursor {
            return None;
        }
        let height = self.height();
        if self.cursor.row >= height {
            return None;
        }
        for row in (0..=self.cursor.row).rev() {
            let Some(grid_row) = self.grid().row(row) else {
                continue;
            };
            if grid_row.shell_integration.prompt_marker {
                return Some(row);
            }
            if grid_row.shell_integration.command_end.is_some() {
                return None;
            }
        }
        None
    }

    /// Decide what a primary click at (`click_col`, `click_row`) does.
    ///
    /// Returns `None` when normal click behavior (selection) applies:
    /// no active prompt, or the click is above the prompt top.
    pub fn prompt_click_action(
        &self,
        click_col: usize,
        click_row: usize,
    ) -> Option<PromptClickAction> {
        let prompt_row = self.cursor_at_shell_prompt()?;
        if prompt_row > click_row {
            return None;
        }
        let settings = self.prompt_click_settings();
        if settings.supports_click_events {
            let row = if settings.relative_click_events {
                click_row.saturating_sub(prompt_row)
            } else {
                click_row
            };
            return Some(PromptClickAction::ForwardPress {
                col: click_col,
                row,
            });
        }
        if settings.use_special_key {
            return Some(PromptClickAction::SpecialKey);
        }
        let (left, right) = self.fake_move_counts(click_col, click_row);
        Some(PromptClickAction::MoveCursor { left, right })
    }

    /// Count Left/Right presses walking the cursor to the click, mirroring
    /// `screen_fake_move_cursor_to_position` (`kitty/screen.c:2788-2813`)
    /// with one deliberate deviation: cterm cells cannot distinguish a
    /// never-written blank (`" "`) from a written space, so every
    /// non-spacer cell in range costs one press instead of Kitty's
    /// stop-after-first-trailing-gap. The cursor lands identically —
    /// shells ignore past-end arrows — at the cost of extra no-op presses
    /// past the input end. Scan order decides the direction; wide and
    /// multicell continuations ride free, so one press crosses a
    /// dual-width char.
    fn fake_move_counts(&self, click_col: usize, click_row: usize) -> (u32, u32) {
        let (cursor_col, cursor_row) = (self.cursor.col, self.cursor.row);
        let height = self.height();
        // Scan order decides the direction; the whole walk uses one key.
        let leftward =
            click_row < cursor_row || (click_row == cursor_row && click_col < cursor_col);
        let (first_x, first_y, end_x, end_y) = if leftward {
            (click_col, click_row, cursor_col, cursor_row)
        } else {
            (cursor_col, cursor_row, click_col, click_row)
        };
        let mut count: u32 = 0;
        // Clamp to the viewport so scrollback clicks cannot loop.
        let end_y = end_y.min(height.saturating_sub(1));
        let mut y = first_y.min(end_y);
        loop {
            let x_start = if y == first_y { first_x } else { 0 };
            let x_limit = if y == end_y {
                end_x.min(self.width())
            } else {
                self.width()
            };
            count += self.fake_move_row_count(y, x_start.min(x_limit), x_limit);
            if y >= end_y || y + 1 >= height {
                break;
            }
            y += 1;
        }
        if leftward {
            (count, 0)
        } else {
            (0, count)
        }
    }

    /// Arrow presses to cross one row segment `[x_start, x_limit)`: every
    /// non-spacer cell costs one press.
    fn fake_move_row_count(&self, row: usize, x_start: usize, x_limit: usize) -> u32 {
        let Some(grid_row) = self.grid().row(row) else {
            return 0;
        };
        let mut count: u32 = 0;
        let mut x = x_start;
        while x < x_limit {
            let Some(cell) = grid_row.get(x) else {
                break;
            };
            if !cell.is_wide_spacer() && !cell.is_multicell_spacer() {
                count += 1;
            }
            x += 1;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::screen::ScreenConfig;

    fn screen_with_text(rows: &[&str]) -> Screen {
        // Double the char count so wide cells never wrap mid-fixture.
        let width = rows
            .iter()
            .map(|row| row.chars().count() * 2)
            .max()
            .unwrap_or(1)
            .max(1);
        let mut screen = Screen::new(width, rows.len(), ScreenConfig::default());
        let mut parser = Parser::new();
        for (index, text) in rows.iter().enumerate() {
            if index > 0 {
                parser.parse(&mut screen, b"\r\n");
            }
            parser.parse(&mut screen, text.as_bytes());
        }
        screen.cursor.col = 0;
        screen.cursor.row = 0;
        screen
    }

    #[test]
    fn prompt_settings_tokens_are_sticky_and_unknown_ignored() {
        let mut settings = ShellPromptClickSettings::new();
        assert!(settings.redraws_prompts);
        assert!(!settings.supports_click_events);
        settings.apply_token("click_events=2");
        settings.apply_token("special_key=1");
        settings.apply_token("nonsense=x");
        settings.apply_token("click_events=banana");
        assert!(settings.supports_click_events);
        assert!(settings.relative_click_events);
        assert!(settings.use_special_key);
        assert!(settings.redraws_prompts);
        settings.apply_token("redraw=0");
        assert!(!settings.redraws_prompts);
        settings.apply_token("k=s");
        assert!(settings.secondary_on_current_line);
    }

    #[test]
    fn at_prompt_detection_respects_output_boundaries() {
        let mut screen = screen_with_text(&["$ cmd", "output", "$ "]);
        // No markers yet: not at a prompt.
        assert_eq!(screen.cursor_at_shell_prompt(), None);
        screen.cursor.row = 2;
        // Prompt marker on the input row: at prompt even with output above.
        screen
            .grid_mut()
            .row_mut(2)
            .unwrap()
            .shell_integration
            .prompt_marker = true;
        assert_eq!(screen.cursor_at_shell_prompt(), Some(2));
        // A closed output boundary between cursor and marker fails,
        // mirroring Kitty's OUTPUT_START stop.
        screen
            .grid_mut()
            .row_mut(1)
            .unwrap()
            .shell_integration
            .command_end = Some(6);
        screen
            .grid_mut()
            .row_mut(2)
            .unwrap()
            .shell_integration
            .prompt_marker = false;
        assert_eq!(screen.cursor_at_shell_prompt(), None);
    }

    #[test]
    fn click_above_prompt_top_falls_through_to_selection() {
        let mut screen = screen_with_text(&["old", "$ cmd"]);
        screen.cursor.row = 1;
        screen.cursor.col = 5;
        screen
            .grid_mut()
            .row_mut(1)
            .unwrap()
            .shell_integration
            .prompt_marker = true;
        // Click on row 0 is above the prompt top (row 1): no action.
        assert_eq!(screen.prompt_click_action(2, 0), None);
        // Click on the prompt row itself acts.
        assert!(screen.prompt_click_action(2, 1).is_some());
    }

    #[test]
    fn click_events_forward_sgr_press_with_relative_rows() {
        let mut screen = screen_with_text(&["$ cmd", "more"]);
        screen.cursor.row = 1;
        screen.cursor.col = 5;
        screen
            .grid_mut()
            .row_mut(0)
            .unwrap()
            .shell_integration
            .prompt_marker = true;
        screen
            .prompt_click_settings_mut()
            .apply_token("click_events=1");
        assert_eq!(
            screen.prompt_click_action(3, 1),
            Some(PromptClickAction::ForwardPress { col: 3, row: 1 })
        );
        screen
            .prompt_click_settings_mut()
            .apply_token("click_events=2");
        // Relative rows count from the prompt top (row 0 here).
        assert_eq!(
            screen.prompt_click_action(3, 1),
            Some(PromptClickAction::ForwardPress { col: 3, row: 1 })
        );
        assert_eq!(
            screen.prompt_click_action(3, 0),
            Some(PromptClickAction::ForwardPress { col: 3, row: 0 })
        );
    }

    #[test]
    fn fake_move_counts_single_direction_with_wide_chars() {
        // "$ l中s": 中 occupies two cells but costs one press, like Kitty.
        let mut screen = screen_with_text(&["$ l中s"]);
        screen.cursor.row = 0;
        screen.cursor.col = 0;
        screen
            .grid_mut()
            .row_mut(0)
            .unwrap()
            .shell_integration
            .prompt_marker = true;
        // Click on the wide char: $, space, l.
        assert_eq!(
            screen.prompt_click_action(3, 0),
            Some(PromptClickAction::MoveCursor { left: 0, right: 3 })
        );
        // Click past the end counts every non-spacer cell (documented
        // overshoot: harmless no-op arrows, never an undershoot).
        assert_eq!(
            screen.prompt_click_action(9, 0),
            Some(PromptClickAction::MoveCursor { left: 0, right: 8 })
        );
        // Click back home from col 5: s, 中 (one press), l, space, $
        // — four presses, since one arrow crosses the dual-width char.
        screen.cursor.col = 5;
        assert_eq!(
            screen.prompt_click_action(0, 0),
            Some(PromptClickAction::MoveCursor { left: 4, right: 0 })
        );
    }
}
