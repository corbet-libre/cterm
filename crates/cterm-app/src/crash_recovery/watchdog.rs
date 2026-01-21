//! Watchdog process for crash recovery
//!
//! The watchdog is a lightweight parent process that:
//! 1. Spawns the main cterm UI process
//! 2. Receives PTY file descriptors from the child via Unix socket
//! 3. Monitors the child for crashes
//! 4. On crash, relaunches with --crash-recovery flag

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

use cterm_core::fd_passing;

use super::state::write_crash_marker;

/// Errors from the watchdog
#[derive(Debug, thiserror::Error)]
pub enum WatchdogError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Failed to spawn child: {0}")]
    Spawn(String),

    #[error("Socket error: {0}")]
    Socket(String),
}

/// Message types sent between watchdog and supervised process
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogMessage {
    /// Register a new PTY FD (followed by FD via SCM_RIGHTS)
    RegisterFd = 1,
    /// Unregister a PTY FD (tab closed)
    UnregisterFd = 2,
    /// Graceful shutdown (no restart on exit)
    Shutdown = 3,
    /// Heartbeat
    Heartbeat = 4,
}

/// Run the watchdog process
///
/// This function:
/// 1. Creates a socket pair for communication
/// 2. Spawns the supervised child process
/// 3. Receives and stores PTY FDs from the child
/// 4. Monitors for crashes and relaunches if needed
///
/// Returns when graceful shutdown is requested or max restarts exceeded.
pub fn run_watchdog(binary_path: &Path, args: &[String]) -> Result<i32, WatchdogError> {
    let max_restarts = 5;
    let mut restart_count = 0;
    let mut graceful_shutdown = false;

    loop {
        // Create socket pair for communication
        let (watchdog_sock, child_sock) = UnixStream::pair()
            .map_err(|e| WatchdogError::Socket(format!("Failed to create socketpair: {}", e)))?;

        let child_fd = child_sock.as_raw_fd();

        // Track FDs received from child
        let mut pty_fds: HashMap<u64, RawFd> = HashMap::new();
        let mut next_fd_id: u64 = 0;

        // Build args - pass the watchdog socket FD
        let mut child_args = args.to_vec();
        child_args.push("--supervised".to_string());
        child_args.push(child_fd.to_string());

        // Spawn child process
        let child_fd_for_closure = child_fd;
        let mut child = unsafe {
            Command::new(binary_path)
                .args(&child_args)
                .pre_exec(move || {
                    // Clear close-on-exec for the socket FD
                    let flags = libc::fcntl(child_fd_for_closure, libc::F_GETFD);
                    if flags != -1 {
                        libc::fcntl(
                            child_fd_for_closure,
                            libc::F_SETFD,
                            flags & !libc::FD_CLOEXEC,
                        );
                    }
                    Ok(())
                })
                .spawn()
                .map_err(|e| WatchdogError::Spawn(e.to_string()))?
        };

        log::info!(
            "Watchdog: spawned child PID {} (restart count: {})",
            child.id(),
            restart_count
        );

        // Close our copy of the child socket
        drop(child_sock);

        // Set socket to non-blocking for interleaved read/wait
        watchdog_sock.set_nonblocking(true)?;

        // Monitor loop
        let exit_status = monitor_child(&mut child, &watchdog_sock, &mut pty_fds, &mut next_fd_id, &mut graceful_shutdown)?;

        if graceful_shutdown {
            log::info!("Watchdog: graceful shutdown requested");
            // Close all PTY FDs
            for (_, fd) in pty_fds {
                unsafe { libc::close(fd) };
            }
            return Ok(exit_status.unwrap_or(0));
        }

        // Check if it was a crash (signal) vs normal exit
        let crashed = exit_status.is_none() || exit_status.unwrap() != 0;

        if crashed {
            restart_count += 1;
            log::warn!(
                "Watchdog: child crashed with status {:?}, restart {}/{}",
                exit_status,
                restart_count,
                max_restarts
            );

            if restart_count > max_restarts {
                log::error!("Watchdog: max restarts exceeded, giving up");
                // Close all PTY FDs
                for (_, fd) in pty_fds {
                    unsafe { libc::close(fd) };
                }
                return Ok(1);
            }

            // Write crash marker for the new process
            let _ = write_crash_marker(exit_status.unwrap_or(-1));

            // Small delay before restart
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Continue to next iteration to restart
            // Note: pty_fds are preserved and will be passed to new child
        } else {
            log::info!("Watchdog: child exited normally");
            // Close all PTY FDs
            for (_, fd) in pty_fds {
                unsafe { libc::close(fd) };
            }
            return Ok(0);
        }
    }
}

/// Monitor the child process and handle messages
fn monitor_child(
    child: &mut Child,
    sock: &UnixStream,
    pty_fds: &mut HashMap<u64, RawFd>,
    next_fd_id: &mut u64,
    graceful_shutdown: &mut bool,
) -> Result<Option<i32>, WatchdogError> {
    let mut buf = [0u8; 1024];

    loop {
        // Check if child has exited
        match child.try_wait()? {
            Some(status) => {
                return Ok(status.code());
            }
            None => {}
        }

        // Try to read a message (non-blocking)
        // We need to use a mutable reference here
        let sock_ref: &UnixStream = sock;
        match Read::read(&mut &*sock_ref, &mut buf) {
            Ok(0) => {
                // Child closed socket, wait for exit
                let status = child.wait()?;
                return Ok(status.code());
            }
            Ok(n) => {
                // Process message
                if n >= 1 {
                    match buf[0] {
                        1 => {
                            // RegisterFd - receive FD via SCM_RIGHTS
                            let mut fd_buf = [0u8; 8];
                            let (fds, _) = fd_passing::recv_fds(sock, 1, &mut fd_buf)?;
                            if let Some(&fd) = fds.first() {
                                let id = *next_fd_id;
                                *next_fd_id += 1;
                                pty_fds.insert(id, fd);
                                log::debug!("Watchdog: registered FD {} with id {}", fd, id);
                            }
                        }
                        2 => {
                            // UnregisterFd
                            if n >= 9 {
                                let id = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                                if let Some(fd) = pty_fds.remove(&id) {
                                    unsafe { libc::close(fd) };
                                    log::debug!("Watchdog: unregistered FD id {}", id);
                                }
                            }
                        }
                        3 => {
                            // Shutdown
                            *graceful_shutdown = true;
                            log::debug!("Watchdog: shutdown requested");
                        }
                        4 => {
                            // Heartbeat - just acknowledge
                            log::trace!("Watchdog: heartbeat received");
                        }
                        _ => {}
                    }
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No data available, sleep briefly
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                return Err(WatchdogError::Io(e));
            }
        }
    }
}

/// Send a PTY FD to the watchdog for safekeeping
#[cfg(unix)]
pub fn register_fd_with_watchdog(watchdog_fd: RawFd, pty_fd: RawFd) -> io::Result<()> {
    let sock = unsafe { UnixStream::from_raw_fd(watchdog_fd) };

    // Send register message with FD
    let msg = [WatchdogMessage::RegisterFd as u8];
    fd_passing::send_fds(&sock, &[pty_fd], &msg)?;

    // Don't close the socket - it's borrowed
    std::mem::forget(sock);

    Ok(())
}

/// Tell watchdog we're shutting down gracefully
#[cfg(unix)]
pub fn notify_watchdog_shutdown(watchdog_fd: RawFd) -> io::Result<()> {
    let mut sock = unsafe { UnixStream::from_raw_fd(watchdog_fd) };

    sock.write_all(&[WatchdogMessage::Shutdown as u8])?;

    // Don't close the socket - it's borrowed
    std::mem::forget(sock);

    Ok(())
}
