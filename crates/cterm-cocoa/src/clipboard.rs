//! Clipboard implementation for macOS
//!
//! Handles copy/paste operations using NSPasteboard.

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// Get text from the system clipboard
pub fn get_text() -> Option<String> {
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };

    // Read string directly from pasteboard
    unsafe { pasteboard.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string())
}

/// Set text to the system clipboard
pub fn set_text(text: &str) {
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };

    // Clear and set new content
    unsafe {
        pasteboard.clearContents();
        pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString);
    }
}

/// Clipboard wrapper implementing cterm-ui traits
pub struct Clipboard;

impl cterm_ui::traits::Clipboard for Clipboard {
    fn get_text(&self) -> Option<String> {
        get_text()
    }

    fn set_text(&mut self, text: &str) {
        set_text(text);
    }

    fn get_primary(&self) -> Option<String> {
        // macOS doesn't have primary selection like X11
        // Return regular clipboard content
        get_text()
    }

    fn set_primary(&mut self, text: &str) {
        // macOS doesn't have primary selection
        // Set to regular clipboard
        set_text(text);
    }
}
