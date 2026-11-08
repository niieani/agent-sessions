//! Platform identity for self-deploy. The central binary may replace a
//! remote's binary only when the remote's `uname -sm` names the platform the
//! central binary was built for; every other remote needs its own release
//! asset installed at `remote-bin`.

use std::fmt;

use anyhow::{Result, bail};

/// The target triple this binary was compiled for (set by `build.rs`).
pub const BUILD_TARGET: &str = env!("ASESSIONS_BUILD_TARGET");

/// Target triples published as release assets (see RELEASING.md).
pub const RELEASE_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-musl"];

/// An operating system and CPU architecture, normalized so that a target
/// triple and `uname -sm` output compare equal for the same machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    os: String,
    arch: String,
}

impl Platform {
    /// The platform of a Rust target triple, e.g. `x86_64-unknown-linux-musl`.
    pub fn from_target(triple: &str) -> Result<Self> {
        let Some((arch, rest)) = triple.split_once('-') else {
            bail!("malformed target triple {triple:?}");
        };
        let os = if rest.ends_with("apple-darwin") {
            "darwin"
        } else if rest.split('-').any(|part| part == "linux") {
            "linux"
        } else {
            bail!("target {triple:?} is not a supported fleet platform");
        };
        Ok(Self {
            os: os.into(),
            arch: normalize_arch(arch),
        })
    }

    /// The platform `uname -sm` reports, e.g. `Darwin arm64`.
    pub fn from_uname(line: &str) -> Result<Self> {
        let mut parts = line.split_whitespace();
        let (Some(os), Some(arch), None) = (parts.next(), parts.next(), parts.next()) else {
            bail!("unexpected `uname -sm` output {line:?}");
        };
        Ok(Self {
            os: os.to_ascii_lowercase(),
            arch: normalize_arch(arch),
        })
    }

    /// The platform this binary was built for.
    pub fn build() -> Result<Self> {
        Self::from_target(BUILD_TARGET)
    }

    /// The published release asset target for this platform, if any.
    pub fn release_target(&self) -> Option<&'static str> {
        RELEASE_TARGETS
            .into_iter()
            .find(|target| Self::from_target(target).is_ok_and(|platform| platform == *self))
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.os, self.arch)
    }
}

fn normalize_arch(arch: &str) -> String {
    match arch {
        "arm64" | "aarch64" => "aarch64".into(),
        "x86_64" | "amd64" => "x86_64".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// Why the central binary must not replace the binary of a remote on
/// `remote` platform: what to install there instead.
pub fn cross_platform_refusal(remote: &Platform, remote_bin: &str) -> String {
    let asset = match remote.release_target() {
        Some(target) => format!("the {target} release asset"),
        None => format!("a {remote} build (no release asset is published for it)"),
    };
    format!(
        "remote platform is {remote} but the central binary is {BUILD_TARGET} and cannot \
         self-deploy across platforms; install {asset} of this release at {remote_bin} on the remote"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uname_and_target_triples_compare_by_os_and_arch() {
        let mac = Platform::from_target("aarch64-apple-darwin").unwrap();
        assert_eq!(Platform::from_uname("Darwin arm64").unwrap(), mac);
        let linux = Platform::from_target("x86_64-unknown-linux-musl").unwrap();
        assert_eq!(Platform::from_uname("Linux x86_64\n").unwrap(), linux);
        assert_eq!(
            Platform::from_target("x86_64-unknown-linux-gnu").unwrap(),
            linux
        );
        assert_ne!(mac, linux);
        assert_ne!(Platform::from_uname("Linux aarch64").unwrap(), linux);
    }

    #[test]
    fn malformed_platform_evidence_is_rejected() {
        assert!(Platform::from_uname("").is_err());
        assert!(Platform::from_uname("Linux").is_err());
        assert!(Platform::from_uname("bash: uname: command not found").is_err());
        assert!(Platform::from_target("x86_64-pc-windows-msvc").is_err());
    }

    #[test]
    fn build_platform_is_known_and_release_targets_resolve() {
        Platform::build().unwrap();
        assert_eq!(
            Platform::from_uname("Linux x86_64")
                .unwrap()
                .release_target(),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            Platform::from_uname("Darwin arm64")
                .unwrap()
                .release_target(),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            Platform::from_uname("Linux aarch64")
                .unwrap()
                .release_target(),
            None
        );
    }
}
