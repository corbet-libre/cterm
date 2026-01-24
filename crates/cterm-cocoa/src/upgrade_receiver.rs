//! Upgrade receiver - handles receiving state from the old process during seamless upgrade
//!
//! This module is used when cterm is started with --upgrade-receiver flag.
//! It receives state from the parent process via an inherited FD, then starts
//! the normal app with the received state.

use std::os::unix::io::RawFd;

use cterm_app::upgrade::receive_upgrade;

/// Run the upgrade receiver
///
/// This function:
/// 1. Reads from the inherited FD passed by the parent
/// 2. Receives the upgrade state and PTY file descriptors
/// 3. Sends acknowledgment
/// 4. Stores state and starts normal app (which will restore windows)
pub fn run_receiver(fd: i32) -> i32 {
    match receive_and_start(fd) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("Upgrade receiver failed: {}", e);
            1
        }
    }
}

fn receive_and_start(fd: i32) -> Result<(), Box<dyn std::error::Error>> {
    // Use the upgrade module to receive the state
    let (state, fds) = receive_upgrade(fd as RawFd)?;

    log::info!(
        "Upgrade state received: format_version={}, cterm_version={}, windows={}",
        state.format_version,
        state.cterm_version,
        state.windows.len()
    );

    // Store the upgrade state for AppDelegate to use during launch
    crate::app::set_upgrade_state(state, fds);

    log::info!("Starting app with restored state...");

    // Run the app - AppDelegate will detect the upgrade state and restore windows
    crate::app::run_app_internal();

    Ok(())
}
