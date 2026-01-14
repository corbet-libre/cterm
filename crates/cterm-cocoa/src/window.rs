//! Main window implementation for macOS
//!
//! Handles NSWindow creation and management.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use cterm_app::config::Config;
use cterm_app::shortcuts::ShortcutManager;
use cterm_ui::theme::Theme;

use crate::tab_bar::TabBar;
use crate::terminal_view::TerminalView;

/// Tab entry tracking terminal and its ID
struct TabEntry {
    id: u64,
    title: String,
    terminal: Retained<TerminalView>,
}

/// Window state stored in ivars
pub struct CtermWindowIvars {
    config: Config,
    theme: Theme,
    shortcuts: ShortcutManager,
    tabs: RefCell<Vec<TabEntry>>,
    next_tab_id: RefCell<u64>,
    tab_bar: RefCell<Option<Retained<TabBar>>>,
    content_view: RefCell<Option<Retained<TerminalView>>>,
}

define_class!(
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "CtermWindow"]
    #[ivars = CtermWindowIvars]
    pub struct CtermWindow;

    unsafe impl NSObjectProtocol for CtermWindow {}

    unsafe impl NSWindowDelegate for CtermWindow {
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            log::debug!("Window became key");
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            log::debug!("Window resigned key");
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            log::debug!("Window will close");
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            log::debug!("Window did resize");
            // Update terminal dimensions
            if let Some(terminal) = self.ivars().content_view.borrow().as_ref() {
                terminal.handle_resize();
            }
        }
    }
);

impl CtermWindow {
    pub fn new(mtm: MainThreadMarker, config: &Config, theme: &Theme) -> Retained<Self> {
        // Calculate initial window size for 80x24 terminal
        let cell_width = config.appearance.font.size * 0.6; // Approximate
        let cell_height = config.appearance.font.size * 1.2;
        let width = cell_width * 80.0 + 20.0;
        let height = cell_height * 24.0 + 60.0; // Extra for tab bar

        let content_rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(width, height));

        let style_mask = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;

        // Allocate and initialize
        let this = mtm.alloc::<Self>();
        let this = this.set_ivars(CtermWindowIvars {
            config: config.clone(),
            theme: theme.clone(),
            shortcuts: ShortcutManager::from_config(&config.shortcuts),
            tabs: RefCell::new(Vec::new()),
            next_tab_id: RefCell::new(0),
            tab_bar: RefCell::new(None),
            content_view: RefCell::new(None),
        });

        let this: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: content_rect,
                styleMask: style_mask,
                backing: 2u64, // NSBackingStoreBuffered
                defer: false
            ]
        };

        // Set window title
        this.setTitle(&NSString::from_str("cterm"));

        // Set minimum size
        this.setMinSize(NSSize::new(400.0, 200.0));

        // Set self as delegate
        this.setDelegate(Some(ProtocolObject::from_ref(&*this)));

        // Create the terminal view
        let terminal = TerminalView::new(mtm, config, theme);
        this.setContentView(Some(&terminal));
        *this.ivars().content_view.borrow_mut() = Some(terminal.clone());

        // Store terminal in tabs
        let tab_id = {
            let mut id = this.ivars().next_tab_id.borrow_mut();
            let current = *id;
            *id += 1;
            current
        };

        this.ivars().tabs.borrow_mut().push(TabEntry {
            id: tab_id,
            title: "Terminal".to_string(),
            terminal,
        });

        this
    }

    /// Create a new tab
    pub fn new_tab(&self) {
        let mtm = MainThreadMarker::from(self);

        let terminal = TerminalView::new(mtm, &self.ivars().config, &self.ivars().theme);

        let tab_id = {
            let mut id = self.ivars().next_tab_id.borrow_mut();
            let current = *id;
            *id += 1;
            current
        };

        self.ivars().tabs.borrow_mut().push(TabEntry {
            id: tab_id,
            title: "Terminal".to_string(),
            terminal: terminal.clone(),
        });

        // Switch to new tab
        self.setContentView(Some(&terminal));
        *self.ivars().content_view.borrow_mut() = Some(terminal);

        log::info!("Created new tab {}", tab_id);
    }

    /// Close current tab
    pub fn close_tab(&self) {
        let tabs = self.ivars().tabs.borrow();
        if tabs.len() <= 1 {
            // Last tab, close window
            drop(tabs);
            self.close();
            return;
        }
        drop(tabs);

        // Remove current tab and switch to previous
        let mut tabs = self.ivars().tabs.borrow_mut();
        if !tabs.is_empty() {
            tabs.pop();
            if let Some(last) = tabs.last() {
                let terminal = last.terminal.clone();
                drop(tabs);
                self.setContentView(Some(&terminal));
                *self.ivars().content_view.borrow_mut() = Some(terminal);
            }
        }
    }

    /// Get config reference
    pub fn config(&self) -> &Config {
        &self.ivars().config
    }

    /// Get theme reference
    pub fn theme(&self) -> &Theme {
        &self.ivars().theme
    }
}
