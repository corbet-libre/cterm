//! Session management for ctermd

mod id;
mod manager;
mod state;
mod transfer_policy;
mod tty_transfer;

pub use id::generate_session_id;
pub use manager::SessionManager;
pub use state::{OutputData, PromptReply, SessionState};
pub(crate) use transfer_policy::DaemonTransferContext;

/// Build the daemon-wide transfer consent context at startup: the loaded
/// operator config plus a fresh process-lifetime bypass keypair.
pub(crate) fn transfer_context_for_startup(
    config: cterm_app::TransferConfig,
) -> DaemonTransferContext {
    DaemonTransferContext::new(config)
}
