//! Seamless upgrade system for cterm
//!
//! This module provides functionality for upgrading cterm without losing
//! any running terminal sessions. It handles:
//!
//! - Checking for updates from GitHub releases
//! - Downloading and verifying new binaries
//! - Serializing terminal state
//! - Passing PTY file descriptors to the new process
//! - Reconstructing windows and tabs in the new process

mod state;
#[cfg(unix)]
mod protocol;
mod updater;

pub use state::{
    TabUpgradeState, TerminalUpgradeState, UpgradeState, WindowUpgradeState,
};
#[cfg(unix)]
pub use protocol::{execute_upgrade, receive_upgrade, UpgradeError, MAX_FDS, MAX_STATE_SIZE};
pub use updater::{UpdateError, UpdateInfo, Updater};
