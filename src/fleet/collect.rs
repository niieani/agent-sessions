//! Fleet Pull across every configured Host, with per-Host isolation and
//! Unreachable Host judgement.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use super::config::{FleetConfig, HostConfig};
use super::reachability::{UnreachableHost, judge};
use super::remote::{HostPull, pull_ssh_host};
use super::transport::RemoteTransport;
use crate::adapters::AVAILABLE_SOURCES;
use crate::archive::Archive;
use crate::pull::{PullStats, SourcePullStats, pull_localhost_locked};
use crate::timefmt::{format_elapsed, frontmatter};

/// How one fleet Pull reaches its remotes.
pub struct FleetPull<'a> {
    pub transport: &'a dyn RemoteTransport,
    /// The binary self-deploy copies to same-platform remotes. Injected so
    /// integration tests deploy the real CLI, never their test harness.
    pub local_binary: &'a Path,
    /// Reopen this Source's Bookmarks on every Host before pulling.
    pub full_source: Option<&'a str>,
}

/// Outcome of a fleet pull across all configured Hosts.
#[derive(Debug, Default)]
pub struct FleetStats {
    pub hosts_pulled: usize,
    /// Hosts that failed (deploy, export or ingest error, or Unreachable
    /// beyond the Unreachable Grace). Failures are isolated: they never
    /// abort the pull for other Hosts.
    pub hosts_failed: usize,
    pub failed_hosts: Vec<String>,
    /// Every Unreachable Host, within its grace or not.
    pub unreachable_hosts: Vec<UnreachableHost>,
    pub totals: PullStats,
}

impl FleetStats {
    /// Whether `pull` must exit nonzero.
    pub fn failed(&self) -> bool {
        self.hosts_failed > 0
            || self.totals.sessions_failed > 0
            || self.totals.repository_refresh_failed()
    }
}

/// Pull every configured Host under one Archive writer lock. Per-Host
/// failures are logged and collected; the caller decides the exit status.
pub fn pull_hosts(
    archive: &Archive,
    hosts: &[HostConfig],
    pull: &FleetPull<'_>,
) -> Result<FleetStats> {
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let _writer_lock = archive.acquire_writer_lock()?;
    archive.db()?.sync_host_pull_health()?;
    crate::origins::preserve_current(archive, &archive.db()?)?;
    crate::pull::selection::reconcile_index(archive)?;
    if let Some(source) = pull.full_source {
        anyhow::ensure!(
            AVAILABLE_SOURCES.contains(&source),
            "unknown full Source {source:?}"
        );
        let db = archive.db()?;
        for host in hosts {
            db.reset_bookmark(&host.name, source)?;
        }
    }
    let mut fleet = FleetStats::default();
    for host in hosts {
        let grace_text = host.unreachable_grace(&config.pull);
        let grace = crate::timefmt::parse_duration(grace_text)?;
        archive.db()?.track_host(&host.name, now_ms())?;
        let result = match &host.ssh {
            None => pull_localhost_locked(archive, &host.store_roots(), &host.name)
                .map(HostPull::Pulled),
            Some(_) => pull_ssh_host(archive, host, pull),
        };
        match result {
            Ok(HostPull::Pulled(stats)) => {
                if stats.sessions_failed == 0 && !stats.repository_refresh_failed() {
                    archive
                        .db()?
                        .record_host_pull_success(&host.name, now_ms())?;
                }
                fleet.hosts_pulled += 1;
                fleet.totals.merge(stats);
            }
            Ok(HostPull::Unreachable { detail }) => {
                let record = archive
                    .db()?
                    .host_pull_record(&host.name)?
                    .expect("Host tracked before its Pull");
                let unreachable = judge(&host.name, detail, record, now_ms(), grace);
                report_unreachable(&unreachable, grace_text);
                if unreachable.failure.is_some() {
                    fleet.hosts_failed += 1;
                    fleet.failed_hosts.push(host.name.clone());
                }
                fleet.unreachable_hosts.push(unreachable);
            }
            Err(error) => {
                let error = format!("{error:#}");
                tracing::error!(host = host.name, error, "host pull failed");
                fleet.hosts_failed += 1;
                fleet.failed_hosts.push(host.name.clone());
                for source in AVAILABLE_SOURCES {
                    fleet.totals.add_source(SourcePullStats {
                        host: host.name.clone(),
                        source: source.to_string(),
                        sessions_failed: 1,
                        error: Some(error.clone()),
                        ..Default::default()
                    });
                }
            }
        }
    }
    crate::origins::reconcile(archive, &mut archive.db()?)?;
    crate::pull::warn_store_retention(&fleet.totals, config.pull.warn_store_retention);
    Ok(fleet)
}

fn report_unreachable(host: &UnreachableHost, grace: &str) {
    let last_success = host.last_success_ms.map_or("never".into(), frontmatter);
    let unpulled_for = format_elapsed(host.unpulled_for_ms);
    match &host.failure {
        Some(failure) => tracing::error!(
            host = host.host,
            last_success,
            unpulled_for,
            grace,
            error = host.detail,
            "{failure}"
        ),
        None => tracing::warn!(
            host = host.host,
            last_success,
            unpulled_for,
            grace,
            error = host.detail,
            "host unreachable; skipped, Bookmarks unchanged"
        ),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the Unix epoch")
        .as_millis()
        .try_into()
        .expect("epoch milliseconds fit i64")
}
