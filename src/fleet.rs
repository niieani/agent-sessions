//! Fleet transport: configuration, SSH self-deploy, and per-Host isolation.
//!
//! Local ingestion lives in [`crate::pull`]. This module is the IO boundary
//! around it: a remote Source exports a Bundle over SSH stdout; the central
//! Archive ingests that Bundle and owns every Bookmark. Remotes are reached
//! only through a [`RemoteTransport`].

mod collect;
mod commands;
mod config;
mod deploy;
mod platform;
mod reachability;
mod remote;
mod summary;
mod transport;
mod view_config;

pub use collect::{FleetPull, FleetStats, pull_hosts};
pub use commands::{
    expected_version_line, remote_export_command, remote_prepare_command, remote_probe_command,
    scp_args, shell_quote, ssh_args,
};
pub use config::{
    DEFAULT_REMOTE_BIN, DigestConfig, DigestPromptConfig, FleetConfig, HostConfig, MediaConfig,
    TokenizerPromptFormat,
};
pub use platform::{BUILD_TARGET, Platform, RELEASE_TARGETS};
pub use reachability::UnreachableHost;
pub use summary::summary_lines;
pub use transport::{RemoteTransport, SystemSsh};
pub use view_config::{
    MessageConditions, OneOrMany, PathConditions, ViewConfig, ViewKindConfig, ViewLayoutConfig,
    ViewMatchConfig, ViewUnit,
};
