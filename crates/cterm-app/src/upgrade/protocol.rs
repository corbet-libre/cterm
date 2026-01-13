//! Upgrade protocol - handles sending and receiving upgrade state
//!
//! This module provides the core protocol for seamless upgrades:
//! - Sender side: serializes state and sends to new process
//! - Receiver side: deserializes state and reconstructs terminals

#[cfg(unix)]
use cterm_core::fd_passing;

use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::process::Command;

use super::state::UpgradeState;

/// Maximum size of the state buffer (64MB)
pub const MAX_STATE_SIZE: usize = 64 * 1024 * 1024;

/// Maximum number of file descriptors to transfer
pub const MAX_FDS: usize = 256;

/// Errors that can occur during upgrade
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Process spawn error: {0}")]
    Spawn(String),

    #[error("Socket error: {0}")]
    Socket(String),

    #[error("Acknowledgment timeout")]
    AckTimeout,

    #[error("Too many file descriptors: {0} (max: {1})")]
    TooManyFds(usize, usize),
}

/// Execute an upgrade by sending state to a new process
///
/// # Arguments
/// * `new_binary` - Path to the new binary to execute
/// * `state` - The upgrade state to transfer
/// * `fds` - File descriptors to pass (PTY master FDs)
///
/// # Returns
/// Returns Ok(()) if the upgrade was successful (at which point this process should exit)
#[cfg(unix)]
pub fn execute_upgrade(
    new_binary: &Path,
    state: &UpgradeState,
    fds: &[RawFd],
) -> Result<(), UpgradeError> {
    use std::io::Read;

    if fds.len() > MAX_FDS {
        return Err(UpgradeError::TooManyFds(fds.len(), MAX_FDS));
    }

    // Create a temporary socket path
    let socket_path = std::env::temp_dir().join(format!("cterm-upgrade-{}", std::process::id()));

    // Clean up any existing socket
    let _ = std::fs::remove_file(&socket_path);

    // Create the listener
    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| UpgradeError::Socket(format!("Failed to bind socket: {}", e)))?;

    log::info!("Upgrade socket created at {:?}", socket_path);

    // Serialize the state
    let state_bytes =
        bincode::serialize(state).map_err(|e| UpgradeError::Serialization(e.to_string()))?;

    log::info!(
        "State serialized: {} bytes, {} FDs",
        state_bytes.len(),
        fds.len()
    );

    // Spawn the new process
    let child = Command::new(new_binary)
        .arg("--upgrade-receiver")
        .arg(&socket_path)
        .spawn()
        .map_err(|e| UpgradeError::Spawn(e.to_string()))?;

    log::info!("New process spawned with PID: {}", child.id());

    // Set a timeout for accepting the connection
    listener.set_nonblocking(false)?;

    // Accept the connection from the new process
    let (mut stream, _) = listener
        .accept()
        .map_err(|e| UpgradeError::Socket(format!("Failed to accept connection: {}", e)))?;

    log::info!("Connection accepted from new process");

    // Send the state and FDs
    fd_passing::send_fds(&stream, fds, &state_bytes)?;

    log::info!("State and FDs sent");

    // Wait for acknowledgment
    let mut ack = [0u8; 1];
    stream
        .read_exact(&mut ack)
        .map_err(|_| UpgradeError::AckTimeout)?;

    if ack[0] != 1 {
        return Err(UpgradeError::Socket("Invalid acknowledgment".to_string()));
    }

    log::info!("Acknowledgment received, upgrade successful");

    // Clean up the socket
    let _ = std::fs::remove_file(&socket_path);

    Ok(())
}

/// Receive upgrade state from the old process
///
/// This is called by the new process when started with --upgrade-receiver
///
/// # Arguments
/// * `socket_path` - Path to the Unix socket to connect to
///
/// # Returns
/// The upgrade state and file descriptors received
#[cfg(unix)]
pub fn receive_upgrade(socket_path: &Path) -> Result<(UpgradeState, Vec<RawFd>), UpgradeError> {
    use std::io::Write;

    log::info!("Connecting to upgrade socket: {:?}", socket_path);

    // Connect to the socket
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| UpgradeError::Socket(format!("Failed to connect: {}", e)))?;

    log::info!("Connected to upgrade socket");

    // Receive state and FDs
    let mut buf = vec![0u8; MAX_STATE_SIZE];
    let (fds, data_len) = fd_passing::recv_fds(&stream, MAX_FDS, &mut buf)?;

    log::info!(
        "Received {} bytes of state data and {} FDs",
        data_len,
        fds.len()
    );

    // Deserialize the state
    let state: UpgradeState = bincode::deserialize(&buf[..data_len])
        .map_err(|e| UpgradeError::Deserialization(e.to_string()))?;

    log::info!(
        "State deserialized: format_version={}, cterm_version={}, windows={}",
        state.format_version,
        state.cterm_version,
        state.windows.len()
    );

    // Send acknowledgment
    stream.write_all(&[1])?;
    stream.flush()?;

    log::info!("Acknowledgment sent");

    Ok((state, fds))
}

#[cfg(test)]
mod tests {
    use crate::upgrade::state::*;

    #[test]
    fn test_state_serialization_roundtrip() {
        let mut state = UpgradeState::new("0.1.0");

        let mut window = WindowUpgradeState::new();
        window.width = 1024;
        window.height = 768;
        state.windows.push(window);

        let bytes = bincode::serialize(&state).expect("Serialize failed");
        let restored: UpgradeState = bincode::deserialize(&bytes).expect("Deserialize failed");

        assert_eq!(restored.windows.len(), 1);
        assert_eq!(restored.windows[0].width, 1024);
    }
}
