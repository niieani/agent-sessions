//! The Bundle format: a tar stream of Captures plus one `manifest.json`.
//!
//! The self-contained unit a Host exports for pulling (ADR-0002). Designed to
//! pipe: `ssh host agent-sessions export --since X` → stdout → central
//! ingest, with no temp files on either side of the stream and bounded
//! memory (Capture bodies are streamed entry by entry, never slurped).
//!
//! Layout of the tar stream:
//!
//! 1. `manifest.json` — always the first entry, so the central side knows
//!    what it is receiving before any payload arrives.
//! 2. Per session, in sorted order: the Provenance sidecar
//!    (`captures/<host>/<source>/<native-id>.meta.json`) immediately
//!    followed by its body (`captures/<host>/<source>/<native-id>.<ext>.zst`).
//!    Sidecar-first lets ingest decide supersede-vs-skip before the body
//!    bytes arrive; the *write* order on disk stays body-before-sidecar (a
//!    sidecar always describes the body next to it).
//!
//! Ingest reuses the exact local supersede rules
//! ([`crate::adapters::revision_check`], then Dialog coverage): unchanged
//! Captures are reused, a body covering the Capture's Dialog supersedes it
//! atomically, a diverged one supersedes it with the prior body retained as
//! a Prior Revision, and one that lost Dialog is refused.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

mod metadata;
mod paths;
mod settle;

use metadata::capture_metadata_is_newer;
use paths::{bundled_capture_paths, ensure_safe_component};

use crate::adapters::{
    AVAILABLE_SOURCES, Capture, CaptureMeta, CaptureOutcome, EnumerateGate, adapter_for, now_ms,
};
use crate::archive::Archive;
use crate::archive::Db;
use crate::pull::StoreRoots;
use crate::pull::validate_store_root;

pub const BUNDLE_SCHEMA_VERSION: u32 = 2;

/// The first entry of every Bundle: what the exporting Host is sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleManifest {
    pub schema_version: u32,
    /// The Host these Captures were recorded on.
    pub host: String,
    /// Source names included in this Bundle.
    pub sources: Vec<String>,
    /// The Bookmark basis the export was asked for (`--since`): everything
    /// with a native mtime above this is included. `None` = full export.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    /// Number of native archival units (sidecar + body pairs) in the Bundle.
    pub unit_count: usize,
    /// Native archival units that could not be captured. A partial Bundle is still
    /// ingestible, but cannot justify advancing its Source Bookmark.
    #[serde(default)]
    pub units_failed: usize,
    /// Busy units deferred after bounded re-reads; nonfatal, but withhold the Bookmark.
    #[serde(default)]
    pub units_retry_later: usize,
    pub exported_at_ms: i64,
    #[serde(default)]
    pub retention: BTreeMap<String, crate::pull::StoreRetention>,
}

/// Write a Bundle: `manifest.json`, then each Capture's sidecar and body.
/// `spool_root` is the captures directory the Captures live under — entry
/// paths inside the tar are `captures/<path relative to spool_root>`.
pub fn write_bundle<W: Write>(
    writer: W,
    manifest: &BundleManifest,
    captures: &[Capture],
    spool_root: &Path,
) -> Result<()> {
    let mut tar = tar::Builder::new(writer);
    append_bytes(
        &mut tar,
        Path::new("manifest.json"),
        serde_json::to_string_pretty(manifest)?.as_bytes(),
    )?;

    crate::media::bundle::append(
        &mut tar,
        spool_root.parent().context("Capture root parent absent")?,
    )?;
    let mut sorted: Vec<&Capture> = captures.iter().collect();
    sorted.sort_by(|a, b| a.body_path.cmp(&b.body_path));
    for capture in sorted {
        let sidecar_rel = bundle_entry_path(&capture.meta_path, spool_root)?;
        let body_rel = bundle_entry_path(&capture.body_path, spool_root)?;
        append_bytes(
            &mut tar,
            &sidecar_rel,
            serde_json::to_string_pretty(&capture.meta)?.as_bytes(),
        )?;
        let mut body = std::fs::File::open(&capture.body_path)
            .with_context(|| format!("opening capture body {}", capture.body_path.display()))?;
        let len = body.metadata()?.len();
        let mut header = tar::Header::new_gnu();
        header.set_size(len);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, &body_rel, &mut body)?;
    }
    tar.into_inner()?.flush()?;
    Ok(())
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, path: &Path, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn bundle_entry_path(path: &Path, spool_root: &Path) -> Result<PathBuf> {
    let rel = path.strip_prefix(spool_root).with_context(|| {
        format!(
            "capture {} is outside the spool {}",
            path.display(),
            spool_root.display()
        )
    })?;
    Ok(Path::new("captures").join(rel))
}

/// Outcome of one `export` run on the exporting Host.
#[derive(Debug, Default)]
pub struct ExportStats {
    pub sessions_found: usize,
    pub sessions_failed: usize,
    pub sessions_retry_later: usize,
}

/// The remote side of `pull` (ADR-0002): enumerate + capture everything
/// newer than `since` into a temporary spool, then stream the Bundle to
/// `writer`. Requires no Archive and leaves no state behind — remotes are
/// stateless, so the skip gate is Bookmark-only ([`EnumerateGate::bookmark_only`]).
pub fn export<W: Write>(
    writer: W,
    roots: &StoreRoots,
    host: &str,
    source: Option<&str>,
    since: Option<i64>,
) -> Result<ExportStats> {
    export_with_repository_lookup(writer, roots, host, source, since, true, &[])
}

pub(crate) fn export_with_repository_lookup<W: Write>(
    writer: W,
    roots: &StoreRoots,
    host: &str,
    source: Option<&str>,
    since: Option<i64>,
    lookup: bool,
    exclude_paths: &[String],
) -> Result<ExportStats> {
    if since.is_some() && source.is_none() {
        bail!("`--since` requires a single Source; export each Source at its own Bookmark");
    }
    let selected_sources: Vec<&str> = match source {
        None => AVAILABLE_SOURCES.to_vec(),
        Some(source) if AVAILABLE_SOURCES.contains(&source) => vec![source],
        Some(source) => bail!("unknown export Source {source:?}"),
    };
    let spool = tempfile::tempdir().context("creating export spool")?;
    let spool_captures = spool.path().join("captures");
    std::fs::create_dir(&spool_captures)?;
    let spool_root = spool_captures.as_path();

    let mut retention = BTreeMap::new();
    let mut captures: Vec<Capture> = Vec::new();
    let mut sessions_found = 0;
    let mut sessions_failed = 0;
    let mut sessions_retry_later = 0;
    for source in &selected_sources {
        let adapter = adapter_for(source).expect("available Source has an Adapter");
        let root = roots
            .for_source(source)
            .expect("available Source has a Store root");
        let enumeration = (|| -> Result<_> {
            if !validate_store_root(source, root, roots.source_is_explicit(source))? {
                return Ok(None);
            }
            if let Some(value) = adapter.store_retention(root)? {
                retention.insert((*source).to_owned(), value);
            }
            let refs = crate::pull::selection::enumerate(
                adapter,
                root,
                &EnumerateGate::bookmark_only(since),
                exclude_paths,
            )
            .with_context(|| format!("enumerating {} store", adapter.source()))?;
            Ok(Some(refs))
        })();
        let selected = match enumeration {
            Ok(Some(refs)) => refs,
            Ok(None) => continue,
            Err(error) if selected_sources.len() > 1 => {
                sessions_failed += 1;
                tracing::error!(
                    source = adapter.source(),
                    error = format!("{error:#}"),
                    "Source export failed; continuing"
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        sessions_failed += selected.failed.len();
        sessions_found += selected.failed.len();
        for (session, error) in &selected.failed {
            tracing::warn!(
                host,
                source = adapter.source(),
                session,
                error,
                "Session export admission failed; continuing"
            );
        }
        sessions_retry_later += selected.retry_later.len();
        sessions_found += selected.retry_later.len();
        for (session, error) in &selected.retry_later {
            tracing::warn!(
                host,
                source = adapter.source(),
                session,
                error,
                "Session export admission retry-later after 3 attempts"
            );
        }
        let refs = selected.sessions;
        tracing::info!(
            source = adapter.source(),
            sessions = refs.len(),
            since,
            "enumerated for export"
        );
        sessions_found += refs.len();
        for session in &refs {
            match adapter.capture(session, spool_root, host) {
                Ok(capture) => captures.push(capture),
                Err(error) if error.is::<crate::adapters::BusyCapture>() => {
                    sessions_retry_later += 1;
                    tracing::warn!(
                        source = adapter.source(),
                        session = session.native_id,
                        error = format!("{error:#}"),
                        "Session export retry-later after 3 attempts"
                    );
                }
                Err(error) => {
                    sessions_failed += 1;
                    tracing::warn!(
                        source = adapter.source(),
                        session = session.native_id,
                        error = format!("{error:#}"),
                        "Session export failed; continuing"
                    );
                }
            }
        }
    }

    if lookup {
        sessions_failed += crate::repository_resolution::refresh_captures(
            spool_root,
            spool.path(),
            Some(host),
            &crate::repository_resolution::GitResolver,
        )?
        .units_failed;
    }
    for capture in &mut captures {
        capture.meta = serde_json::from_reader(std::fs::File::open(&capture.meta_path)?)?;
    }
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        host: host.to_string(),
        sources: selected_sources.into_iter().map(str::to_owned).collect(),
        since_ms: since,
        unit_count: captures.len(),
        units_failed: sessions_failed,
        units_retry_later: sessions_retry_later,
        exported_at_ms: now_ms(),
        retention,
    };
    write_bundle(writer, &manifest, &captures, spool_root)?;
    tracing::info!(sessions = captures.len(), "bundle written");
    Ok(ExportStats {
        sessions_found,
        sessions_failed,
        sessions_retry_later,
    })
}

/// Outcome of ingesting one Bundle centrally.
#[derive(Debug, Default)]
pub struct BundleIngestStats {
    /// Native archival units (sidecar + body pairs) present in the Bundle.
    pub units_in_bundle: usize,
    /// Captures freshly written (new Sessions, or richer ones superseding).
    pub sessions_captured: usize,
    pub sessions_new: usize,
    pub sessions_extended: usize,
    /// Sessions whose existing Capture was already identical and reused.
    pub sessions_unchanged: usize,
    /// Captures superseded by a diverged Dialog, prior body retained.
    pub sessions_revised: usize,
    /// Incoming payloads refused (lost Dialog, or a stale logical revision);
    /// each also counts in `sessions_failed`.
    pub sessions_refused: usize,
    pub conversations_extracted: usize,
    pub sessions_failed: usize,
}

/// A fully ingested Bundle: its manifest, counts, and the Bookmark values
/// it justifies. Bookmarks are *returned, not advanced* — the caller
/// advances them only once the whole transfer is known to have succeeded.
#[derive(Debug)]
pub struct BundleIngest {
    pub manifest: BundleManifest,
    pub stats: BundleIngestStats,
    /// (Host, Source) → highest native mtime seen in the Bundle.
    pub bookmarks: BTreeMap<(String, String), i64>,
}

/// Ingest a Bundle stream into the Archive: upsert Captures under the local
/// supersede rules, re-extract, and upsert Conversations. Streaming and
/// bounded: one sidecar in memory at a time, bodies copied entry → file.
pub fn ingest_bundle<R: Read>(reader: R, archive: &Archive, db: &mut Db) -> Result<BundleIngest> {
    crate::origins::preserve_current(archive, db)?;
    let mut result = ingest_bundle_unreconciled(reader, archive, db)?;
    result.stats.sessions_failed +=
        crate::repository_resolution::refresh_recorded(archive, &result.manifest.host)?
            .units_failed;
    crate::origins::reconcile(archive, db)?;
    Ok(result)
}

pub(crate) fn ingest_bundle_unreconciled<R: Read>(
    reader: R,
    archive: &Archive,
    db: &mut Db,
) -> Result<BundleIngest> {
    archive.sweep_capture_temps()?;
    let captures_dir = archive.captures_dir();
    let mut tar = tar::Archive::new(reader);
    let mut entries = tar.entries().context("reading bundle stream")?;

    let manifest: BundleManifest = {
        let mut first = match entries.next() {
            Some(entry) => entry.context("reading bundle manifest entry")?,
            None => bail!("empty bundle: no manifest.json"),
        };
        if first.path()?.as_ref() != Path::new("manifest.json") {
            bail!("malformed bundle: first entry is not manifest.json");
        }
        let mut raw = String::new();
        first.read_to_string(&mut raw)?;
        serde_json::from_str(&raw).context("parsing bundle manifest")?
    };
    if manifest.schema_version != BUNDLE_SCHEMA_VERSION {
        bail!(
            "bundle schema v{} is newer than this binary understands (v{})",
            manifest.schema_version,
            BUNDLE_SCHEMA_VERSION
        );
    }
    ensure_safe_component(&manifest.host)?;
    if manifest.sources.is_empty() {
        bail!("malformed bundle: manifest has no Sources");
    }
    let mut manifest_sources = std::collections::BTreeSet::new();
    for source in &manifest.sources {
        ensure_safe_component(source)?;
        if !manifest_sources.insert(source.as_str()) {
            bail!("malformed bundle: manifest repeats Source {source:?}");
        }
    }

    let mut stats = BundleIngestStats::default();
    let mut bookmarks: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut identities = BTreeSet::new();
    let mut extracted = BTreeSet::new();
    let mut extraction_contexts = std::collections::BTreeMap::new();
    let mut settled_captures = Vec::new();

    while let Some(entry) = entries.next() {
        let mut sidecar_entry = entry?;
        let sidecar_path = sidecar_entry.path()?.into_owned();
        if sidecar_path.starts_with("media") {
            crate::media::bundle::ingest(archive.root(), &sidecar_path, &mut sidecar_entry)?;
            continue;
        }

        if !sidecar_path.to_string_lossy().ends_with(".meta.json") {
            bail!(
                "malformed bundle: expected a sidecar entry, got {}",
                sidecar_path.display()
            );
        }
        let mut raw = String::new();
        sidecar_entry.read_to_string(&mut raw)?;
        let mut meta: CaptureMeta = serde_json::from_str(&raw)
            .with_context(|| format!("parsing bundled sidecar {}", sidecar_path.display()))?;
        if meta.host != manifest.host || !manifest_sources.contains(meta.source.as_str()) {
            bail!(
                "malformed bundle: sidecar Host/Source ({:?}, {:?}) does not match manifest",
                meta.host,
                meta.source
            );
        }
        if !meta.superseded_native_ids.is_empty() {
            let adapter = adapter_for(&meta.source)
                .with_context(|| format!("validating superseded identities for {}", meta.source))?;
            for superseded in &meta.superseded_native_ids {
                anyhow::ensure!(
                    adapter.validates_superseded_identity(&meta.native_id, superseded),
                    "invalid superseded {} identity {superseded:?} for {:?}",
                    meta.source,
                    meta.native_id
                );
            }
        }
        ensure_safe_component(&meta.host)?;
        ensure_safe_component(&meta.source)?;
        let identity = (
            meta.host.clone(),
            meta.source.clone(),
            meta.native_id.clone(),
        );
        if !identities.insert(identity) {
            bail!(
                "malformed bundle: duplicate native archival unit identity ({:?}, {:?}, {:?})",
                meta.host,
                meta.source,
                meta.native_id
            );
        }

        let mut body_entry = match entries.next() {
            Some(e) => e?,
            None => bail!(
                "malformed bundle: sidecar {} has no body entry",
                sidecar_path.display()
            ),
        };
        let body_name = body_entry
            .path()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        stats.units_in_bundle += 1;

        // Destinations are derived from the *sidecar*, never from tar entry
        // paths, so a hostile or corrupt Bundle cannot traverse outside the
        // captures directory.
        let base = captures_dir.join(&meta.host).join(&meta.source);
        let (body_path, meta_path) = bundled_capture_paths(&base, &meta, &body_name)?;
        if let Some(parent) = body_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let existing = read_sidecar(&meta_path).filter(|_| body_path.is_file());
        if let Some(existing) = &existing {
            meta.superseded_native_ids
                .extend(existing.superseded_native_ids.iter().cloned());
            meta.superseded_native_ids.sort();
            meta.superseded_native_ids.dedup();
        }
        let metadata_update = existing
            .as_ref()
            .is_some_and(|existing| capture_metadata_is_newer(&meta, existing));
        // Mark derived work pending before any winning Capture state is
        // changed. The Provenance sidecar, when present, remains untouched.
        crate::adapters::write_retry_sentinel(&meta_path)?;
        let settled = settle::settle_unit(
            &mut body_entry,
            existing.as_ref(),
            &meta,
            &body_path,
            &meta_path,
            metadata_update,
        );
        let outcome = match settled {
            Ok(outcome) => outcome,
            Err(error) => {
                stats.sessions_failed += 1;
                tracing::warn!(
                    capture = %body_path.display(),
                    error = format!("{error:#}"),
                    "bundled native archival unit could not be settled; Capture kept"
                );
                continue;
            }
        };
        match outcome {
            CaptureOutcome::New => stats.sessions_new += 1,
            CaptureOutcome::Extended => stats.sessions_extended += 1,
            CaptureOutcome::Revised => stats.sessions_revised += 1,
            CaptureOutcome::Unchanged => stats.sessions_unchanged += 1,
            // A stateless remote would skip this Session forever if its
            // newer Bookmark advanced. Treat the refused incoming state as
            // failed so the Source remains in the retry window.
            CaptureOutcome::Refused => {
                stats.sessions_refused += 1;
                stats.sessions_failed += 1;
            }
        }
        if matches!(
            outcome,
            CaptureOutcome::New | CaptureOutcome::Extended | CaptureOutcome::Revised
        ) {
            stats.sessions_captured += 1;
        }

        let wm = bookmarks
            .entry((meta.host.clone(), meta.source.clone()))
            .or_insert(meta.bookmark_mtime_ms());
        *wm = (*wm).max(meta.bookmark_mtime_ms());

        // Re-extract from the Capture that won (self-repair: even an
        // unchanged Capture refreshes its database rows).
        let winning = read_sidecar(&meta_path).ok_or_else(|| {
            anyhow::anyhow!("missing sidecar after ingest: {}", meta_path.display())
        })?;
        // A refused eligible incoming body can leave an excluded incumbent
        // on disk. Never reintroduce that incumbent into the derived index.
        let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
        if let Some(adapter) = adapter_for(&winning.source) {
            let capture = Capture {
                body_path: body_path.clone(),
                meta_path: meta_path.clone(),
                meta: winning.clone(),
                written: false,
                outcome,
            };
            if crate::pull::selection::excluded_capture(
                adapter,
                &capture,
                &config.pull.exclude_paths,
            )? {
                if outcome != CaptureOutcome::Refused {
                    stats.sessions_failed += 1;
                }
                continue;
            }
        }
        for superseded in &winning.superseded_native_ids {
            db.retire_session_identity(&winning.source, superseded)?;
        }
        db.record_capture_presence(&winning)?;
        let Some(_) = adapter_for(&winning.source) else {
            tracing::warn!(
                source = winning.source,
                "no adapter for bundled source; capture preserved, extraction skipped"
            );
            continue;
        };
        let mut capture = Capture {
            body_path: body_path.clone(),
            meta_path: meta_path.clone(),
            meta: winning,
            written: false,
            outcome: crate::adapters::CaptureOutcome::Unchanged,
        };
        capture.set_needs_retry(true)?;
        settled_captures.push((capture, outcome));
    }

    // Parent and auxiliary Source facts require the complete settled payload.
    // Prepare one context per Source, then extract each native unit once.
    for (capture, _) in &settled_captures {
        let adapter = adapter_for(&capture.meta.source).expect("settled Source has an Adapter");
        if !extraction_contexts.contains_key(adapter.source()) {
            extraction_contexts.insert(
                adapter.source(),
                adapter
                    .prepare_extraction(archive.root())
                    .map_err(|error| format!("{error:#}")),
            );
        }
    }
    for (mut capture, outcome) in settled_captures {
        let extraction = (|| -> Result<bool> {
            capture.set_needs_retry(true)?;
            let adapter =
                adapter_for(&capture.meta.source).expect("settled Capture source has an Adapter");
            let context = extraction_contexts[adapter.source()]
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            adapter.archive_media(&capture, archive.root())?;
            let extracted_now =
                match crate::archive::extract_capture(adapter, &capture, &captures_dir, context)? {
                    Some(crate::archive::ExtractedCapture::Conversations(conversations)) => {
                        context.store_conversations(db, &conversations, &capture.meta)?;
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
            Ok(extracted_now)
        })();
        match extraction {
            Ok(true) => {
                if extracted.insert((capture.meta.source.clone(), capture.meta.native_id.clone())) {
                    stats.conversations_extracted += 1;
                }
            }
            Ok(false) => {}
            Err(error) => {
                match outcome {
                    CaptureOutcome::New => stats.sessions_new -= 1,
                    CaptureOutcome::Extended => stats.sessions_extended -= 1,
                    CaptureOutcome::Revised => stats.sessions_revised -= 1,
                    CaptureOutcome::Unchanged => stats.sessions_unchanged -= 1,
                    CaptureOutcome::Refused => {}
                }
                if outcome != CaptureOutcome::Refused {
                    stats.sessions_failed += 1;
                }
                tracing::warn!(capture = %capture.body_path.display(), error = format!("{error:#}"), "settled native archival unit extraction failed; continuing");
            }
        }
    }

    db.resolve_subagent_links()?;
    db.resolve_orchestrator_links()?;
    crate::archive::log_orchestrator_fidelity(db)?;

    if stats.units_in_bundle != manifest.unit_count {
        bail!(
            "malformed bundle: manifest declares {} native archival units but stream contains {}",
            manifest.unit_count,
            stats.units_in_bundle
        );
    }

    tracing::info!(
        host = manifest.host,
        units = stats.units_in_bundle,
        captured = stats.sessions_captured,
        unchanged = stats.sessions_unchanged,
        revised = stats.sessions_revised,
        refused = stats.sessions_refused,
        extracted = stats.conversations_extracted,
        "bundle ingested"
    );
    Ok(BundleIngest {
        manifest,
        stats,
        bookmarks,
    })
}

fn read_sidecar(path: &Path) -> Option<CaptureMeta> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}
