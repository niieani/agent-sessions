//! One SSH Host's Pull: first contact (probe + deploy), then a
//! source-scoped remote Export per Source, ingested straight off ssh stdout.

use crate::repository_resolution::LookupResults;
use anyhow::{Context, Result, bail};

use super::collect::FleetPull;
use super::commands::remote_export_command;
use super::config::HostConfig;
use super::deploy::{Contact, ensure_remote_binary};
use crate::adapters::AVAILABLE_SOURCES;
use crate::archive::Archive;
use crate::bundle;
use crate::pull::{PullStats, SourcePullStats};

/// What happened when Pull tried an SSH Host.
pub(super) enum HostPull {
    Pulled(PullStats),
    Unreachable { detail: String },
}

/// Pull one SSH Host: first contact + deploy, source-scoped remote exports,
/// then central Bundle ingest. Each Source advances only after its stream
/// ends.
pub(super) fn pull_ssh_host(
    archive: &Archive,
    host: &HostConfig,
    pull: &FleetPull<'_>,
) -> Result<HostPull> {
    let destination = host.ssh.as_deref().expect("ssh host");
    if let Contact::Unreachable { detail } =
        ensure_remote_binary(pull.transport, host, pull.local_binary)?
    {
        return Ok(HostPull::Unreachable { detail });
    }
    let mut stats = collect_source_results(&host.name, |source| {
        pull_ssh_source(archive, host, pull, destination, source)
    });

    let refresh = crate::repository_resolution::refresh(
        archive,
        Some(&host.name),
        &RemoteResolver {
            transport: pull.transport,
            destination,
            binary: host.remote_bin(),
        },
    );
    stats.record_repository_refresh(&host.name, refresh);
    Ok(HostPull::Pulled(stats))
}

fn collect_source_results(
    host_name: &str,
    mut pull_one: impl FnMut(&str) -> Result<SourcePullStats>,
) -> PullStats {
    let mut totals = PullStats::default();
    for source in AVAILABLE_SOURCES {
        let started = std::time::Instant::now();
        let result = pull_one(source);
        let source_stats = match result {
            Ok(mut stats) => {
                stats.elapsed_ms = started.elapsed().as_millis();
                stats
            }
            Err(error) => {
                tracing::error!(
                    host = host_name,
                    source,
                    error = format!("{error:#}"),
                    "remote Source pull failed; continuing"
                );
                SourcePullStats {
                    host: host_name.to_string(),
                    source: source.to_string(),
                    sessions_failed: 1,
                    elapsed_ms: started.elapsed().as_millis(),
                    error: Some(format!("{error:#}")),
                    ..Default::default()
                }
            }
        };
        totals.add_source(source_stats);
    }
    totals
}

fn pull_ssh_source(
    archive: &Archive,
    host: &HostConfig,
    pull: &FleetPull<'_>,
    destination: &str,
    source: &str,
) -> Result<SourcePullStats> {
    let mut db = archive.db()?;
    let since = db.bookmark(&host.name, source)?;
    let mut export_cmd = remote_export_command(
        host.remote_bin(),
        &host.name,
        source,
        since,
        host.source_root_override(source),
    );
    export_cmd.push_str(" --skip-repository-lookup");
    let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
    for pattern in &config.pull.exclude_paths {
        export_cmd.push_str(" --exclude-path ");
        export_cmd.push_str(&super::commands::shell_quote(pattern));
    }
    tracing::info!(
        host = host.name,
        destination,
        source,
        cmd = export_cmd,
        "remote export"
    );

    let mut child = pull
        .transport
        .spawn(destination, &export_cmd)
        .context("spawning ssh export")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let ingested =
        bundle::ingest_bundle_unreconciled(std::io::BufReader::new(stdout), archive, &mut db);
    let status = child.wait().context("waiting for ssh export")?;
    let ingested = ingested?;
    if !status.success() && ingested.manifest.units_failed == 0 {
        bail!("remote {source} export exited with {status}");
    }
    validate_remote_manifest(&ingested.manifest, &host.name, source, since)?;
    let failed = ingested.manifest.units_failed + ingested.stats.sessions_failed;
    if failed == 0 && ingested.manifest.units_retry_later == 0 {
        for ((bundle_host, bundle_source), bookmark_ms) in &ingested.bookmarks {
            db.advance_bookmark(bundle_host, bundle_source, *bookmark_ms)?;
        }
    }
    Ok(SourcePullStats {
        host: host.name.clone(),
        source: source.to_string(),
        retention: ingested.manifest.retention.get(source).cloned(),
        sessions_found: ingested.stats.units_in_bundle
            + ingested.manifest.units_failed
            + ingested.manifest.units_retry_later,
        sessions_retry_later: ingested.manifest.units_retry_later,
        sessions_new: ingested.stats.sessions_new,
        sessions_extended: ingested.stats.sessions_extended,
        sessions_unchanged: ingested.stats.sessions_unchanged,
        sessions_revised: ingested.stats.sessions_revised,
        sessions_refused: ingested.stats.sessions_refused,
        sessions_failed: failed,
        conversations_extracted: ingested.stats.conversations_extracted,
        ..Default::default()
    })
}

fn validate_remote_manifest(
    manifest: &bundle::BundleManifest,
    host: &str,
    source: &str,
    since: Option<i64>,
) -> Result<()> {
    if manifest.host != host || manifest.sources != [source] {
        bail!(
            "remote Bundle does not match requested Host/Source: host {:?}, sources {:?}",
            manifest.host,
            manifest.sources
        );
    }
    if manifest.since_ms != since {
        bail!(
            "remote Bundle Bookmark basis {:?} does not match requested {:?}",
            manifest.since_ms,
            since
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_manifest_must_echo_requested_bookmark() {
        let manifest = bundle::BundleManifest {
            schema_version: bundle::BUNDLE_SCHEMA_VERSION,
            host: "laptop".into(),
            sources: vec!["claude".into()],
            since_ms: Some(42),
            unit_count: 0,
            units_failed: 0,
            units_retry_later: 0,
            exported_at_ms: 0,
            retention: Default::default(),
        };
        assert!(validate_remote_manifest(&manifest, "laptop", "claude", Some(42)).is_ok());
        assert!(validate_remote_manifest(&manifest, "laptop", "claude", Some(41)).is_err());
    }

    #[test]
    fn one_remote_source_error_keeps_completed_stats_and_continues() {
        let mut called = Vec::new();
        let totals = collect_source_results("laptop", |source| {
            called.push(source.to_string());
            if source == "codex" {
                anyhow::bail!("broken stream");
            }
            Ok(SourcePullStats {
                host: "laptop".into(),
                source: source.into(),
                sessions_found: 1,
                sessions_new: 1,
                ..Default::default()
            })
        });
        assert_eq!(called, AVAILABLE_SOURCES);
        assert_eq!(totals.sources.len(), AVAILABLE_SOURCES.len());
        assert_eq!(totals.sessions_new, AVAILABLE_SOURCES.len() - 1);
        assert_eq!(totals.sessions_failed, 1);
        assert!(totals.sources[1].error.is_some());
    }
}

struct RemoteResolver<'a> {
    transport: &'a dyn super::transport::RemoteTransport,
    destination: &'a str,
    binary: &'a str,
}
impl crate::repository_resolution::Resolver for RemoteResolver<'_> {
    fn lookup(&self, path: &str) -> Result<crate::repository_resolution::Lookup> {
        self.lookup_many(&[path.to_owned()])?
            .remove(path)
            .context("missing repository result")?
            .map_err(anyhow::Error::msg)
    }
    fn lookup_many(&self, paths: &[String]) -> Result<LookupResults> {
        Ok(self.lookup_many_before(
            paths,
            std::time::Instant::now() + crate::repository_resolution::HOST_LOOKUP_BUDGET,
        ))
    }
}
impl RemoteResolver<'_> {
    fn lookup_many_before(&self, paths: &[String], deadline: std::time::Instant) -> LookupResults {
        let prefix = format!(
            "{} resolve-repository --budget-ms 120000 --paths-json ",
            super::commands::shell_quote(self.binary)
        );
        // Include the JSON array delimiters and outer shell quotes. Count
        // serialized UTF-8 bytes and POSIX quote expansion, not path characters.
        let base_bytes = prefix.len() + 4;
        let mut bytes = base_bytes;
        let mut chunk = Vec::new();
        let mut answers = std::collections::BTreeMap::new();
        for path in paths {
            let encoded = serde_json::to_string(path).expect("serialize path");
            let entry_bytes = encoded.len() + 3 * encoded.bytes().filter(|b| *b == b'\'').count();
            if base_bytes + entry_bytes > REPOSITORY_COMMAND_LIMIT {
                answers.insert(
                    path.clone(),
                    Err("repository path exceeds the remote command byte limit".into()),
                );
                continue;
            }
            if bytes + usize::from(!chunk.is_empty()) + entry_bytes > REPOSITORY_COMMAND_LIMIT {
                answers.extend(self.lookup_chunk(&chunk, deadline));
                chunk.clear();
                bytes = base_bytes;
            }
            bytes += usize::from(!chunk.is_empty()) + entry_bytes;
            chunk.push(path.clone());
        }
        if !chunk.is_empty() {
            answers.extend(self.lookup_chunk(&chunk, deadline));
        }
        answers
    }
}

// Linux limits an individual argv entry to 128 KiB. The complete ssh command
// stays below 100 KiB, including JSON escaping, shell quoting and the binary.
const REPOSITORY_COMMAND_LIMIT: usize = 100 * 1024;
impl RemoteResolver<'_> {
    fn lookup_chunk(&self, paths: &[String], deadline: std::time::Instant) -> LookupResults {
        let run = || -> Result<LookupResults> {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            // Reserve startup/SSH cleanup time within the same Host budget.
            let remote_budget = remaining.saturating_sub(std::time::Duration::from_secs(2));
            anyhow::ensure!(
                !remote_budget.is_zero(),
                "Host repository lookup budget exhausted; retry later"
            );
            let command = format!(
                "{} resolve-repository --budget-ms {} --paths-json {}",
                super::commands::shell_quote(self.binary),
                remote_budget.as_millis(),
                super::commands::shell_quote(&serde_json::to_string(paths)?)
            );
            let output = self
                .transport
                .run_bounded(self.destination, &command, remaining)?;
            anyhow::ensure!(
                output.status.success(),
                "remote repository lookup failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let answers: LookupResults = serde_json::from_slice(&output.stdout)?;
            anyhow::ensure!(
                answers.len() == paths.len() && paths.iter().all(|p| answers.contains_key(p)),
                "repository batch response paths mismatch"
            );
            Ok(answers)
        };
        match run() {
            Ok(answers) => answers,
            Err(error) => paths
                .iter()
                .map(|p| (p.clone(), Err(format!("{error:#}"))))
                .collect(),
        }
    }
}

#[cfg(test)]
mod repository_chunks {
    use super::*;
    use crate::fleet::RemoteTransport;
    use crate::repository_resolution::{Lookup, Resolver};
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Child, ExitStatus, Output};

    #[derive(Default)]
    struct Transport {
        commands: RefCell<Vec<String>>,
        stall: bool,
    }
    impl RemoteTransport for Transport {
        fn run(&self, _: &str, command: &str) -> Result<Output> {
            assert!(command.len() <= REPOSITORY_COMMAND_LIMIT);
            self.commands.borrow_mut().push(command.to_owned());
            let quoted = command.split_once(" --paths-json ").unwrap().1;
            let json = quoted
                .strip_prefix('\'')
                .unwrap()
                .strip_suffix('\'')
                .unwrap()
                .replace("'\\''", "'");
            let paths: Vec<String> = serde_json::from_str(&json)?;
            let answers: std::collections::BTreeMap<_, _> = paths
                .into_iter()
                .map(|path| {
                    let answer = if path.contains("error") {
                        Err("one path failed".to_owned())
                    } else {
                        Ok(Lookup::Missing)
                    };
                    (path, answer)
                })
                .collect();
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: serde_json::to_vec(&answers)?,
                stderr: Vec::new(),
            })
        }
        fn run_bounded(
            &self,
            destination: &str,
            command: &str,
            limit: std::time::Duration,
        ) -> Result<Output> {
            if self.stall {
                std::thread::sleep(limit);
            }
            self.run(destination, command)
        }
        fn copy(&self, _: &Path, _: &str, _: &str) -> Result<Output> {
            unreachable!()
        }
        fn spawn(&self, _: &str, _: &str) -> Result<Child> {
            unreachable!()
        }
    }

    #[test]
    fn large_path_list_chunks_by_escaped_command_bytes_and_merges_every_answer() {
        let transport = Transport::default();
        let resolver = RemoteResolver {
            transport: &transport,
            destination: "host",
            binary: "/bin/with 'quotes'/agent-sessions",
        };
        let mut paths = (0..3000)
            .map(|i| format!("/root/{}-{i}", "é'\\\"".repeat(20)))
            .collect::<Vec<_>>();
        paths.push("/error/path".into());
        assert!(serde_json::to_string(&paths).unwrap().len() > 128 * 1024);
        let answers = resolver.lookup_many(&paths).unwrap();
        assert!(transport.commands.borrow().len() > 1);
        assert_eq!(answers.len(), paths.len());
        for path in paths {
            assert_eq!(
                answers[&path],
                if path.contains("error") {
                    Err("one path failed".into())
                } else {
                    Ok(Lookup::Missing)
                }
            );
        }
    }

    #[test]
    fn shared_remote_budget_stops_later_chunks_and_keeps_earlier_answers() {
        let transport = Transport {
            stall: true,
            ..Default::default()
        };
        let resolver = RemoteResolver {
            transport: &transport,
            destination: "host",
            binary: "agent-sessions",
        };
        let paths = (0..4)
            .map(|i| format!("/{i}{}", "x".repeat(60_000)))
            .collect::<Vec<_>>();
        let answers = resolver.lookup_many_before(
            &paths,
            std::time::Instant::now() + std::time::Duration::from_secs(3),
        );
        assert_eq!(transport.commands.borrow().len(), 1);
        assert_eq!(answers.len(), paths.len());
        assert_eq!(answers[&paths[0]], Ok(Lookup::Missing));
        assert!(
            paths[1..]
                .iter()
                .all(|p| answers[p].as_ref().unwrap_err().contains("budget"))
        );
        let command = &transport.commands.borrow()[0];
        assert!(command.contains("--budget-ms "));
    }

    #[test]
    fn empty_and_single_oversized_path_do_not_block_other_paths() {
        let transport = Transport::default();
        let resolver = RemoteResolver {
            transport: &transport,
            destination: "host",
            binary: "agent-sessions",
        };
        assert!(resolver.lookup_many(&[]).unwrap().is_empty());
        assert!(transport.commands.borrow().is_empty());
        let oversized = "'".repeat(REPOSITORY_COMMAND_LIMIT);
        let answers = resolver
            .lookup_many(&[oversized.clone(), "/small".into()])
            .unwrap();
        assert!(answers[&oversized].is_err());
        assert_eq!(answers["/small"], Ok(Lookup::Missing));
        assert_eq!(transport.commands.borrow().len(), 1);
    }
}
