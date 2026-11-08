//! First contact with an SSH Host: the platform + version probe, Unreachable
//! detection, and platform-safe self-deploy of the central binary.

use std::path::Path;

use anyhow::{Context, Result, bail};

use super::commands::{
    expected_version_line, remote_prepare_command, remote_probe_command, shell_quote,
};
use super::config::HostConfig;
use super::platform::{Platform, cross_platform_refusal};
use super::transport::RemoteTransport;

/// The outcome of first contact with an SSH Host.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Contact {
    /// The remote ran the probe and holds a binary matching the central one.
    Ready,
    /// ssh could not connect; `detail` is ssh's own message.
    Unreachable { detail: String },
}

/// ssh's message when it failed to connect before any remote command ran:
/// exit 255 plus OpenSSH's connect/resolve diagnostics. Any other failure
/// (authentication, host key, a remote command's own status) is not
/// Unreachable.
pub(super) fn connection_failure(exit_code: Option<i32>, stderr: &str) -> Option<String> {
    if exit_code != Some(255) {
        return None;
    }
    stderr
        .lines()
        .map(str::trim)
        .find(|line| {
            line.starts_with("ssh: connect to host ")
                || line.starts_with("ssh: Could not resolve hostname ")
        })
        .map(str::to_string)
}

struct Probe {
    platform: Platform,
    version: String,
    succeeded: bool,
}

fn parse_probe(output: &std::process::Output) -> Result<Probe> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let platform = lines
        .next()
        .context("remote printed nothing")
        .and_then(Platform::from_uname)
        .with_context(|| {
            format!(
                "remote platform probe failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
    Ok(Probe {
        platform,
        version: lines.next().unwrap_or_default().trim().to_string(),
        succeeded: output.status.success(),
    })
}

/// Probe the remote and make its binary match the central one. The central
/// binary is copied only to a remote on the platform it was built for; any
/// other remote keeps its binary untouched and the Host fails explicitly.
/// The caller injects `local_binary` so tests deploy the real CLI.
pub(super) fn ensure_remote_binary(
    transport: &dyn RemoteTransport,
    host: &HostConfig,
    local_binary: &Path,
) -> Result<Contact> {
    let destination = host.ssh.as_deref().expect("ssh host");
    let remote_bin = host.remote_bin();
    let probe_cmd = remote_probe_command(remote_bin);
    let expected = expected_version_line();

    let output = transport.run(destination, &probe_cmd)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(detail) = connection_failure(output.status.code(), &stderr) {
        return Ok(Contact::Unreachable { detail });
    }
    let probe = parse_probe(&output)?;
    if probe.succeeded && probe.version == expected {
        return Ok(Contact::Ready);
    }
    let central = Platform::build()?;
    if probe.platform != central {
        bail!(
            "{remote_bin} reports {:?}, not {expected:?}; {}",
            probe.version,
            cross_platform_refusal(&probe.platform, remote_bin)
        );
    }
    tracing::info!(
        host = host.name,
        remote_bin,
        reported = probe.version,
        expected,
        platform = %probe.platform,
        "deploying binary to remote"
    );
    deploy(transport, destination, remote_bin, local_binary)?;

    let verify = parse_probe(&transport.run(destination, &probe_cmd)?)?;
    if !verify.succeeded || verify.version != expected {
        bail!(
            "remote binary still wrong after deploy (reported {:?}, expected {:?})",
            verify.version,
            expected,
        );
    }
    Ok(Contact::Ready)
}

fn deploy(
    transport: &dyn RemoteTransport,
    destination: &str,
    remote_bin: &str,
    local_binary: &Path,
) -> Result<()> {
    if let Some(prepare) = remote_prepare_command(remote_bin) {
        let out = transport.run(destination, &prepare)?;
        if !out.status.success() {
            bail!(
                "preparing remote directory failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let copy = transport.copy(local_binary, destination, remote_bin)?;
    if !copy.status.success() {
        bail!(
            "scp deploy failed: {}",
            String::from_utf8_lossy(&copy.stderr).trim()
        );
    }
    let chmod = transport.run(
        destination,
        &format!("chmod +x {}", shell_quote(remote_bin)),
    )?;
    if !chmod.status.success() {
        bail!(
            "chmod +x on remote binary failed: {}",
            String::from_utf8_lossy(&chmod.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::connection_failure;

    #[test]
    fn only_ssh_connect_failures_are_unreachable() {
        for stderr in [
            "ssh: connect to host 192.0.2.10 port 22: Operation timed out",
            "ssh: connect to host dev-mac port 22: Connection refused",
            "ssh: connect to host 10.0.0.2 port 22: No route to host",
            "ssh: connect to host 10.0.0.2 port 22: Host is down",
            "ssh: Could not resolve hostname dev-linux.example.ts.net: nodename nor servname provided, or not known",
        ] {
            assert_eq!(
                connection_failure(Some(255), &format!("{stderr}\n")).as_deref(),
                Some(stderr)
            );
        }
        for stderr in [
            "user@host: Permission denied (publickey).",
            "Host key verification failed.",
            "kex_exchange_identification: Connection closed by remote host",
        ] {
            assert_eq!(connection_failure(Some(255), stderr), None, "{stderr}");
        }
        let connect = "ssh: connect to host h port 22: Connection refused";
        assert_eq!(connection_failure(Some(1), connect), None);
        assert_eq!(connection_failure(None, connect), None);
    }
}
