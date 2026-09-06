//! cterm-core: Core terminal emulation library
//!
//! This crate provides the fundamental building blocks for terminal emulation:
//! - Color and cell attribute types
//! - Screen buffer management (grid, scrollback)
//! - ANSI/VT sequence parsing
//! - Cross-platform PTY handling

pub mod cell;
pub mod color;
pub mod dnd;
pub mod drcs;
pub mod grid;
pub mod image_decode;
pub mod iterm2;
pub mod keyboard;
pub mod kitty_clipboard;
pub mod kitty_file_transfer;
mod kitty_graphics;
mod kitty_placeholder;
pub mod mouse;
mod multiple_cursors;
pub mod notifications;
mod osc1337;
pub mod parser;
pub mod pointer_shape;
pub mod pty;
pub mod screen;
pub mod sixel;
pub mod ssh;
pub mod streaming_file;
pub mod term;
mod text_sizing;

pub use cell::{Cell, CellAttrs};
pub use color::{AnsiColor, Color, ColorPalette, Rgb};
pub use dnd::{DndCommand, DndCommandType, MAX_DND_CHUNK_BYTES};
pub use drcs::{DecdldDecoder, DrcsFont, DrcsGlyph};
pub use grid::Grid;
pub use image_decode::{decode_image, DecodedImage, ImageDecodeError};
pub use iterm2::{Iterm2Dimension, Iterm2FileParams};
pub use keyboard::{KeyEventKind, KeyEventMetadata, KeyboardEnhancementFlags};
pub use kitty_clipboard::{
    PastePassword, RichClipboardAlias, RichClipboardItem, RichClipboardLocation,
    RichClipboardOperation, RichClipboardStatus, RichCredential, RichReadRequest, RichWriteSession,
    MAX_PASTE_PASSWORDS, MAX_PENDING_RICH_CLIPBOARD_OPS, MAX_PENDING_RICH_READS,
    MAX_RICH_ALIAS_COUNT, MAX_RICH_CHUNK_BYTES, MAX_RICH_CREDENTIAL_BYTES, MAX_RICH_ID_CHARS,
    MAX_RICH_ITEM_BYTES, MAX_RICH_MIME_BYTES, MAX_RICH_MIME_COUNT, MAX_RICH_SESSION_BYTES,
    MAX_RICH_WRITE_ITEMS, PASTE_PASSWORD_TTL,
};
pub use kitty_file_transfer::{
    parse_file_transfer_command, FileTransferAction, FileTransferCodecError, FileTransferCommand,
    FileTransferCompression, FileTransferType, FileTransmissionType, MAX_FILE_TRANSFER_CHUNK_BYTES,
    MAX_FILE_TRANSFER_PATH_BYTES, MAX_PENDING_FILE_TRANSFER_COMMANDS,
};
pub use kitty_graphics::GraphicsAnimationTick;
pub use multiple_cursors::{ExtraCursor, ExtraCursorColor, ExtraCursorColors, ExtraCursorShape};
pub use notifications::{
    ActiveNotificationSet, IconCache, NotificationOccasion, MAX_ACTIVE_NOTIFICATIONS,
    MAX_APP_NAME_BYTES, MAX_ICON_CACHE_BYTES, MAX_ICON_CACHE_ENTRIES, MAX_ICON_NAMES,
    MAX_ICON_NAME_BYTES, MAX_NOTIFICATION_BODY_BYTES, MAX_NOTIFICATION_BUTTONS,
    MAX_NOTIFICATION_BUTTONS_BYTES, MAX_NOTIFICATION_BUTTON_BYTES, MAX_NOTIFICATION_ICON_BYTES,
    MAX_NOTIFICATION_TITLE_BYTES, MAX_NOTIFICATION_TYPES, MAX_NOTIFICATION_TYPE_BYTES,
    MAX_SOUND_NAME_BYTES,
};
pub use parser::Parser;
pub use pointer_shape::{
    PointerShape, DEFAULT_POINTER_CSS, GRABBED_POINTER_CSS, MAX_POINTER_SHAPE_STACK_DEPTH,
};
#[cfg(unix)]
pub use pty::save_original_nofile_limit;
pub use pty::{Pty, PtyConfig, PtyError, PtySize};
pub use screen::{
    ClipboardOperation, ClipboardSelection, ColorQuery, CursorStyle, DesktopNotification,
    DesktopNotificationAction, FileTransferOperation, FrontendState, ImageLayer, KittyColorKey,
    KittyColorSetting, KittyColorSnapshot, KittyQueryField, MouseEncoding, MouseMode,
    NotificationUrgency, Screen, SearchResult, Selection, SelectionMode, SelectionPoint,
    TerminalImage, ThemeAppearance, WindowVisibility, MAX_OSC21_FIELDS, MAX_OSC21_KEY_LEN,
};
pub use sixel::{SixelDecoder, SixelImage};
pub use ssh::{
    HostKeyPrompt, HostKeyRequest, LocalForward, PassphrasePrompt, PasswordPrompt, SshConfig,
    SshPrompts,
};
#[cfg(unix)]
pub use ssh::{SshChannelOpener, SshChannelReader, SshChannelWriter, SshTunnel};
pub use streaming_file::{StreamingFileData, StreamingFileReceiver, StreamingFileResult};
pub use term::{Terminal, WriteFn};
pub use text_sizing::{Multicell, TextSizeAlignment};
