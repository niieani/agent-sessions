//! An in-process stand-in for `ssh`/`scp`: remote commands run under `sh -c`
//! in a temp "remote home", so fleet Pull is exercised end to end without a
//! network, an sshd, or a real remote. Never touches the real home.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use agent_sessions::fleet::RemoteTransport;

/// What the fake remote reports, and every call it received.
pub struct FakeRemote {
    home: tempfile::TempDir,
    tools: tempfile::TempDir,
    /// `Some(stderr)`: every call fails like ssh does before any remote
    /// command ran (exit 255, message on stderr).
    connect_failure: Option<String>,
    calls: RefCell<Vec<String>>,
}

impl FakeRemote {
    /// A reachable remote on this machine's real platform.
    pub fn reachable() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            tools: tempfile::tempdir().unwrap(),
            connect_failure: None,
            calls: RefCell::default(),
        }
    }

    /// A remote whose every ssh call fails with exit 255 and `stderr`.
    pub fn failing_connection(stderr: &str) -> Self {
        Self {
            connect_failure: Some(stderr.to_string()),
            ..Self::reachable()
        }
    }

    /// Make the remote's `uname -sm` report `platform` (e.g. `Linux x86_64`).
    pub fn with_uname(self, platform: &str) -> Self {
        write_executable(
            &self.tools.path().join("uname"),
            &format!("#!/bin/sh\necho '{platform}'\n"),
        );
        self
    }

    /// The remote home: relative `remote-bin` paths resolve against it.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Install an executable at `relative` under the remote home.
    pub fn install(&self, relative: &str, script: &str) -> PathBuf {
        let path = self.home.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_executable(&path, script);
        path
    }

    /// Every transport call, in order: `run <cmd>`, `copy <path>`, `spawn <cmd>`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn command(&self, remote_command: &str) -> Command {
        let mut command = match &self.connect_failure {
            Some(stderr) => {
                let mut command = Command::new("sh");
                command.args(["-c", "printf '%s\\n' \"$1\" >&2; exit 255", "sh", stderr]);
                command
            }
            None => {
                let mut command = Command::new("sh");
                command.args(["-c", remote_command]);
                command
            }
        };
        let path = format!(
            "{}:{}",
            self.tools.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("PATH", path)
            .stdin(Stdio::null());
        command
    }
}

impl RemoteTransport for FakeRemote {
    fn run(&self, _destination: &str, remote_command: &str) -> anyhow::Result<Output> {
        self.calls
            .borrow_mut()
            .push(format!("run {remote_command}"));
        Ok(self.command(remote_command).output()?)
    }

    fn run_bounded(
        &self,
        _: &str,
        remote_command: &str,
        limit: std::time::Duration,
    ) -> anyhow::Result<Output> {
        self.calls
            .borrow_mut()
            .push(format!("run {remote_command}"));
        agent_sessions::repository_resolution::bounded_command_output(
            self.command(remote_command),
            limit,
        )
    }

    fn copy(&self, local: &Path, _destination: &str, remote_path: &str) -> anyhow::Result<Output> {
        self.calls.borrow_mut().push(format!("copy {remote_path}"));
        let mut command = self.command("");
        if self.connect_failure.is_none() {
            command = Command::new("cp");
            command
                .arg(local)
                .arg(self.home.path().join(remote_path))
                .stdin(Stdio::null());
        }
        Ok(command.output()?)
    }

    fn spawn(&self, _destination: &str, remote_command: &str) -> anyhow::Result<Child> {
        self.calls
            .borrow_mut()
            .push(format!("spawn {remote_command}"));
        Ok(self
            .command(remote_command)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?)
    }
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `uname -sm` line for a platform other than the one these tests run on,
/// and the release target such a remote needs.
pub fn foreign_platform() -> (&'static str, &'static str) {
    if cfg!(target_os = "linux") {
        ("Darwin arm64", "aarch64-apple-darwin")
    } else {
        ("Linux x86_64", "x86_64-unknown-linux-musl")
    }
}
