//! cterm - A high-performance terminal emulator
//!
//! This is the main entry point that selects the appropriate UI backend
//! based on the target platform.

fn main() {
    #[cfg(target_os = "macos")]
    {
        cterm_cocoa::run();
    }

    #[cfg(not(target_os = "macos"))]
    {
        cterm_gtk::run();
    }
}
