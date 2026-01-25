//! Git-backed configuration sync
//!
//! Provides git operations for syncing configuration across machines.

use std::path::Path;
use std::process::Command;

use thiserror::Error;

/// Git sync errors
#[derive(Error, Debug)]
pub enum GitError {
    #[error("Git command failed: {0}")]
    CommandFailed(String),

    #[error("Invalid path")]
    InvalidPath,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Check if the config directory is a git repository
pub fn is_git_repo(config_dir: &Path) -> bool {
    config_dir.join(".git").exists()
}

/// Get the remote URL for origin (if any)
pub fn get_remote_url(config_dir: &Path) -> Option<String> {
    if !is_git_repo(config_dir) {
        return None;
    }

    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(config_dir)
        .output()
        .ok()?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if url.is_empty() {
            None
        } else {
            Some(url)
        }
    } else {
        None
    }
}

/// Result of initializing a git repo with a remote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitResult {
    /// Repository was initialized, no remote content
    Initialized,
    /// Remote content was pulled, config should be reloaded
    PulledRemote,
    /// Remote was updated, existing local content preserved
    UpdatedRemote,
}

/// Initialize git repo and set remote.
/// Returns `InitResult::PulledRemote` if remote content was pulled and config should be reloaded.
pub fn init_with_remote(config_dir: &Path, remote_url: &str) -> Result<InitResult, GitError> {
    let was_repo = is_git_repo(config_dir);
    let had_commits = was_repo && run_git(config_dir, &["rev-parse", "HEAD"]).is_ok();

    // git init (if not already a repo)
    if !was_repo {
        run_git(config_dir, &["init"])?;
        log::info!("Initialized git repository in config directory");
    }

    // Set or update remote
    if get_remote_url(config_dir).is_some() {
        run_git(config_dir, &["remote", "set-url", "origin", remote_url])?;
        log::info!("Updated git remote URL to: {}", remote_url);
    } else {
        run_git(config_dir, &["remote", "add", "origin", remote_url])?;
        log::info!("Added git remote origin: {}", remote_url);
    }

    // Try to fetch and set up tracking branch
    if run_git(config_dir, &["fetch", "origin"]).is_err() {
        // Fetch failed (maybe empty repo or network issue)
        return Ok(InitResult::Initialized);
    }

    // Check if remote has main or master branch
    let has_main = run_git(config_dir, &["rev-parse", "--verify", "origin/main"]).is_ok();
    let has_master = run_git(config_dir, &["rev-parse", "--verify", "origin/master"]).is_ok();

    if !has_main && !has_master {
        // Remote is empty
        return Ok(InitResult::Initialized);
    }

    let branch = if has_main { "main" } else { "master" };

    if !had_commits {
        // No local commits - checkout the remote branch (this loads remote config)
        let _ = run_git(
            config_dir,
            &["checkout", "-b", branch, &format!("origin/{}", branch)],
        );
        log::info!(
            "Checked out remote {} branch - config should be reloaded",
            branch
        );
        Ok(InitResult::PulledRemote)
    } else {
        // Local commits exist - set up tracking
        let _ = run_git(config_dir, &["branch", "-u", &format!("origin/{}", branch)]);
        Ok(InitResult::UpdatedRemote)
    }
}

/// Pull from remote (with rebase to avoid merge commits)
/// Returns true if changes were pulled
pub fn pull(config_dir: &Path) -> Result<bool, GitError> {
    if !is_git_repo(config_dir) {
        return Ok(false);
    }

    // Check if we have a remote configured
    if get_remote_url(config_dir).is_none() {
        return Ok(false);
    }

    // First fetch to see what's available
    run_git(config_dir, &["fetch", "origin"])?;

    // Determine which branch to pull (main or master)
    let branch = if run_git(config_dir, &["rev-parse", "--verify", "origin/main"]).is_ok() {
        "main"
    } else if run_git(config_dir, &["rev-parse", "--verify", "origin/master"]).is_ok() {
        "master"
    } else {
        log::warn!("No main or master branch found on remote");
        return Ok(false);
    };

    // Try to pull with rebase and autostash
    let output = run_git(
        config_dir,
        &["pull", "--rebase", "--autostash", "origin", branch],
    )?;

    // Check if "Already up to date" or similar
    let up_to_date = output.contains("Already up to date")
        || output.contains("up-to-date")
        || output.contains("Already up-to-date");

    Ok(!up_to_date)
}

/// Commit and push all changes
pub fn commit_and_push(config_dir: &Path, message: &str) -> Result<(), GitError> {
    if !is_git_repo(config_dir) {
        return Err(GitError::CommandFailed("Not a git repository".to_string()));
    }

    // Ensure we have git user configured (use defaults if not)
    ensure_git_user(config_dir)?;

    // Stage all changes
    run_git(config_dir, &["add", "-A"])?;

    // Check if there are changes to commit
    let status = run_git(config_dir, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        log::debug!("No changes to commit");
        return Ok(()); // Nothing to commit
    }

    // Commit
    run_git(config_dir, &["commit", "-m", message])?;
    log::info!("Committed changes: {}", message);

    // Push (try main first, fall back to master)
    let push_result = run_git(config_dir, &["push", "-u", "origin", "main"]);
    if push_result.is_err() {
        run_git(config_dir, &["push", "-u", "origin", "master"])?;
    }

    log::info!("Pushed changes to remote");
    Ok(())
}

/// Ensure git user is configured (required for commits)
fn ensure_git_user(config_dir: &Path) -> Result<(), GitError> {
    // Check if user.name is set
    let name_set = run_git(config_dir, &["config", "user.name"]).is_ok();
    let email_set = run_git(config_dir, &["config", "user.email"]).is_ok();

    if !name_set {
        // Set a default name for this repo only
        run_git(config_dir, &["config", "user.name", "cterm"])?;
    }

    if !email_set {
        // Set a default email for this repo only
        run_git(config_dir, &["config", "user.email", "cterm@localhost"])?;
    }

    Ok(())
}

/// Run a git command and return stdout
fn run_git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    log::trace!("Running git {:?} in {}", args, dir.display());

    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(GitError::Io)?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        log::debug!("Git command failed: {:?} - {}", args, stderr);
        Err(GitError::CommandFailed(stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_is_git_repo_false() {
        let temp = TempDir::new().unwrap();
        assert!(!is_git_repo(temp.path()));
    }

    #[test]
    fn test_is_git_repo_true() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        assert!(is_git_repo(temp.path()));
    }

    #[test]
    fn test_get_remote_url_no_repo() {
        let temp = TempDir::new().unwrap();
        assert!(get_remote_url(temp.path()).is_none());
    }
}
