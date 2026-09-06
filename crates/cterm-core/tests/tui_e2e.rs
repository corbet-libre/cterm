#![cfg(unix)]
//! Real-program TUI fixtures through the cterm PTY layer.
//!
//! Host availability (checked 2026-09-06, also gated per-test at runtime):
//! - nvim, vim, tmux, less, man, fzf, lazygit: present (vim/less added here;
//!   man exercises less as its pager; fzf/lazygit are present but too
//!   interactive/flaky for deterministic fixtures, so they stay documented
//!   rather than asserted).
//! - screen, emacs-nox, htop: absent here — skipped by design. CI installs
//!   only foot/neovim/tmux for the hard gate, so vim/less run wherever the
//!   binaries exist and skip otherwise via `require_program`.

use cterm_core::screen::ScreenConfig;
use cterm_core::{PtyConfig, Terminal};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TuiHarness {
    terminal: Terminal,
    output: Receiver<Vec<u8>>,
}

impl TuiHarness {
    fn spawn(program: &str, args: &[String], env: Vec<(String, String)>) -> Self {
        let config = PtyConfig {
            shell: Some(program.to_string()),
            args: args.to_vec(),
            env,
            term: Some("foot".to_string()),
            ..Default::default()
        };
        let terminal = Terminal::with_shell(80, 24, ScreenConfig::default(), &config)
            .unwrap_or_else(|error| panic!("failed to start {program}: {error}"));
        let mut reader = terminal.pty_reader().expect("local PTY reader");
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if sender.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { terminal, output }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.terminal.write(bytes).expect("write TUI input");
    }

    fn wait_until(&mut self, description: &str, predicate: impl Fn(&Terminal) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if predicate(&self.terminal) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}; screen:\n{}",
                screen_text(&self.terminal)
            );
            match self.output.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => {
                    self.terminal.process(&bytes);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    panic!(
                        "PTY closed while waiting for {description}; screen:\n{}",
                        screen_text(&self.terminal)
                    );
                }
            }
        }
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.terminal.is_running() {
            assert!(Instant::now() < deadline, "TUI did not exit");
            match self.output.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => {
                    self.terminal.process(&bytes);
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
        }
    }
}

impl Drop for TuiHarness {
    fn drop(&mut self) {
        if self.terminal.is_running() {
            let _ = self.terminal.send_signal(libc::SIGTERM);
        }
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cterm-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn require_program(program: &str, version_argument: &str) {
    let available = Command::new(program)
        .arg(version_argument)
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(available, "required TUI is unavailable: {program}");
}

fn screen_text(terminal: &Terminal) -> String {
    (0..terminal.rows())
        .filter_map(|row| terminal.screen().grid().row(row))
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "run by the hard Linux TUI compatibility GHA gate"]
fn neovim_edits_and_saves_through_cterm_pty() {
    require_program("nvim", "--version");
    let directory = TestDirectory::new("nvim");
    let file = directory.path().join("document.txt");
    fs::write(&file, "initial\n").expect("write fixture");
    let args = vec![
        "--clean".to_string(),
        "--cmd".to_string(),
        "set noswapfile nohlsearch".to_string(),
        file.to_string_lossy().into_owned(),
    ];
    let mut tui = TuiHarness::spawn("nvim", &args, Vec::new());

    tui.wait_until("Neovim alternate screen", |terminal| {
        terminal.screen().modes.alternate_screen
    });
    tui.send(b"gg0ciwCTERM_NVIM_OK\x1b:wq\r");
    tui.wait_for_exit();

    let saved = fs::read_to_string(file).expect("read edited fixture");
    assert_eq!(saved, "CTERM_NVIM_OK\n");
}

#[test]
#[ignore = "run by the hard Linux TUI compatibility GHA gate"]
fn vim_edits_and_saves_through_cterm_pty() {
    require_program("vim", "--version");
    let directory = TestDirectory::new("vim");
    let file = directory.path().join("document.txt");
    fs::write(&file, "initial\n").expect("write fixture");
    let args = vec![
        "-u".to_string(),
        "NONE".to_string(),
        "-i".to_string(),
        "NONE".to_string(),
        "-n".to_string(),
        file.to_string_lossy().into_owned(),
    ];
    let mut tui = TuiHarness::spawn("vim", &args, Vec::new());

    tui.wait_until("Vim alternate screen", |terminal| {
        terminal.screen().modes.alternate_screen
    });
    tui.send(b"gg0ciwCTERM_VIM_OK\x1b:wq\r");
    tui.wait_for_exit();

    let saved = fs::read_to_string(file).expect("read edited fixture");
    assert_eq!(saved, "CTERM_VIM_OK\n");
}

#[test]
#[ignore = "run by the hard Linux TUI compatibility GHA gate"]
fn less_renders_file_and_quits_through_cterm_pty() {
    require_program("less", "--version");
    let directory = TestDirectory::new("less");
    let file = directory.path().join("pager.txt");
    // More lines than the 24-row viewport so less stays open in the
    // alternate screen instead of quitting immediately (`F` would exit).
    let mut contents = String::from("CTERM_LESS_LINE_ONE\n");
    for line in 2..=60 {
        contents.push_str(&format!("filler line {line}\n"));
    }
    fs::write(&file, contents).expect("write fixture");
    let args = vec![file.to_string_lossy().into_owned()];
    // Pager-friendly env: raw control chars, but keep init (alternate
    // screen) so the fixture observes the fullscreen pager path.
    let env = vec![("LESS".to_string(), "R".to_string())];
    let mut tui = TuiHarness::spawn("less", &args, env);

    tui.wait_until("less alternate screen", |terminal| {
        terminal.screen().modes.alternate_screen
    });
    tui.wait_until("less file content", |terminal| {
        screen_text(terminal).contains("CTERM_LESS_LINE_ONE")
    });
    tui.send(b"q");
    tui.wait_for_exit();
}

#[test]
#[ignore = "run by the hard Linux TUI compatibility GHA gate"]
fn tmux_session_renders_and_accepts_input_through_cterm_pty() {
    require_program("tmux", "-V");
    let socket = format!("cterm-ci-{}", std::process::id());
    let args = vec![
        "-L".to_string(),
        socket.clone(),
        "-f".to_string(),
        "/dev/null".to_string(),
        "new-session".to_string(),
        "/bin/sh".to_string(),
    ];
    let env = vec![("SHELL".to_string(), "/bin/sh".to_string())];
    let mut tui = TuiHarness::spawn("tmux", &args, env);

    tui.wait_until("tmux alternate screen", |terminal| {
        terminal.screen().modes.alternate_screen
    });
    tui.send(b"printf 'CTERM_TMUX_OK\\n'\r");
    tui.wait_until("tmux command output", |terminal| {
        screen_text(terminal).contains("CTERM_TMUX_OK")
    });
    tui.send(b"exit\r");
    tui.wait_for_exit();

    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
}
