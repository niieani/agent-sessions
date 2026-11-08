//! The Archive: capture file store plus the regenerable SQLite database.
//!
//! Two layers with opposite change rates:
//!
//! - `captures/<host>/<source>/<native-id>.<ext>.zst` plus a
//!   `<native-id>.meta.json` Provenance sidecar — the only source of truth
//!   (ADR-0001). Native ids may contain `/` (Claude subagents); the segments
//!   become subdirectories. A diverged supersede keeps the replaced body as
//!   a Prior Revision under `<native-id>.revisions/` — preserved, never
//!   rebuilt.
//! - `archive.db` — Conversations, indexes, Bookmarks; always fully
//!   regenerable from Captures, Digests, and Project Assignment files.
//! - `views/` — rendered Transcripts; regenerable and disposable.

pub mod db;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::adapters::{
    Adapter, AdapterExtraction, Capture, CaptureMeta, CaptureOutcome, adapter_for, collect_sidecars,
};

pub use db::{Db, HostPullRecord};

/// An open Archive directory. Creating one lays down the SPEC layout.
pub struct Archive {
    root: PathBuf,
}

/// Exclusive, advisory guard for every Archive-mutating CLI operation.
///
/// The guard explicitly unlocks before closing its descriptor, so a child
/// between fork and exec cannot prolong ownership. Process exit also releases
/// the lock once all inherited descriptors have closed. The lock file itself is an
/// inert archive-local coordination point and intentionally remains behind.
pub struct ArchiveWriterLock {
    _file: File,
}

impl Drop for ArchiveWriterLock {
    fn drop(&mut self) {
        // flock belongs to the open file description, shared across fork.
        // CLOEXEC is too late if another thread drops this guard while a
        // spawn is still executing its child setup. Release ownership now,
        // rather than waiting for the last inherited descriptor to close.
        if let Err(error) = self._file.unlock() {
            tracing::error!(%error, "releasing archive writer lock");
        }
    }
}

/// A Capture selected for on-demand inspection. Selection uses the same
/// source-specific winner policy as canonical archive projections.
#[derive(Debug, Clone)]
pub struct CaptureLookup {
    pub capture: Capture,
    pub available_hosts: Vec<String>,
}

impl Archive {
    /// Open (creating if absent) the Archive at `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let archive = Self {
            root: root.to_path_buf(),
        };
        std::fs::create_dir_all(archive.captures_dir())
            .with_context(|| format!("creating archive at {}", root.display()))?;
        std::fs::create_dir_all(archive.views_dir())?;
        Ok(archive)
    }

    /// Open an existing Archive without creating or migrating anything.
    pub fn open_existing(root: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(root)
            .with_context(|| format!("opening existing archive at {}", root.display()))?;
        anyhow::ensure!(metadata.is_dir(), "Archive root is not a directory");
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn captures_dir(&self) -> PathBuf {
        self.root.join("captures")
    }

    pub fn views_dir(&self) -> PathBuf {
        self.root.join("views")
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("archive.db")
    }

    /// Open the archive database, creating and migrating the schema.
    pub fn db(&self) -> Result<Db> {
        Db::open(&self.db_path())
    }

    /// Fail fast when another process is already mutating this Archive.
    ///
    /// Capture commits use deterministic temporary siblings and the
    /// regenerable database has one writer, so concurrent Archive mutations
    /// are unsafe even though individual renames are atomic.
    pub fn acquire_writer_lock(&self) -> Result<ArchiveWriterLock> {
        let path = self.root.join(".agent-sessions.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening archive writer lock {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(ArchiveWriterLock { _file: file }),
            Err(TryLockError::WouldBlock) => bail!(
                "archive {} already has another writer active; wait for it to finish before retrying",
                self.root.display()
            ),
            Err(TryLockError::Error(error)) if error.kind() == ErrorKind::Deadlock => bail!(
                "archive {} already has another writer active; wait for it to finish before retrying",
                self.root.display()
            ),
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("locking archive writer lock {}", path.display())),
        }
    }

    /// Resolve a Source-native id directly from Capture sidecars. This avoids
    /// treating the regenerable database as a source of truth and also lets
    /// `info` inspect capture-only Sessions.
    pub fn lookup_capture(
        &self,
        native_id: &str,
        source: Option<&str>,
        host: Option<&str>,
    ) -> Result<CaptureLookup> {
        let mut sidecars = Vec::new();
        collect_sidecars(&self.captures_dir(), &mut sidecars)?;
        sidecars.sort();

        let mut by_source: BTreeMap<String, Vec<(CaptureMeta, PathBuf)>> = BTreeMap::new();
        for meta_path in sidecars {
            let raw = std::fs::read_to_string(&meta_path)
                .with_context(|| format!("reading sidecar {}", meta_path.display()))?;
            let meta: CaptureMeta = serde_json::from_str(&raw)
                .with_context(|| format!("parsing sidecar {}", meta_path.display()))?;
            if adapter_for(&meta.source)
                .map_or(native_id, |adapter| adapter.capture_native_id(native_id))
                == meta.native_id
                && source.is_none_or(|wanted| meta.source == wanted)
            {
                by_source
                    .entry(meta.source.clone())
                    .or_default()
                    .push((meta, meta_path));
            }
        }

        if by_source.is_empty() {
            let mut scope = format!("session `{native_id}`");
            if let Some(source) = source {
                scope.push_str(&format!(" for source `{source}`"));
            }
            if let Some(host) = host {
                scope.push_str(&format!(" on host `{host}`"));
            }
            bail!("{scope} was not found in archive captures");
        }
        if by_source.len() > 1 {
            let sources = by_source.keys().cloned().collect::<Vec<_>>().join(", ");
            bail!(
                "session `{native_id}` is ambiguous across sources: {sources}; retry with --source SOURCE"
            );
        }

        let (selected_source, mut candidates) =
            by_source.into_iter().next().expect("non-empty source map");
        let available_hosts = candidates
            .iter()
            .map(|(meta, _)| meta.host.clone())
            .collect::<Vec<_>>();
        if let Some(host) = host {
            candidates.retain(|(meta, _)| meta.host == host);
            if candidates.is_empty() {
                bail!(
                    "session `{native_id}` for source `{selected_source}` has no preserved capture on host `{host}`"
                );
            }
        }
        let (meta, meta_path) = candidates
            .into_iter()
            .max_by(|(candidate, _), (winner, _)| {
                db::compare_capture_revisions(&candidate.source, candidate, winner)
            })
            .expect("non-empty candidates");
        let body_path = body_for_sidecar(&meta_path)?;
        Ok(CaptureLookup {
            capture: Capture {
                body_path,
                meta_path,
                meta,
                written: false,
                outcome: CaptureOutcome::Unchanged,
            },
            available_hosts,
        })
    }

    /// Remove interrupted atomic-write siblings left by an earlier process.
    /// Archive mutation is single-writer; callers invoke this before ingest.
    pub fn sweep_capture_temps(&self) -> Result<usize> {
        sweep_temps(&self.captures_dir())
    }
}

fn sweep_temps(dir: &Path) -> Result<usize> {
    let mut removed = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            removed += sweep_temps(&path)?;
        } else if path.extension().is_some_and(|ext| ext == "tmp") {
            std::fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Outcome of one rebuild.
#[derive(Debug, Default)]
pub struct RebuildStats {
    /// Captures (sidecar + body pairs) found under `captures/`.
    pub captures_found: usize,
    pub conversations_extracted: usize,
    pub orchestrator_threads_extracted: usize,
    pub captures_failed: usize,
    /// Captures preserved but skipped: no Adapter for their Source yet, or
    /// not extractable yet (subagent Sessions until slice 5).
    pub captures_skipped: usize,
}

/// Regenerate `archive.db` from Captures, Digests, and Project Assignments — the native Stores are never touched (ADR-0001). The old
/// database is dropped, every Capture is re-extracted, and Bookmarks are
/// restored from the sidecars' recorded source mtimes so the next `pull`
/// stays incremental. Deterministic: Captures are processed in sorted path
/// order, and all derived content is a pure function of durable Archive inputs.
pub fn rebuild(archive: &Archive) -> Result<RebuildStats> {
    let _writer_lock = archive.acquire_writer_lock()?;
    rebuild_locked(archive)
}

pub(crate) fn rebuild_locked(archive: &Archive) -> Result<RebuildStats> {
    let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
    let assignments = crate::assignments::load(archive.root())?;
    let leads = crate::leads::load(archive.root())?;
    archive.sweep_capture_temps()?;
    let db_path = archive.db_path();
    // Backfill legacy health before discarding its only copy.
    Db::preserve_host_pull_health(&db_path)?;
    crate::origins::load(archive.root())?;
    // Keep diagnostic decisions across index replacement. A missing/corrupt
    // derived database must still be rebuildable from durable inputs.
    let previous_origins = archive
        .db()
        .and_then(|db| db.origin_decisions())
        .unwrap_or_default();
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", db_path.display()));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("dropping {}", path.display())),
        }
    }

    let mut db = archive.db()?;
    db.sync_host_pull_health()?;
    let mut sidecars = Vec::new();
    collect_sidecars(&archive.captures_dir(), &mut sidecars)?;
    sidecars.sort();

    let mut captures = Vec::with_capacity(sidecars.len());
    let mut superseded = BTreeSet::new();
    for meta_path in sidecars {
        let raw = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("reading sidecar {}", meta_path.display()))?;
        let meta: CaptureMeta = serde_json::from_str(&raw)
            .with_context(|| format!("parsing sidecar {}", meta_path.display()))?;
        if !meta.superseded_native_ids.is_empty() {
            let adapter = adapter_for(&meta.source)
                .with_context(|| format!("validating superseded identities for {}", meta.source))?;
            for native_id in &meta.superseded_native_ids {
                anyhow::ensure!(
                    adapter.validates_superseded_identity(&meta.native_id, native_id),
                    "invalid superseded {} identity {native_id:?} for {:?}",
                    meta.source,
                    meta.native_id
                );
            }
        }
        superseded.extend(
            meta.superseded_native_ids
                .iter()
                .map(|native_id| (meta.source.clone(), native_id.clone())),
        );
        captures.push((meta_path, meta));
    }

    let mut stats = RebuildStats::default();
    let mut bookmarks: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut failed_sources = BTreeSet::new();
    let mut contexts = BTreeMap::new();
    for (_, meta) in &captures {
        if let Some(adapter) = adapter_for(&meta.source)
            && !contexts.contains_key(adapter.source())
        {
            contexts.insert(
                adapter.source(),
                adapter
                    .prepare_extraction(archive.root())
                    .map_err(|error| format!("{error:#}")),
            );
        }
    }
    for (meta_path, meta) in captures {
        let body_path = body_for_sidecar(&meta_path)?;
        stats.captures_found += 1;
        if superseded.contains(&(meta.source.clone(), meta.native_id.clone())) {
            stats.captures_skipped += 1;
            continue;
        }

        if let Some(adapter) = adapter_for(&meta.source) {
            let capture = Capture {
                body_path: body_path.clone(),
                meta_path: meta_path.clone(),
                meta: meta.clone(),
                written: false,
                outcome: crate::adapters::CaptureOutcome::Unchanged,
            };
            if crate::pull::selection::excluded_capture(
                adapter,
                &capture,
                &config.pull.exclude_paths,
            )? {
                stats.captures_skipped += 1;
                continue;
            }
        }
        db.record_capture_presence(&meta)?;
        let wm = bookmarks
            .entry((meta.host.clone(), meta.source.clone()))
            .or_insert(meta.bookmark_mtime_ms());
        *wm = (*wm).max(meta.bookmark_mtime_ms());

        let Some(adapter) = adapter_for(&meta.source) else {
            tracing::warn!(
                source = meta.source,
                capture = %body_path.display(),
                "no adapter for source; capture preserved, extraction skipped"
            );
            stats.captures_skipped += 1;
            continue;
        };
        let capture = Capture {
            body_path,
            meta_path,
            meta,
            written: false,
            outcome: crate::adapters::CaptureOutcome::Unchanged,
        };
        let result = (|| -> Result<()> {
            let context = contexts[adapter.source()]
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            adapter.archive_media(&capture, archive.root())?;
            match extract_capture(adapter, &capture, &archive.captures_dir(), context)
                .with_context(|| format!("extracting {}", capture.body_path.display()))?
            {
                Some(ExtractedCapture::Conversations(conversations)) => {
                    stats.conversations_extracted +=
                        context.store_conversations(&mut db, &conversations, &capture.meta)?;
                }
                Some(ExtractedCapture::Orchestrator(projection)) => {
                    db.upsert_orchestrator_projection(&projection)?;
                    stats.orchestrator_threads_extracted += 1;
                }
                None => stats.captures_skipped += 1,
            }
            Ok(())
        })();
        if let Err(error) = result {
            stats.captures_failed += 1;
            failed_sources.insert((capture.meta.host.clone(), capture.meta.source.clone()));
            tracing::warn!(
                capture = %capture.body_path.display(),
                error = format!("{error:#}"),
                "Capture extraction failed during rebuild; continuing"
            );
        }
    }

    stats.captures_failed += crate::repository_resolution::refresh(
        archive,
        None,
        &crate::repository_resolution::GitResolver,
    )?
    .units_failed;
    db.resolve_subagent_links()?;
    db.resolve_orchestrator_links()?;
    crate::origins::reconcile_with_previous(archive, &mut db, &previous_origins)?;
    log_orchestrator_fidelity(&db)?;

    for ((host, source), bookmark_ms) in bookmarks {
        if !failed_sources.contains(&(host.clone(), source.clone())) {
            db.advance_bookmark(&host, &source, bookmark_ms)?;
        }
    }

    db.replace_project_assignments(&assignments)?;
    db.replace_session_leads(&leads)?;
    if stats.captures_failed == 0 {
        db.set_capture_exclusions(&config.pull.exclude_paths)?;
    }
    drop(db);
    crate::digest::refresh_cache(archive)?;

    tracing::info!(
        found = stats.captures_found,
        conversations = stats.conversations_extracted,
        orchestrator_threads = stats.orchestrator_threads_extracted,
        skipped = stats.captures_skipped,
        failed = stats.captures_failed,
        "rebuild complete"
    );
    Ok(stats)
}

/// Extract through the Archive-owned parent-Capture seam. Parent discovery is
/// intentionally here, not in an Adapter: it is constrained to the child's
/// Host and Source and works identically for local pulls, Bundle import, and
/// rebuilds from captured data alone.
pub(crate) fn extract_capture(
    adapter: &dyn Adapter,
    capture: &Capture,
    captures_dir: &Path,
    context: &crate::adapters::ExtractionContext,
) -> Result<Option<ExtractedCapture>> {
    let parent = resolve_direct_parent_capture(adapter, capture, captures_dir)?;
    let mut extracted = adapter.extract_unit(capture, parent.as_ref(), context)?;
    if let Some(ExtractedCapture::Conversations(conversations)) = &mut extracted {
        for conversation in conversations {
            context.enrich(conversation);
        }
    }
    Ok(extracted)
}

pub(crate) type ExtractedCapture = AdapterExtraction;

pub(crate) fn log_orchestrator_fidelity(db: &Db) -> Result<()> {
    for diagnostic in db.orchestrator_link_diagnostics()? {
        tracing::debug!(
            orchestrator_source = diagnostic.source,
            orchestrator_thread = diagnostic.native_id,
            orchestrator_host = diagnostic.host,
            link_status = diagnostic.status,
            reason = diagnostic.reason,
            "unresolved Orchestrator Link"
        );
    }
    for report in db.orchestrator_fidelity_reports()? {
        tracing::debug!(
            orchestrator_source = report.orchestrator_source,
            orchestrator_thread = report.orchestrator_native_id,
            provider_source = report.session_source,
            provider_session = report.session_native_id,
            orchestrator_user = report.orchestrator_user_messages,
            orchestrator_assistant = report.orchestrator_assistant_messages,
            provider_user = report.provider_user_messages,
            provider_assistant = report.provider_assistant_messages,
            ordered_equal = report.ordered_role_text_equal,
            "Orchestrator/provider Dialog fidelity"
        );
    }
    Ok(())
}

fn resolve_direct_parent_capture(
    adapter: &dyn Adapter,
    child: &Capture,
    captures_dir: &Path,
) -> Result<Option<Capture>> {
    let Some(parent_native_id) = adapter.direct_parent_native_id(child)? else {
        return Ok(None);
    };
    if parent_native_id == child.meta.native_id || !safe_native_path(&parent_native_id) {
        tracing::warn!(
            child = %child.body_path.display(),
            parent = parent_native_id,
            "invalid direct parent identity; parent-dependent extraction skipped"
        );
        return Ok(None);
    }
    let Ok(paths) = crate::adapters::capture_paths(
        captures_dir,
        &child.meta.host,
        &child.meta.source,
        &parent_native_id,
    ) else {
        return Ok(None);
    };
    let meta_path = paths.meta;
    let Ok(raw) = std::fs::read_to_string(&meta_path) else {
        return Ok(None);
    };
    let Ok(meta) = serde_json::from_str::<CaptureMeta>(&raw) else {
        tracing::warn!(
            parent = %meta_path.display(),
            "unreadable direct parent sidecar; parent-dependent extraction skipped"
        );
        return Ok(None);
    };
    if meta.source != child.meta.source
        || meta.host != child.meta.host
        || meta.native_id != parent_native_id
    {
        tracing::warn!(
            parent = %meta_path.display(),
            "direct parent sidecar identity mismatches child Host/Source; extraction skipped"
        );
        return Ok(None);
    }
    let Ok(body_path) = body_for_sidecar(&meta_path) else {
        tracing::warn!(
            parent = %meta_path.display(),
            "direct parent Capture body unavailable; parent-dependent extraction skipped"
        );
        return Ok(None);
    };
    Ok(Some(Capture {
        body_path,
        meta_path,
        meta,
        written: false,
        outcome: crate::adapters::CaptureOutcome::Unchanged,
    }))
}

fn safe_native_path(native_id: &str) -> bool {
    !native_id.is_empty()
        && Path::new(native_id)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// The Capture body sitting next to a sidecar: the unique
/// `<native-id>.<ext>.zst` sharing the sidecar's `<native-id>` prefix (the
/// extension is Adapter-specific, so it is discovered, not assumed).
pub(crate) fn body_for_sidecar(meta_path: &Path) -> Result<PathBuf> {
    let dir = meta_path.parent().unwrap_or(Path::new(""));
    let name = meta_path.file_name().unwrap_or_default().to_string_lossy();
    let id = name
        .strip_suffix(".meta.json")
        .ok_or_else(|| anyhow::anyhow!("not a sidecar: {}", meta_path.display()))?;
    let prefix = format!("{id}.");
    let mut found = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let candidate = entry.file_name().to_string_lossy().into_owned();
        if candidate.starts_with(&prefix)
            && candidate.ends_with(".zst")
            && let Some(previous) = found.replace(entry.path())
        {
            anyhow::bail!(
                "ambiguous capture bodies for {}: {} and {}",
                meta_path.display(),
                previous.display(),
                entry.path().display(),
            );
        }
    }
    found.ok_or_else(|| anyhow::anyhow!("no capture body for sidecar {}", meta_path.display()))
}
