//! Local pull pipeline: enumerate → capture → extract → upsert.
//!
//! Fleet transport lives in [`crate::fleet`]. This module knows only how to
//! ingest local Stores into an Archive; it never opens a network connection.

mod config;
pub mod retention;
pub(crate) mod selection;
pub use config::PullConfig;
pub use retention::{StoreRetention, store_retention_warnings, warn_store_retention};

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::adapters::{AVAILABLE_SOURCES, Adapter, CaptureOutcome, EnumerateGate, adapter_for};
use crate::archive::Archive;

/// Which Store roots to read on a Host. Injectable so tests and unusual
/// installations can point at any directory. A missing root is empty.
#[derive(Debug, Clone)]
pub struct StoreRoots {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub pi: PathBuf,
    pub cursor: PathBuf,
    pub t3: PathBuf,
    pub chatgpt: PathBuf,
    /// Whether the T3 Store base was explicitly configured. A missing
    /// default Store is normal; an explicit bad root is a Source failure.
    pub t3_explicit: bool,
}

impl Default for StoreRoots {
    fn default() -> Self {
        Self {
            claude: crate::adapters::claude::default_store_root(),
            codex: crate::adapters::codex::default_store_root(),
            pi: crate::adapters::pi::default_store_root(),
            cursor: crate::adapters::cursor::default_store_root(),
            t3: crate::adapters::t3::default_store_root(),
            chatgpt: PathBuf::from(std::env::var_os("HOME").expect("HOME required"))
                .join(".local/share/chatgpt-tools/captures"),
            t3_explicit: false,
        }
    }
}

impl StoreRoots {
    /// Root for an Adapter in [`AVAILABLE_SOURCES`]. Unknown Sources are not
    /// routable and must fail at their caller's validation boundary.
    pub fn for_source(&self, source: &str) -> Option<&Path> {
        match source {
            crate::adapters::claude::SOURCE => Some(&self.claude),
            crate::adapters::codex::SOURCE => Some(&self.codex),
            crate::adapters::pi::SOURCE => Some(&self.pi),
            crate::adapters::cursor::SOURCE => Some(&self.cursor),
            crate::adapters::t3::SOURCE => Some(&self.t3),
            "chatgpt" => Some(&self.chatgpt),
            _ => None,
        }
    }

    pub fn source_is_explicit(&self, source: &str) -> bool {
        source == crate::adapters::t3::SOURCE && self.t3_explicit
    }
}

/// Outcome of one local or remote Host pull.
#[derive(Debug, Default)]
pub struct PullStats {
    pub repository_refresh: Vec<RepositoryRefreshReport>,
    /// Sessions past the enumerate gate (Bookmark + matching sidecar).
    pub sessions_found: usize,
    /// Captures freshly written (new Sessions, or grown ones superseded).
    pub sessions_captured: usize,
    pub sessions_new: usize,
    pub sessions_extended: usize,
    /// Sessions whose existing Capture matched the native file and was
    /// reused, not rewritten.
    pub sessions_unchanged: usize,
    /// Captures superseded by a diverged Dialog; each prior body is kept as a
    /// retained Prior Revision. Reported, never a failure.
    pub sessions_revised: usize,
    /// Incoming payloads refused because they lost Dialog the Capture holds.
    /// Each also counts in `sessions_failed`.
    pub sessions_refused: usize,
    pub sessions_failed: usize,
    pub sessions_retry_later: usize,
    pub conversations_extracted: usize,
    pub sources: Vec<SourcePullStats>,
}

#[derive(Debug, Clone)]
pub struct RepositoryRefreshReport {
    pub retry_later: usize,
    pub retry_sample: Option<String>,
    pub host: String,
    pub units_failed: usize,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct SourcePullStats {
    pub retention: Option<StoreRetention>,
    pub host: String,
    pub source: String,
    pub sessions_found: usize,
    pub sessions_new: usize,
    pub sessions_extended: usize,
    pub sessions_unchanged: usize,
    pub sessions_revised: usize,
    pub sessions_refused: usize,
    pub sessions_failed: usize,
    pub sessions_retry_later: usize,
    pub conversations_extracted: usize,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}

impl PullStats {
    pub fn record_repository_refresh(
        &mut self,
        host: &str,
        result: Result<crate::repository_resolution::RefreshStats>,
    ) {
        let report = match result {
            Ok(stats) => RepositoryRefreshReport {
                host: host.into(),
                retry_later: stats.retry_later,
                retry_sample: stats.retry_sample,
                units_failed: stats.units_failed,
                error: None,
            },
            Err(error) => {
                tracing::error!(
                    host,
                    error = format!("{error:#}"),
                    "repository refresh failed after Source collection"
                );
                RepositoryRefreshReport {
                    host: host.into(),
                    units_failed: 0,
                    retry_later: 0,
                    retry_sample: None,
                    error: Some(format!("{error:#}")),
                }
            }
        };
        if report.units_failed > 0 || report.error.is_some() || report.retry_later > 0 {
            self.repository_refresh.push(report);
        }
    }
    pub fn repository_refresh_failed(&self) -> bool {
        self.repository_refresh
            .iter()
            .any(|r| r.units_failed > 0 || r.error.is_some())
    }

    pub fn add_source(&mut self, source: SourcePullStats) {
        self.sessions_found += source.sessions_found;
        self.sessions_new += source.sessions_new;
        self.sessions_extended += source.sessions_extended;
        self.sessions_captured +=
            source.sessions_new + source.sessions_extended + source.sessions_revised;
        self.sessions_unchanged += source.sessions_unchanged;
        self.sessions_revised += source.sessions_revised;
        self.sessions_refused += source.sessions_refused;
        self.sessions_failed += source.sessions_failed;
        self.sessions_retry_later += source.sessions_retry_later;
        self.conversations_extracted += source.conversations_extracted;
        self.sources.push(source);
    }

    pub fn merge(&mut self, other: PullStats) {
        for source in other.sources {
            self.add_source(source);
        }
        self.repository_refresh.extend(other.repository_refresh);
    }
}

/// Pull one local Host's Stores into the Archive.
///
/// Incremental via the per-(Host, Source) Bookmark: Enumerate skips a file
/// only when it is at or below the Bookmark and its Capture sidecar records
/// the same mtime + size. A Bookmark advances only after its entire Source
/// completed, leaving a failed Source's retry window open.
pub fn pull_localhost(archive: &Archive, roots: &StoreRoots, host: &str) -> Result<PullStats> {
    pull_localhost_with_resolver(
        archive,
        roots,
        host,
        &crate::repository_resolution::GitResolver,
    )
}

pub fn pull_localhost_with_resolver(
    archive: &Archive,
    roots: &StoreRoots,
    host: &str,
    resolver: &dyn crate::repository_resolution::Resolver,
) -> Result<PullStats> {
    let _writer_lock = archive.acquire_writer_lock()?;
    crate::origins::preserve_current(archive, &archive.db()?)?;
    crate::pull::selection::reconcile_index(archive)?;
    let stats = pull_localhost_locked_with_resolver(archive, roots, host, resolver)?;
    crate::origins::reconcile(archive, &mut archive.db()?)?;
    let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
    warn_store_retention(&stats, config.pull.warn_store_retention);
    Ok(stats)
}

/// Pull one local Host while the caller holds the Archive writer lock.
///
/// Fleet collection holds one guard across every Host so that no other CLI
/// mutation can interleave with any Capture commit or SQLite update.
pub(crate) fn pull_localhost_locked(
    archive: &Archive,
    roots: &StoreRoots,
    host: &str,
) -> Result<PullStats> {
    pull_localhost_locked_with_resolver(
        archive,
        roots,
        host,
        &crate::repository_resolution::GitResolver,
    )
}
fn pull_localhost_locked_with_resolver(
    archive: &Archive,
    roots: &StoreRoots,
    host: &str,
    resolver: &dyn crate::repository_resolution::Resolver,
) -> Result<PullStats> {
    let swept = archive.sweep_capture_temps()?;
    if swept > 0 {
        tracing::warn!(files = swept, "removed stale Capture temp files");
    }
    let mut db = archive.db()?;

    let mut stats = PullStats::default();

    for source in AVAILABLE_SOURCES {
        let adapter = adapter_for(source).expect("available Source has an Adapter");
        let root = roots
            .for_source(source)
            .expect("available Source has a Store root");
        let started = Instant::now();
        let source_stats = match validate_store_root(source, root, roots.source_is_explicit(source))
            .and_then(|available| {
                if available {
                    ingest_source(&mut db, archive, adapter, root, host)
                } else {
                    Ok(SourcePullStats {
                        host: host.to_string(),
                        source: source.to_string(),
                        ..Default::default()
                    })
                }
            }) {
            Ok(mut source_stats) => {
                source_stats.elapsed_ms = started.elapsed().as_millis();
                source_stats
            }
            Err(error) => {
                tracing::error!(
                    host,
                    source,
                    error = format!("{error:#}"),
                    "Source ingest failed"
                );
                SourcePullStats {
                    host: host.to_string(),
                    source: source.to_string(),
                    sessions_failed: 1,
                    elapsed_ms: started.elapsed().as_millis(),
                    error: Some(format!("{error:#}")),
                    ..Default::default()
                }
            }
        };
        stats.add_source(source_stats);
    }

    stats.record_repository_refresh(
        host,
        crate::repository_resolution::refresh(archive, Some(host), resolver),
    );

    tracing::info!(
        found = stats.sessions_found,
        captured = stats.sessions_captured,
        unchanged = stats.sessions_unchanged,
        revised = stats.sessions_revised,
        refused = stats.sessions_refused,
        failed = stats.sessions_failed,
        retry_later = stats.sessions_retry_later,
        extracted = stats.conversations_extracted,
        "pull complete"
    );
    Ok(stats)
}

pub(crate) fn validate_store_root(source: &str, root: &Path, explicit: bool) -> Result<bool> {
    if source != crate::adapters::t3::SOURCE {
        return Ok(true);
    }
    let store = root.join(crate::adapters::t3::STORE_PATH);
    if store.is_file() {
        return Ok(true);
    }
    if root.join("userdata/state.sqlite").is_file() {
        anyhow::bail!(
            "incompatible T3 Store: expected orchestration v2 Store at {}; legacy state.sqlite is not authoritative",
            store.display()
        );
    }
    if explicit {
        anyhow::bail!("explicit T3 root has no Store at {}", store.display());
    }
    Ok(false)
}

fn ingest_source(
    db: &mut crate::archive::Db,
    archive: &Archive,
    adapter: &dyn Adapter,
    root: &Path,
    host: &str,
) -> Result<SourcePullStats> {
    let captures_dir = &archive.captures_dir();
    let mut stats = SourcePullStats {
        host: host.to_string(),
        source: adapter.source().to_string(),
        retention: adapter.store_retention(root)?,
        ..Default::default()
    };
    let since = db.bookmark(host, adapter.source())?;
    let gate = EnumerateGate::load(captures_dir, host, adapter.source(), since)?;
    let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
    let selected = selection::enumerate(adapter, root, &gate, &config.pull.exclude_paths)
        .with_context(|| format!("enumerating {} store", adapter.source()))?;
    stats.sessions_failed = selected.failed.len();
    for (session, error) in &selected.failed {
        tracing::warn!(
            host,
            source = adapter.source(),
            session,
            error,
            "Session admission failed; continuing"
        );
    }
    stats.sessions_retry_later = selected.retry_later.len();
    for (session, error) in &selected.retry_later {
        tracing::warn!(
            host,
            source = adapter.source(),
            session,
            error,
            "Session admission retry-later after 3 attempts"
        );
    }
    let refs = selected.sessions;
    stats.sessions_found = refs.len() + stats.sessions_retry_later + stats.sessions_failed;
    tracing::info!(
        source = adapter.source(),
        sessions = refs.len(),
        since,
        "enumerated"
    );

    let mut captured_bookmark: Option<i64> = None;
    let mut captures = Vec::with_capacity(refs.len());
    for (index, session) in refs.iter().enumerate() {
        if index == 0 || (index + 1) % 100 == 0 {
            tracing::info!(
                host,
                source = adapter.source(),
                processed = index,
                total = refs.len(),
                "ingest progress"
            );
        }
        let result = (|| -> Result<()> {
            let mut capture = adapter
                .capture(session, captures_dir, host)
                .with_context(|| format!("capturing {}", session.native_id))?;
            anyhow::ensure!(
                !selection::excluded_capture(adapter, &capture, &config.pull.exclude_paths)?,
                "settled Capture is excluded by pull.exclude-paths; eligible native copy could not replace it"
            );
            for superseded in &capture.meta.superseded_native_ids {
                anyhow::ensure!(
                    adapter.validates_superseded_identity(&capture.meta.native_id, superseded),
                    "invalid superseded {} identity {superseded:?} for {:?}",
                    adapter.source(),
                    capture.meta.native_id
                );
                db.retire_session_identity(adapter.source(), superseded)?;
            }
            db.record_capture_presence(&capture.meta)?;
            captured_bookmark = Some(
                captured_bookmark.map_or(capture.meta.bookmark_mtime_ms(), |current| {
                    current.max(capture.meta.bookmark_mtime_ms())
                }),
            );
            // Make the retry obligation durable before any fallible derived
            // work begins. A crash during Extract/upsert leaves this marker
            // set, so an old-mtime Session cannot hide below its Bookmark.
            capture.set_needs_retry(true)?;
            captures.push((session.native_id.clone(), capture));
            Ok(())
        })();
        if let Err(error) = result {
            if error.is::<crate::adapters::BusyCapture>() {
                stats.sessions_retry_later += 1;
                tracing::warn!(
                    host,
                    source = adapter.source(),
                    session = session.native_id,
                    error = format!("{error:#}"),
                    "Session capture retry-later after 3 attempts"
                );
                continue;
            }
            stats.sessions_failed += 1;
            tracing::warn!(
                host,
                source = adapter.source(),
                session = session.native_id,
                error = format!("{error:#}"),
                "Session capture failed; continuing"
            );
        }
    }

    // Extraction starts only after every candidate Capture has settled. A
    // child may sort before its direct parent, so this Archive-owned phase is
    // the first point at which parent-dependent extraction is order-free.
    let extraction_context = adapter.prepare_extraction(archive.root())?;
    for (native_id, mut capture) in captures {
        let outcome = capture.outcome;
        let result =
            (|| -> Result<bool> {
                adapter.archive_media(&capture, archive.root())?;
                let extracted = crate::archive::extract_capture(
                    adapter,
                    &capture,
                    captures_dir,
                    &extraction_context,
                )
                .with_context(|| format!("extracting {native_id}"))?;
                let conversation_extracted =
                    match extracted {
                        Some(crate::archive::ExtractedCapture::Conversations(conversations)) => {
                            stats.conversations_extracted += extraction_context
                                .store_conversations(db, &conversations, &capture.meta)?;
                            true
                        }
                        Some(crate::archive::ExtractedCapture::Orchestrator(projection)) => {
                            db.upsert_orchestrator_projection(&projection)?;
                            false
                        }
                        None => {
                            db.upsert_nonextractable_from_capture(&capture.meta)?;
                            false
                        }
                    };
                capture.set_needs_retry(false)?;
                Ok(conversation_extracted)
            })();
        match result {
            Ok(_) => match outcome {
                CaptureOutcome::New => stats.sessions_new += 1,
                CaptureOutcome::Extended => stats.sessions_extended += 1,
                CaptureOutcome::Revised => stats.sessions_revised += 1,
                CaptureOutcome::Refused => {
                    stats.sessions_refused += 1;
                    stats.sessions_failed += 1;
                }
                CaptureOutcome::Unchanged => stats.sessions_unchanged += 1,
            },
            Err(error) => {
                if outcome == CaptureOutcome::Refused {
                    stats.sessions_refused += 1;
                }
                stats.sessions_failed += 1;
                tracing::warn!(
                    host,
                    source = adapter.source(),
                    session = native_id,
                    error = format!("{error:#}"),
                    "Session extraction failed; continuing"
                );
            }
        }
    }

    db.apply_native_projects(adapter.source(), &extraction_context)?;
    db.resolve_subagent_links()?;
    db.resolve_orchestrator_links()?;
    crate::archive::log_orchestrator_fidelity(db)?;

    if stats.sessions_failed == 0
        && stats.sessions_retry_later == 0
        && let Some(max_mtime) = captured_bookmark
    {
        db.advance_bookmark(host, adapter.source(), max_mtime)?;
    }
    Ok(stats)
}

/// The name of the Host we are running on (Provenance for localhost pulls).
pub fn local_hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

#[cfg(test)]
mod busy_capture_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn busy_local_and_export_preserve_bookmark_and_are_nonfatal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects/-fixture");
        std::fs::create_dir_all(&root).unwrap();
        let id = "11111111-1111-4111-8111-111111111111";
        let native = root.join(format!("{id}.jsonl"));
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/claude/projects/-workspace-Projects-example")
                .join(format!("{id}.jsonl")),
            &native,
        )
        .unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let adapter = adapter_for("claude").unwrap();
        let mut db = archive.db().unwrap();
        let store = root.parent().unwrap();
        let first = ingest_source(&mut db, &archive, adapter, store, "fixture").unwrap();
        assert_eq!(first.sessions_new, 1);
        let bookmark = db.bookmark("fixture", "claude").unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&native)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let target = native.clone();
        crate::adapters::file_capture::AFTER_STREAM.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&target)
                    .unwrap()
                    .write_all(b"\n")
                    .unwrap();
            }))
        });
        let stats = ingest_source(&mut db, &archive, adapter, store, "fixture").unwrap();
        assert_eq!(stats.sessions_retry_later, 1);
        assert_eq!(stats.sessions_failed, 0);
        assert_eq!(db.bookmark("fixture", "claude").unwrap(), bookmark);
        let mut fleet = crate::fleet::FleetStats::default();
        fleet.totals.add_source(stats);
        assert!(!fleet.failed());
        assert!(
            crate::fleet::summary_lines(&fleet, 1)
                .last()
                .unwrap()
                .contains("1 retry-later, 0 failed")
        );
        let roots = StoreRoots {
            claude: store.into(),
            codex: dir.path().join("missing-codex"),
            pi: dir.path().join("missing-pi"),
            cursor: dir.path().join("missing-cursor"),
            t3: dir.path().join("missing-t3"),
            chatgpt: dir.path().join("missing-chatgpt"),
            t3_explicit: false,
        };
        let mut bytes = Vec::new();
        let stats =
            crate::bundle::export(&mut bytes, &roots, "fixture", Some("claude"), None).unwrap();
        crate::adapters::file_capture::AFTER_STREAM.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(stats.sessions_retry_later, 1);
        assert_eq!(stats.sessions_failed, 0);
        let ingested = crate::bundle::ingest_bundle(bytes.as_slice(), &archive, &mut db).unwrap();
        assert_eq!(ingested.manifest.units_retry_later, 1);
        assert_eq!(ingested.manifest.units_failed, 0);
        let recovered = ingest_source(&mut db, &archive, adapter, store, "fixture").unwrap();
        assert_eq!(recovered.sessions_failed, 0);
        assert_eq!(recovered.sessions_retry_later, 0);
        assert_eq!(recovered.sessions_found, 1);
    }
}
