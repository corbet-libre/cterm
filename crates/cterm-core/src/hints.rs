//! Keyboard-driven hints (kitty-hints / QuickSelect semantics, core half).
//!
//! Matches selectable text (URLs, file paths) across scrollback and viewport
//! with cell-accurate coordinates and assigns short home-row labels, so a
//! native overlay can offer open/copy/paste without touching the mouse.
//! Rendering the labels and acting on a choice lives in the native
//! frontends; this module only finds and names candidates.
//!
//! Matching is deliberately conservative: bounded regexes over one visual
//! row at a time (no multi-line URLs), trailing punctuation trimmed, and a
//! hard cap on candidates so hostile output cannot blow up the overlay.

use crate::grid::Row;
use crate::screen::Screen;

/// What kind of selectable text a hint points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    /// `http(s)://`, `file://`, or `www.` URLs.
    Url,
    /// Absolute or home-relative file paths.
    FilePath,
}

/// One labeled hint candidate in cell coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintMatch {
    /// Absolute line index (0 = oldest scrollback line).
    pub line: usize,
    /// Cell column where the match starts (wide spacers excluded).
    pub col: usize,
    /// Length of the match in cells.
    pub len: usize,
    /// Matched text.
    pub text: String,
    /// What the text looks like.
    pub kind: HintKind,
    /// Keyboard label assigned in reading order (`a`, `s`, … then pairs).
    pub label: String,
}

/// Home-row label alphabet, shortest labels first.
const LABEL_ALPHABET: &[u8] = b"asdfghjkl";

/// Maximum hint candidates per collection (bounded overlay input).
pub const MAX_HINT_MATCHES: usize = 256;

/// Assign reading-order labels: single letters first, then pairs.
pub fn assign_hint_labels(count: usize) -> Vec<String> {
    let single = LABEL_ALPHABET.len();
    let mut labels: Vec<String> = LABEL_ALPHABET
        .iter()
        .take(count)
        .map(|byte| (*byte as char).to_string())
        .collect();
    // Pairs cycle the second letter fastest (`aa`, `as`, …),
    // extending without bound for pathological counts.
    let mut pair = 0;
    while labels.len() < count {
        let first = LABEL_ALPHABET[(pair / single) % single] as char;
        let second = LABEL_ALPHABET[pair % single] as char;
        labels.push(format!("{first}{second}"));
        pair += 1;
    }
    labels
}

/// Collect hint candidates of the given kinds over scrollback and viewport.
///
/// Results arrive in reading order (top to bottom, left to right) with
/// labels assigned; at most [`MAX_HINT_MATCHES`] are returned.
pub fn collect_hints(screen: &Screen, kinds: &[HintKind]) -> Vec<HintMatch> {
    let mut matches = Vec::new();
    if kinds.is_empty() {
        return matches;
    }
    let mut text = String::new();
    let mut map = Vec::new();
    let mut rows: Vec<(usize, &Row)> = screen.scrollback().iter().enumerate().collect();
    let base = screen.scrollback().len();
    for row_idx in 0..screen.height() {
        if let Some(row) = screen.grid().row(row_idx) {
            rows.push((base + row_idx, row));
        }
    }
    for (line, row) in rows {
        Screen::row_text_with_cell_map(row, &mut text, &mut map);
        for kind in kinds {
            match kind {
                HintKind::Url => collect_url_hints(&text, &map, line, &mut matches),
                HintKind::FilePath => collect_path_hints(&text, &map, line, &mut matches),
            }
            if matches.len() >= MAX_HINT_MATCHES {
                break;
            }
        }
        if matches.len() >= MAX_HINT_MATCHES {
            break;
        }
    }
    matches.truncate(MAX_HINT_MATCHES);
    matches.sort_by_key(|m| (m.line, m.col));
    let labels = assign_hint_labels(matches.len());
    for (m, label) in matches.iter_mut().zip(labels) {
        m.label = label;
    }
    matches
}

/// Trailing punctuation no URL or path ever ends with.
fn trim_trailing_punctuation(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"', '>', '|'])
}

/// Convert a byte range in row text to a cell span, like find results.
fn cell_span(map: &[u32], byte_start: usize, byte_len: usize) -> Option<(usize, usize)> {
    if map.is_empty() {
        return None;
    }
    let start = map[byte_start.min(map.len() - 1)] as usize;
    let end_byte = (byte_start + byte_len).min(map.len());
    let end = if end_byte > byte_start {
        map[end_byte - 1] as usize + 1
    } else {
        start
    };
    Some((start, end.saturating_sub(start)))
}

fn push_hint(
    map: &[u32],
    line: usize,
    byte_start: usize,
    byte_len: usize,
    text: &str,
    kind: HintKind,
    matches: &mut Vec<HintMatch>,
) {
    let end = (byte_start + byte_len).min(text.len());
    let trimmed = trim_trailing_punctuation(&text[byte_start..end]);
    if trimmed.is_empty() {
        return;
    }
    let Some((col, len)) = cell_span(map, byte_start, trimmed.len()) else {
        return;
    };
    if len == 0 {
        return;
    }
    matches.push(HintMatch {
        line,
        col,
        len,
        text: trimmed.to_string(),
        kind,
        label: String::new(),
    });
}

/// Jennings-style URL matcher over one row: `http(s)://`, `file://`, `www.`.
fn collect_url_hints(text: &str, map: &[u32], line: usize, matches: &mut Vec<HintMatch>) {
    for prefix in ["https://", "http://", "file://", "www."] {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(prefix) {
            let start = search_from + pos;
            let mut end = start;
            for (index, ch) in text[start..].char_indices() {
                if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`' | '|') {
                    break;
                }
                end = start + index + ch.len_utf8();
            }
            if end > start {
                push_hint(map, line, start, end - start, text, HintKind::Url, matches);
            }
            search_from = end.max(start + 1);
            if search_from >= text.len() {
                break;
            }
        }
    }
}

/// File-path matcher over one row: absolute or `~`-rooted runs containing
/// a slash, drawn from filename characters.
fn collect_path_hints(text: &str, map: &[u32], line: usize, matches: &mut Vec<HintMatch>) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let rooted = bytes[index] == b'/'
            || (bytes[index] == b'~' && bytes.get(index + 1).is_some_and(|next| *next == b'/'));
        if !rooted {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < bytes.len() {
            let ch = bytes[end] as char;
            if ch.is_ascii_alphanumeric()
                || matches!(ch, '/' | '.' | '-' | '_' | '~' | '+' | ':' | '@' | '=')
            {
                end += 1;
            } else {
                break;
            }
        }
        // Require a real path shape: a slash beyond the root.
        let candidate = &text[index..end];
        if candidate.contains('/') && candidate.len() > 2 {
            push_hint(
                map,
                line,
                index,
                end - index,
                text,
                HintKind::FilePath,
                matches,
            );
        }
        index = end.max(index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen as CoreScreen;
    use crate::screen::ScreenConfig;

    fn screen_with_lines(lines: &[&str]) -> CoreScreen {
        let width = lines
            .iter()
            .map(|line| line.chars().count() * 2)
            .max()
            .unwrap_or(8)
            .max(8);
        let mut screen = CoreScreen::new(width, lines.len().max(1), ScreenConfig::default());
        let mut parser = crate::parser::Parser::new();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                parser.parse(&mut screen, b"\r\n");
            }
            parser.parse(&mut screen, line.as_bytes());
        }
        screen
    }

    #[test]
    fn labels_run_singles_then_pairs() {
        let labels = assign_hint_labels(11);
        assert_eq!(&labels[..9], &["a", "s", "d", "f", "g", "h", "j", "k", "l"]);
        assert_eq!(&labels[9..], &["aa", "as"]);
    }

    #[test]
    fn urls_match_with_trailing_punctuation_trimmed() {
        let screen = screen_with_lines(&["see https://example.com/a.b, then"]);
        let hints = collect_hints(&screen, &[HintKind::Url]);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].text, "https://example.com/a.b");
        assert_eq!(hints[0].kind, HintKind::Url);
        assert_eq!(hints[0].label, "a");
        assert_eq!(hints[0].col, 4);
    }

    #[test]
    fn file_paths_match_absolute_and_home_rooted() {
        let screen = screen_with_lines(&["edit /etc/hosts and ~/notes/todo.md!"]);
        let hints = collect_hints(&screen, &[HintKind::FilePath]);
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].text, "/etc/hosts");
        assert_eq!(hints[1].text, "~/notes/todo.md");
        assert!(hints[0].col < hints[1].col);
        assert_eq!(hints[1].label, "s");
    }

    #[test]
    fn matches_cap_at_the_documented_bound() {
        let line = (0..300).map(|n| format!("/f{n} ")).collect::<String>();
        let screen = screen_with_lines(&[&line]);
        let hints = collect_hints(&screen, &[HintKind::FilePath, HintKind::Url]);
        assert_eq!(hints.len(), MAX_HINT_MATCHES);
    }
}
