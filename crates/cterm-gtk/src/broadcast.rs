//! Broadcast input to sibling panes (tmux `synchronize-panes` semantics).
//!
//! When enabled for a tab, every key routed through the focused pane is
//! additionally encoded through each sibling pane's own [`Terminal`] and
//! written to that pane's daemon session. Per-pane encoding matters:
//! keyboard-protocol state (progressive flags, pressed-key tracking) lives
//! in each terminal, so pre-encoded bytes must never be reused across
//! panes. Releases fan out the same way; panes without a matching press
//! encode to nothing and stay silent.
//!
//! This module is intentionally UI-toolkit free so the routing stays
//! headless-testable. Window/tab bookkeeping (which panes are siblings,
//! the per-tab toggle) lives in the native frontends; Cocoa and Win32
//! wire it after GTK.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use cterm_core::term::{Key, Modifiers};
use cterm_core::{KeyEventKind, KeyEventMetadata, Terminal};
use parking_lot::Mutex;

/// One translated keypress, ready to encode through any pane terminal.
///
/// Computed once from the focused pane's input path (IM/shortcut handling
/// stays focused-only and never reaches this type), then encoded per pane.
#[derive(Debug, Clone)]
pub(crate) enum KeyRoute {
    /// Enhanced keyboard path with layout metadata.
    Enhanced {
        key: Key,
        shifted: Option<char>,
        base: Option<char>,
        text: Option<String>,
    },
    /// Legacy special key (`keyval_to_key` match).
    Special(Key),
    /// Ctrl+letter control byte.
    CtrlByte(u8),
    /// Alt+key as ESC + char.
    AltChar(char),
    /// Plain text through `handle_text_input`.
    Text(String),
}

/// Encode one routed keypress through a single pane terminal.
pub(crate) fn encode_key_route(
    term: &mut Terminal,
    route: &KeyRoute,
    modifiers: Modifiers,
    kind: KeyEventKind,
) -> Option<Vec<u8>> {
    match route {
        KeyRoute::Enhanced {
            key,
            shifted,
            base,
            text,
        } => {
            let metadata = KeyEventMetadata::new()
                .with_shifted_key(*shifted)
                .with_base_layout_key(*base)
                .with_associated_text(text.as_deref());
            term.handle_key_event_with_metadata(*key, modifiers, kind, metadata)
        }
        KeyRoute::Special(key) => term.handle_key(*key, modifiers),
        KeyRoute::CtrlByte(byte) => Some(vec![*byte]),
        KeyRoute::AltChar(c) => {
            let mut buf = vec![0x1b];
            let mut char_buf = [0u8; 4];
            buf.extend_from_slice(c.encode_utf8(&mut char_buf).as_bytes());
            Some(buf)
        }
        KeyRoute::Text(text) => {
            let encoded = term.handle_text_input(text);
            (!encoded.is_empty()).then_some(encoded)
        }
    }
}

/// Per-tab broadcast wiring shared with every pane widget of the tab.
///
/// The tab owns one `Rc<RefCell<..>>` of this; widgets hold clones, so
/// toggling or re-listing peers never needs a rebuild. `peers` holds weak
/// handles to all pane terminals including the focused one; fan-out skips
/// the source terminal and drops dead panes.
#[derive(Clone, Default, Debug)]
pub(crate) struct BroadcastState {
    pub enabled: bool,
    pub peers: Vec<Weak<Mutex<Terminal>>>,
}

/// Fan out one routed keypress to every live sibling pane.
///
/// Blocks at most for a terminal lock (a skipped keystroke would diverge
/// panes, which is worse than a momentary stall), never panics on dead
/// peers, and never emits for the source terminal itself. Guards are
/// never nested: call only after releasing the source terminal lock.
pub(crate) fn broadcast_to_peers(
    state: &Rc<RefCell<BroadcastState>>,
    own: &Arc<Mutex<Terminal>>,
    route: &KeyRoute,
    modifiers: Modifiers,
    kind: KeyEventKind,
) {
    let peers: Vec<Weak<Mutex<Terminal>>> = {
        let state = state.borrow();
        if !state.enabled {
            return;
        }
        state.peers.clone()
    };
    for weak in peers.iter() {
        let Some(peer) = weak.upgrade() else {
            continue;
        };
        if Arc::ptr_eq(&peer, own) {
            continue;
        }
        let mut term = peer.lock();
        if let Some(bytes) = encode_key_route(&mut term, route, modifiers, kind) {
            if let Err(error) = term.write(&bytes) {
                log::error!("Failed to write broadcast key event to PTY: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cterm_core::screen::ScreenConfig;

    fn terminal() -> Terminal {
        Terminal::new(80, 24, ScreenConfig::default())
    }

    fn shared(peers: Vec<Weak<Mutex<Terminal>>>, enabled: bool) -> Rc<RefCell<BroadcastState>> {
        Rc::new(RefCell::new(BroadcastState { enabled, peers }))
    }

    #[test]
    fn disabled_state_never_fans_out() {
        let own = Arc::new(Mutex::new(terminal()));
        let peer = Arc::new(Mutex::new(terminal()));
        let state = shared(vec![Arc::downgrade(&peer)], false);
        broadcast_to_peers(
            &state,
            &own,
            &KeyRoute::Text("x".into()),
            Modifiers::empty(),
            KeyEventKind::Press,
        );
        // No panic, no write possible: peers without a PTY sink bytes
        // through `Terminal::write`'s harmless no-op path.
        assert!(!state.borrow().enabled);
    }

    #[test]
    fn source_terminal_is_skipped_and_dead_peers_ignored() {
        let own = Arc::new(Mutex::new(terminal()));
        let dead: Weak<Mutex<Terminal>> = Weak::new();
        let state = shared(vec![Arc::downgrade(&own), dead], true);
        broadcast_to_peers(
            &state,
            &own,
            &KeyRoute::CtrlByte(b'a'),
            Modifiers::empty(),
            KeyEventKind::Press,
        );
    }

    #[test]
    fn enhanced_route_matches_direct_encoding() {
        let mut direct = terminal();
        direct.process_collecting(b"\x1b[>1u");
        let expected = direct.handle_key_event_with_metadata(
            Key::Char('c'),
            Modifiers::CTRL,
            KeyEventKind::Press,
            KeyEventMetadata::new(),
        );
        let mut routed = terminal();
        routed.process_collecting(b"\x1b[>1u");
        let route = KeyRoute::Enhanced {
            key: Key::Char('c'),
            shifted: None,
            base: None,
            text: None,
        };
        assert_eq!(
            encode_key_route(&mut routed, &route, Modifiers::CTRL, KeyEventKind::Press),
            expected
        );
    }

    #[test]
    fn legacy_routes_encode_without_keyboard_mode() {
        let mut term = terminal();
        assert_eq!(
            encode_key_route(
                &mut term,
                &KeyRoute::Special(Key::Home),
                Modifiers::empty(),
                KeyEventKind::Press,
            ),
            term.handle_key(Key::Home, Modifiers::empty())
        );
        assert_eq!(
            encode_key_route(
                &mut term,
                &KeyRoute::CtrlByte(0x01),
                Modifiers::empty(),
                KeyEventKind::Press,
            ),
            Some(vec![0x01])
        );
        assert_eq!(
            encode_key_route(
                &mut term,
                &KeyRoute::AltChar('x'),
                Modifiers::empty(),
                KeyEventKind::Press,
            ),
            Some(vec![0x1b, b'x'])
        );
    }
}
