//! Upgrade receiver - handles receiving state from the old process during seamless upgrade
//!
//! This module is used when cterm is started with --upgrade-receiver flag.
//! It connects to the old process via Unix socket, receives the terminal state
//! and PTY file descriptors, then reconstructs the windows and tabs.

use cterm_app::upgrade::UpgradeState;
use cterm_core::fd_passing;
use gtk4::glib;
use std::io::Write;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Maximum size of the state buffer (64MB)
const MAX_STATE_SIZE: usize = 64 * 1024 * 1024;

/// Maximum number of file descriptors to receive
const MAX_FDS: usize = 256;

/// Run the upgrade receiver
///
/// This function:
/// 1. Connects to the Unix socket at the given path
/// 2. Receives the upgrade state and PTY file descriptors
/// 3. Sends acknowledgment
/// 4. Reconstructs the GTK application with the received state
pub fn run_receiver(socket_path: &Path) -> glib::ExitCode {
    match receive_and_reconstruct(socket_path) {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("Upgrade receiver failed: {}", e);
            glib::ExitCode::FAILURE
        }
    }
}

fn receive_and_reconstruct(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Connecting to upgrade socket: {:?}", socket_path);

    // Connect to the socket
    let mut stream = UnixStream::connect(socket_path)?;

    log::info!("Connected, receiving state and file descriptors...");

    // Receive state and FDs
    let mut buf = vec![0u8; MAX_STATE_SIZE];
    let (fds, data_len) = fd_passing::recv_fds(&stream, MAX_FDS, &mut buf)?;

    log::info!(
        "Received {} bytes of state data and {} file descriptors",
        data_len,
        fds.len()
    );

    // Deserialize the state
    let state: UpgradeState = bincode::deserialize(&buf[..data_len])?;

    log::info!(
        "Upgrade state: format_version={}, cterm_version={}, windows={}",
        state.format_version,
        state.cterm_version,
        state.windows.len()
    );

    // Send acknowledgment before we start GTK (so old process can exit)
    stream.write_all(&[1])?;
    stream.flush()?;

    log::info!("Sent acknowledgment, starting GTK with restored state...");

    // Now start GTK and reconstruct the windows
    run_gtk_with_state(state, fds)?;

    Ok(())
}

/// Start GTK application with the restored state
fn run_gtk_with_state(
    state: UpgradeState,
    fds: Vec<RawFd>,
) -> Result<(), Box<dyn std::error::Error>> {
    use gtk4::prelude::*;
    use gtk4::Application;

    // Store state and FDs for use during window construction
    // We use thread-local storage since GTK callbacks don't easily pass data
    UPGRADE_STATE.with(|s| {
        *s.borrow_mut() = Some((state, fds));
    });

    let app = Application::builder()
        .application_id("com.cterm.terminal")
        .build();

    app.connect_activate(|app| {
        // Retrieve the stored state
        UPGRADE_STATE.with(|s| {
            if let Some((state, fds)) = s.borrow_mut().take() {
                reconstruct_windows(app, state, fds);
            }
        });
    });

    app.run();
    Ok(())
}

// Thread-local storage for upgrade state (used to pass data to GTK callback)
thread_local! {
    static UPGRADE_STATE: std::cell::RefCell<Option<(UpgradeState, Vec<RawFd>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Reconstruct windows from the upgrade state
fn reconstruct_windows(
    _app: &gtk4::Application,
    state: UpgradeState,
    fds: Vec<RawFd>,
) {
    log::info!("Reconstructing {} windows", state.windows.len());

    for (window_idx, window_state) in state.windows.iter().enumerate() {
        log::info!(
            "Window {}: {}x{} at ({}, {}), {} tabs, active={}",
            window_idx,
            window_state.width,
            window_state.height,
            window_state.x,
            window_state.y,
            window_state.tabs.len(),
            window_state.active_tab
        );

        // TODO: Create the actual GTK window with the stored state
        // This will require significant integration with the existing window.rs

        for (tab_idx, tab_state) in window_state.tabs.iter().enumerate() {
            log::info!(
                "  Tab {}: id={}, title='{}', fd_index={}",
                tab_idx,
                tab_state.id,
                tab_state.title,
                tab_state.pty_fd_index
            );

            // Get the PTY FD for this tab
            if tab_state.pty_fd_index < fds.len() {
                let _pty_fd = fds[tab_state.pty_fd_index];
                // TODO(Phase 8): Reconstruct NativePty from this FD and child_pid
                // using NativePty::from_raw_fd(), then create the terminal widget
                // with the restored terminal state
            }
        }
    }

    // For now, just log that we received everything
    // The full implementation will be completed in Phase 8
    log::warn!(
        "Window reconstruction not yet fully implemented - \
        this is a placeholder for the upgrade receiver"
    );

    // Close received FDs since we're not using them yet
    fd_passing::close_fds(&fds);
}
