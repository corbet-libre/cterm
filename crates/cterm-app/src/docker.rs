//! Docker utility functions for container/image management

use std::fmt;
use std::process::Command;

/// Error type for Docker operations
#[derive(Debug)]
pub enum DockerError {
    /// Docker binary not found
    NotInstalled,
    /// Docker daemon is not running
    DaemonNotRunning,
    /// Docker command failed with error message
    CommandFailed(String),
    /// Failed to parse Docker output
    ParseError(String),
}

impl fmt::Display for DockerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerError::NotInstalled => write!(f, "Docker is not installed"),
            DockerError::DaemonNotRunning => write!(f, "Docker daemon is not running"),
            DockerError::CommandFailed(msg) => write!(f, "Docker command failed: {}", msg),
            DockerError::ParseError(msg) => write!(f, "Failed to parse Docker output: {}", msg),
        }
    }
}

impl std::error::Error for DockerError {}

/// Information about a running container from `docker ps`
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Container ID (short form)
    pub id: String,
    /// Container name
    pub name: String,
    /// Image used to create the container
    pub image: String,
    /// Container status (e.g., "Up 2 hours")
    pub status: String,
}

/// Information about a Docker image from `docker images`
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// Image ID (short form)
    pub id: String,
    /// Repository name
    pub repository: String,
    /// Image tag
    pub tag: String,
    /// Human-readable size (e.g., "128MB")
    pub size: String,
}

/// Check if Docker is available and the daemon is running
pub fn check_docker_available() -> Result<(), DockerError> {
    let output = Command::new("docker")
        .arg("version")
        .output()
        .map_err(|_| DockerError::NotInstalled)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Cannot connect")
            || stderr.contains("daemon")
            || stderr.contains("Is the docker daemon running")
        {
            return Err(DockerError::DaemonNotRunning);
        }
        return Err(DockerError::CommandFailed(stderr.to_string()));
    }

    Ok(())
}

/// List running containers
pub fn list_containers() -> Result<Vec<ContainerInfo>, DockerError> {
    let output = Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}",
        ])
        .output()
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        return Err(DockerError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let containers = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 {
                Some(ContainerInfo {
                    id: parts[0].to_string(),
                    name: parts[1].to_string(),
                    image: parts[2].to_string(),
                    status: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(containers)
}

/// List available Docker images
pub fn list_images() -> Result<Vec<ImageInfo>, DockerError> {
    let output = Command::new("docker")
        .args([
            "images",
            "--format",
            "{{.ID}}\t{{.Repository}}\t{{.Tag}}\t{{.Size}}",
        ])
        .output()
        .map_err(|e| DockerError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        return Err(DockerError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let images = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 && parts[1] != "<none>" {
                Some(ImageInfo {
                    id: parts[0].to_string(),
                    repository: parts[1].to_string(),
                    tag: parts[2].to_string(),
                    size: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(images)
}

/// Build command and arguments for `docker exec`
///
/// Returns (command, args) tuple suitable for PtyConfig
pub fn build_exec_command(container: &str, shell: Option<&str>) -> (String, Vec<String>) {
    let shell = shell.unwrap_or("/bin/sh");
    (
        "docker".to_string(),
        vec![
            "exec".to_string(),
            "-it".to_string(),
            container.to_string(),
            shell.to_string(),
        ],
    )
}

/// Build command and arguments for `docker run`
///
/// Returns (command, args) tuple suitable for PtyConfig
pub fn build_run_command(
    image: &str,
    shell: Option<&str>,
    auto_remove: bool,
    extra_args: &[String],
) -> (String, Vec<String>) {
    let shell = shell.unwrap_or("/bin/sh");
    let mut args = vec!["run".to_string(), "-it".to_string()];

    if auto_remove {
        args.push("--rm".to_string());
    }

    args.extend(extra_args.iter().cloned());
    args.push(image.to_string());
    args.push(shell.to_string());

    ("docker".to_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_exec_command_default_shell() {
        let (cmd, args) = build_exec_command("my-container", None);
        assert_eq!(cmd, "docker");
        assert_eq!(args, vec!["exec", "-it", "my-container", "/bin/sh"]);
    }

    #[test]
    fn test_build_exec_command_custom_shell() {
        let (cmd, args) = build_exec_command("my-container", Some("/bin/bash"));
        assert_eq!(cmd, "docker");
        assert_eq!(args, vec!["exec", "-it", "my-container", "/bin/bash"]);
    }

    #[test]
    fn test_build_run_command_default() {
        let (cmd, args) = build_run_command("ubuntu:latest", None, true, &[]);
        assert_eq!(cmd, "docker");
        assert_eq!(args, vec!["run", "-it", "--rm", "ubuntu:latest", "/bin/sh"]);
    }

    #[test]
    fn test_build_run_command_no_auto_remove() {
        let (cmd, args) = build_run_command("ubuntu:latest", Some("/bin/bash"), false, &[]);
        assert_eq!(cmd, "docker");
        assert_eq!(args, vec!["run", "-it", "ubuntu:latest", "/bin/bash"]);
    }

    #[test]
    fn test_build_run_command_with_extra_args() {
        let extra = vec!["-v".to_string(), "/host:/container".to_string()];
        let (cmd, args) = build_run_command("ubuntu:latest", None, true, &extra);
        assert_eq!(cmd, "docker");
        assert_eq!(
            args,
            vec![
                "run",
                "-it",
                "--rm",
                "-v",
                "/host:/container",
                "ubuntu:latest",
                "/bin/sh"
            ]
        );
    }
}
