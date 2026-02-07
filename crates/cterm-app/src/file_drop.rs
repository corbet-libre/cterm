//! File drop handling for drag-and-drop operations
//!
//! Provides helpers for building PTY input from dropped files.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;

/// Size threshold above which a warning is shown in the dialog (1 MB).
pub const SIZE_WARNING_THRESHOLD: u64 = 1_048_576;

/// Information about a dropped file.
pub struct FileDropInfo {
    pub path: PathBuf,
    pub filename: String,
    pub size: u64,
    pub is_text: bool,
}

/// Action the user chose from the file drop dialog.
pub enum FileDropAction {
    PastePath,
    PasteContents,
    CreateViaBase64 { filename: String },
    CreateViaPrintf { filename: String },
}

impl FileDropInfo {
    /// Build a `FileDropInfo` from a filesystem path.
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_text = is_text_file(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            filename,
            size: metadata.len(),
            is_text,
        })
    }
}

/// Detect whether a file is likely text by reading the first 8 KB and
/// checking for null bytes.
pub fn is_text_file(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    Ok(!buf[..n].contains(&0))
}

/// Shell-escape a string by wrapping it in single quotes and escaping
/// any embedded single quotes as `'\''`.
pub fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Build the string that should be written to the PTY for the given action.
pub fn build_pty_input(info: &FileDropInfo, action: FileDropAction) -> io::Result<String> {
    match action {
        FileDropAction::PastePath => Ok(shell_escape(&info.path.to_string_lossy())),
        FileDropAction::PasteContents => {
            let contents = std::fs::read_to_string(&info.path)?;
            Ok(contents)
        }
        FileDropAction::CreateViaBase64 { filename } => {
            let data = std::fs::read(&info.path)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            Ok(format!(
                "base64 --decode <<'CTERM_EOF'\n{}\nCTERM_EOF\n > {}\n",
                encoded,
                shell_escape(&filename)
            ))
        }
        FileDropAction::CreateViaPrintf { filename } => {
            let data = std::fs::read(&info.path)?;
            let escaped = shell_escape(&filename);
            let mut result = String::new();
            // Chunk into ~4 KB pieces to stay under ARG_MAX.
            for (i, chunk) in data.chunks(4096).enumerate() {
                let hex: String = chunk.iter().map(|b| format!("\\x{:02x}", b)).collect();
                let op = if i == 0 { ">" } else { ">>" };
                result.push_str(&format!("printf '{}' {} {}\n", hex, op, escaped));
            }
            Ok(result)
        }
    }
}

/// Format a byte count as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} bytes", bytes)
    } else if bytes < 1_048_576 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1_073_741_824 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn test_shell_escape_with_single_quote() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_shell_escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn test_shell_escape_spaces_and_special() {
        assert_eq!(shell_escape("a b$c"), "'a b$c'");
    }

    #[test]
    fn test_is_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let text_path = dir.path().join("text.txt");
        std::fs::write(&text_path, "Hello, world!\n").unwrap();
        assert!(is_text_file(&text_path).unwrap());

        let bin_path = dir.path().join("binary.bin");
        std::fs::write(&bin_path, b"hello\x00world").unwrap();
        assert!(!is_text_file(&bin_path).unwrap());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 bytes");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1_048_576), "1.0 MB");
        assert_eq!(format_size(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn test_build_paste_path() {
        let info = FileDropInfo {
            path: PathBuf::from("/tmp/my file.txt"),
            filename: "my file.txt".into(),
            size: 100,
            is_text: true,
        };
        let result = build_pty_input(&info, FileDropAction::PastePath).unwrap();
        assert_eq!(result, "'/tmp/my file.txt'");
    }

    #[test]
    fn test_build_create_via_base64() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"ABC").unwrap();

        let info = FileDropInfo {
            path: path.clone(),
            filename: "test.bin".into(),
            size: 3,
            is_text: false,
        };
        let result = build_pty_input(
            &info,
            FileDropAction::CreateViaBase64 {
                filename: "test.bin".into(),
            },
        )
        .unwrap();
        assert!(result.contains("QUJD")); // base64 of "ABC"
        assert!(result.contains("CTERM_EOF"));
        assert!(result.contains("'test.bin'"));
    }

    #[test]
    fn test_build_create_via_printf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"\x01\x02\x03").unwrap();

        let info = FileDropInfo {
            path: path.clone(),
            filename: "out.bin".into(),
            size: 3,
            is_text: false,
        };
        let result = build_pty_input(
            &info,
            FileDropAction::CreateViaPrintf {
                filename: "out.bin".into(),
            },
        )
        .unwrap();
        assert!(result.contains("\\x01\\x02\\x03"));
        assert!(result.contains("> 'out.bin'"));
    }
}
