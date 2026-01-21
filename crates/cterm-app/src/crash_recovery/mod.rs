//! Crash recovery system for cterm
//!
//! This module provides crash recovery functionality:
//! - Watchdog process that monitors the main cterm process
//! - Crash state file for persisting terminal state
//! - FD passing between watchdog and main process
//! - Recovery and restart after crashes

#[cfg(unix)]
mod watchdog;
#[cfg(unix)]
mod state;

#[cfg(unix)]
pub use watchdog::{run_watchdog, WatchdogError};
#[cfg(unix)]
pub use state::{CrashState, write_crash_state, read_crash_state, crash_state_path};
