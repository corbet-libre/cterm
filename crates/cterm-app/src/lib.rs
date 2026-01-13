//! cterm-app: Application logic for cterm
//!
//! This crate contains the application logic that is independent of the UI,
//! including configuration management, session handling, sticky tabs,
//! and seamless upgrade functionality.

pub mod config;
pub mod docker;
pub mod session;
pub mod shortcuts;
pub mod upgrade;

pub use config::{load_config, save_config, Config};
pub use session::{Session, TabState, WindowState};
pub use shortcuts::ShortcutManager;
#[cfg(unix)]
pub use upgrade::{execute_upgrade, receive_upgrade, UpgradeError};
pub use upgrade::{UpdateError, UpdateInfo, Updater, UpgradeState};
