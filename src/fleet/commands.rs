//! The exact ssh/scp argument vectors and remote shell commands fleet Pull
//! runs. Pure string construction; no IO.

use std::path::Path;

/// POSIX single-quote `s` for a remote shell (ssh always hands the remote
/// command to a shell).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The exact remote command `pull` runs to export one Source at its own
/// central Bookmark.
pub fn remote_export_command(
    remote_bin: &str,
    host_name: &str,
    source: &str,
    since: Option<i64>,
    source_root: Option<&Path>,
) -> String {
    let mut cmd = format!(
        "{} export --host {} --source {}",
        shell_quote(remote_bin),
        shell_quote(host_name),
        shell_quote(source),
    );
    if let Some(since) = since {
        cmd.push_str(&format!(" --since {since}"));
    }
    if let Some(root) = source_root {
        cmd.push_str(&format!(
            " --{source}-root {}",
            shell_quote(&root.to_string_lossy())
        ));
    }
    cmd
}

/// Arguments for one `ssh` invocation. BatchMode so cron never hangs on a
/// password prompt — auth comes from keys/agents or fails fast.
pub fn ssh_args(destination: &str, remote_command: &str) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        destination.into(),
        remote_command.into(),
    ]
}

/// Arguments for the `scp` deploying the binary (same BatchMode rationale).
pub fn scp_args(local_binary: &Path, destination: &str, remote_bin: &str) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        local_binary.to_string_lossy().into_owned(),
        format!("{destination}:{remote_bin}"),
    ]
}

/// The first contact with a remote: one round trip reporting its platform
/// (`uname -sm`, line 1) and its binary's version (line 2, absent when the
/// binary is missing or cannot execute). Plain `;` keeps it valid in any
/// login shell ssh may hand it to.
pub fn remote_probe_command(remote_bin: &str) -> String {
    format!("uname -sm; {} --version", shell_quote(remote_bin))
}

/// The remote command preparing the binary's directory before scp.
pub fn remote_prepare_command(remote_bin: &str) -> Option<String> {
    let (dir, _) = remote_bin.rsplit_once('/')?;
    Some(format!("mkdir -p {}", shell_quote(dir)))
}

/// What `--version` prints for the binary we would deploy.
pub fn expected_version_line() -> String {
    format!(
        "agent-sessions {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("ASESSIONS_BUILD_HASH")
    )
}
