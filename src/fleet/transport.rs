//! The IO seam between fleet Pull and a remote Host. Production uses the
//! system `ssh`/`scp`; tests substitute a fake that runs commands locally.

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use super::commands::{scp_args, ssh_args};

/// How fleet Pull reaches a remote Host. Every method mirrors one system
/// `ssh`/`scp` invocation, including its exit status (ssh reports its own
/// connection failures as exit 255 with a message on stderr).
pub trait RemoteTransport {
    /// Run `remote_command` on `destination` to completion; stdin is empty.
    fn run(&self, destination: &str, remote_command: &str) -> Result<Output>;
    /// Run a repository batch with a hard local process-group deadline.
    fn run_bounded(
        &self,
        destination: &str,
        remote_command: &str,
        limit: Duration,
    ) -> Result<Output>;
    /// Copy `local` to `remote_path` on `destination`.
    fn copy(&self, local: &Path, destination: &str, remote_path: &str) -> Result<Output>;
    /// Start `remote_command` with stdout piped back and stderr inherited.
    fn spawn(&self, destination: &str, remote_command: &str) -> Result<Child>;
}

/// The system `ssh` and `scp`, in BatchMode.
pub struct SystemSsh;

impl RemoteTransport for SystemSsh {
    fn run(&self, destination: &str, remote_command: &str) -> Result<Output> {
        Command::new("ssh")
            .args(ssh_args(destination, remote_command))
            .stdin(Stdio::null())
            .output()
            .context("invoking ssh")
    }

    fn run_bounded(
        &self,
        destination: &str,
        remote_command: &str,
        limit: Duration,
    ) -> Result<Output> {
        let mut command = Command::new("ssh");
        command.args(ssh_args(destination, remote_command));
        crate::bounded_process::output(command, limit)
    }

    fn copy(&self, local: &Path, destination: &str, remote_path: &str) -> Result<Output> {
        Command::new("scp")
            .args(scp_args(local, destination, remote_path))
            .stdin(Stdio::null())
            .output()
            .context("invoking scp")
    }

    fn spawn(&self, destination: &str, remote_command: &str) -> Result<Child> {
        Command::new("ssh")
            .args(ssh_args(destination, remote_command))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawning ssh")
    }
}
