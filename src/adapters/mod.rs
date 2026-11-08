//! One Adapter per Source — the only code that ever sees native formats.
//!
//! An Adapter's job splits into three re-runnable phases:
//!
//! 1. **Enumerate**: list candidate Sessions from a Store using only metadata
//!    (paths, timestamps, index queries), gated by an [`EnumerateGate`]
//!    (Bookmark + matching Capture sidecar).
//! 2. **Capture**: preserve one Session's native payload verbatim into the
//!    Archive (zstd body + Provenance sidecar).
//! 3. **Extract**: produce a Conversation from a Capture — never from the
//!    Store, so a smarter Adapter can re-extract old Captures at any time.
//!
//! Native Stores are strictly read-only: files are only read, SQLite is only
//! opened in read-only mode, and no write locks are ever taken.

mod birth_time;
mod context;
pub(crate) use birth_time::{native_birth_time_ms, native_file_birth_time_ms};

pub mod claude;
pub mod codex;
pub mod cursor;
mod embedded_media;
pub(crate) mod file_capture;
mod gate;
mod paths;
pub use paths::{CapturePaths, capture_paths};
pub mod jsonl;
pub mod pi;
pub(crate) mod sqlite;
mod supersede;
pub mod t3;
pub mod vscode;

pub use file_capture::{BusyCapture, write_file_capture};
pub(crate) use file_capture::{NativeLocation, commit_staged, kept_capture};
pub(crate) use gate::collect_sidecars;
pub use gate::{CapturedFile, EnumerateGate};
pub use supersede::{BodySettlement, RevisionCheck, revision_check, settle_changed_body};
pub(crate) use supersede::{CaptureOrdering, capture_ordering, revision_check_for_adapter};

pub fn is_hosted_source(source: &str) -> bool {
    source == "chatgpt"
}

/// Sources whose Adapters currently participate in pulls and exports.
/// Keep this list aligned with [`adapter_for`] as each adapter slice lands.
pub const AVAILABLE_SOURCES: [&str; 6] = [
    claude::SOURCE,
    codex::SOURCE,
    pi::SOURCE,
    cursor::SOURCE,
    t3::SOURCE,
    "chatgpt",
];

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Conversation, OrchestratorProjection, Provenance};

/// Canonical projection produced by one native archival unit.
pub enum AdapterExtraction {
    Conversations(Vec<Conversation>),
    Orchestrator(Box<OrchestratorProjection>),
}

/// Operation-scoped Source facts reconstructed once from Archive-owned evidence.
#[derive(Default)]
pub struct ExtractionContext {
    pub aggregate_captures: std::collections::BTreeMap<String, Vec<Capture>>,
    pub native_projects: std::collections::BTreeMap<String, Option<crate::model::NativeProject>>,
}

impl ExtractionContext {
    pub fn enrich(&self, conversation: &mut Conversation) {
        if let Some(project) = self.native_projects.get(
            adapter_for(&conversation.provenance.source)
                .map_or(conversation.provenance.native_id.as_str(), |adapter| {
                    adapter.capture_native_id(&conversation.provenance.native_id)
                }),
        ) {
            conversation.native_project = project.clone();
        }
    }
}

/// What kind of Session a ref points at. Subagent Sessions are Captured from
/// slice 1 but only Extracted from slice 5 (linking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Main,
    Subagent,
    Orchestrator,
}

/// A candidate Session found by Enumerate: metadata only, content unread.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Native session id, unique within the Source (may contain `/`).
    pub native_id: String,
    /// Absolute path of the native payload inside the Store.
    pub path: PathBuf,
    /// Additional physical files belonging to the same logical Session.
    /// Ordered exactly as they must appear in the aggregate Capture.
    pub additional_files: Vec<NativeFileRef>,
    /// Encoded project directory name (Claude: the lossy `<encoded-cwd>`).
    pub project_dir: Option<String>,
    pub kind: SessionKind,
    /// Native file mtime, epoch milliseconds.
    pub mtime_ms: i64,
    /// Native file size in bytes at enumeration time.
    pub size: u64,
    /// Path of a native metadata sidecar (Claude subagent `.meta.json`).
    pub native_meta_path: Option<PathBuf>,
    pub native_meta_mtime_ms: Option<i64>,
    pub native_meta_size: Option<u64>,
    /// Small metadata projection obtained without a standalone JSON file.
    /// Used by catalog-backed Sources such as Codex.
    pub native_meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFileRef {
    pub path: PathBuf,
    pub mtime_ms: i64,
    pub size: u64,
}

impl SessionRef {
    pub fn bookmark_mtime_ms(&self) -> i64 {
        self.additional_files
            .iter()
            .fold(self.mtime_ms, |latest, file| latest.max(file.mtime_ms))
            .max(self.native_meta_mtime_ms.unwrap_or(i64::MIN))
    }

    pub fn native_files(&self) -> impl Iterator<Item = NativeFileRef> + '_ {
        std::iter::once(NativeFileRef {
            path: self.path.clone(),
            mtime_ms: self.mtime_ms,
            size: self.size,
        })
        .chain(self.additional_files.iter().cloned())
    }
}

/// The Provenance sidecar written next to every Capture body. Exists because
/// `rebuild` must work from the captures directory alone — Provenance cannot
/// live only in the regenerable database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_resolution: Option<crate::repository_resolution::Fact>,
    pub schema_version: u32,
    pub source: String,
    pub native_id: String,
    pub host: String,
    /// Absolute native path inside the Store on `host`.
    pub native_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    pub kind: SessionKind,
    /// Source file mtime and size at capture time — the per-session
    /// change-detection input for growth (append-only stores).
    pub source_mtime_ms: i64,
    pub source_size: u64,
    /// Native file creation time, milliseconds since Unix epoch. Null means
    /// the filesystem does not expose it; never substitute mtime or ctime.
    #[serde(default)]
    pub native_birth_time_ms: Option<i64>,
    /// Physical files composing an aggregate Capture. Empty on historical
    /// single-file sidecars, where the scalar source fields remain complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_files: Vec<CapturedSourceFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_meta_mtime_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_meta_size: Option<u64>,
    pub captured_at_ms: i64,
    pub adapter_version: u32,
    /// Incorrect historical Capture identities retired by this canonical
    /// Capture. Pull/bundle ingestion remove their derived projections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded_native_ids: Vec<String>,
    /// A prior Extract/upsert attempt failed. Enumerate must not trust this
    /// sidecar as a skip proof until a later retry succeeds.
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_retry: bool,
    /// The Source's own native metadata sidecar, verbatim (Claude subagent
    /// `agent-*.meta.json`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_meta: Option<serde_json::Value>,
    /// Source-associated Codex goal files captured as text so Extract and
    /// rebuild never depend on the live attachments tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal_attachments: Vec<CapturedGoalAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedSourceFile {
    pub path: String,
    pub mtime_ms: i64,
    pub size: u64,
    #[serde(default)]
    pub birth_time_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedGoalAttachment {
    pub native_path: String,
    pub status: GoalAttachmentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalAttachmentStatus {
    Resolved,
    Missing,
    Rejected,
    Unreadable,
}

impl CaptureMeta {
    pub fn bookmark_mtime_ms(&self) -> i64 {
        self.source_mtime_ms
            .max(self.native_meta_mtime_ms.unwrap_or(i64::MIN))
    }
}

/// A Capture written into the Archive: the verbatim body plus its sidecar.
#[derive(Debug, Clone)]
pub struct Capture {
    /// `captures/<host>/<source>/<native-id>.<ext>.zst`
    pub body_path: PathBuf,
    /// `captures/<host>/<source>/<native-id>.meta.json`
    pub meta_path: PathBuf,
    pub meta: CaptureMeta,
    /// False when an existing Capture already matched the native file
    /// (sidecar mtime + size) and was reused instead of rewritten.
    pub written: bool,
    pub outcome: CaptureOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    New,
    /// Superseded by a body that covers the prior Capture's Dialog.
    Extended,
    /// Superseded by a body whose Dialog diverged; the prior body is kept as
    /// a retained Prior Revision.
    Revised,
    /// The incoming body lost Dialog; the Capture was kept. Counts as a
    /// failed Session so the Source's Bookmark holds.
    Refused,
    Unchanged,
}

impl Capture {
    pub fn provenance(&self) -> Provenance {
        Provenance {
            source: self.meta.source.clone(),
            host: self.meta.host.clone(),
            native_id: self.meta.native_id.clone(),
            native_path: self.meta.native_path.clone(),
        }
    }

    pub fn set_needs_retry(&mut self, needs_retry: bool) -> Result<()> {
        if needs_retry {
            return write_retry_sentinel(&self.meta_path);
        }
        // Compatibility: older slice-4b sidecars stored the marker inline.
        // Atomically clear that field while the old, valid Provenance stays
        // present. On failure both it and the sentinel remain retry proofs.
        if self.meta.needs_retry {
            let mut cleared = self.meta.clone();
            cleared.needs_retry = false;
            let tmp = tmp_sibling(&self.meta_path);
            std::fs::write(&tmp, serde_json::to_string_pretty(&cleared)?)?;
            std::fs::rename(&tmp, &self.meta_path)
                .with_context(|| format!("committing sidecar {}", self.meta_path.display()))?;
            self.meta = cleared;
        }
        match std::fs::remove_file(retry_sentinel(&self.meta_path)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("clearing Capture retry sentinel"),
        }
    }

    pub fn retry_pending(&self) -> bool {
        self.meta.needs_retry || retry_sentinel(&self.meta_path).is_file()
    }
}

pub(super) fn retry_sentinel(meta_path: &Path) -> PathBuf {
    let name = meta_path.file_name().unwrap_or_default().to_string_lossy();
    meta_path.with_file_name(format!("{name}.retry"))
}

pub(crate) fn write_retry_sentinel(meta_path: &Path) -> Result<()> {
    let sentinel = retry_sentinel(meta_path);
    if sentinel.is_file() {
        return Ok(());
    }
    // The filename is the marker; its contents are informational. Create it
    // directly so even a later write/sync error leaves Enumerate-visible
    // retry state rather than a trusted sidecar alone.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&sentinel)
        .with_context(|| format!("creating Capture retry {}", sentinel.display()))?;
    file.write_all(b"Extract/upsert pending\n")?;
    file.sync_all()
        .with_context(|| format!("syncing Capture retry {}", sentinel.display()))
}

/// The per-Source contract. Exactly one implementation per Source.
pub trait Adapter {
    /// Relative stem of a Capture, including any Source-owned hierarchy.
    fn capture_stem(&self, native_id: &str) -> String {
        self.capture_native_id(native_id).to_owned()
    }
    fn capture_extension(&self) -> &'static str {
        "jsonl"
    }

    fn archive_media(&self, _capture: &Capture, _archive_root: &Path) -> Result<()> {
        Ok(())
    }

    fn capture_native_id<'a>(&self, native_id: &'a str) -> &'a str {
        native_id
    }
    /// Additive native evidence can settle a Bundle directly. None leaves the
    /// reader untouched and requests ordinary replacement settlement.
    fn settle_bundle(
        &self,
        _body: &mut dyn std::io::Read,
        _meta: &CaptureMeta,
        _captures: &Path,
    ) -> Result<Option<CaptureOutcome>> {
        Ok(None)
    }

    /// Disposable artifacts derived only from an elected Capture.
    fn render_material(
        &self,
        _capture: &Capture,
        _archive_root: &Path,
        _context: &ExtractionContext,
    ) -> Result<crate::model::RenderMaterial> {
        Ok(crate::model::RenderMaterial::default())
    }

    /// Application retention observed during collection; unknown Sources report none.
    fn store_retention(&self, _store_root: &Path) -> Result<Option<crate::pull::StoreRetention>> {
        Ok(None)
    }

    /// Source name: `claude`, `codex`, `pi`, `cursor`, `vscode`.
    fn source(&self) -> &'static str;

    /// Whether `superseded_native_id` is a Source-format identity bug that
    /// the canonical Capture can authoritatively retire.
    fn validates_superseded_identity(
        &self,
        _canonical_native_id: &str,
        _superseded_native_id: &str,
    ) -> bool {
        false
    }

    /// List candidate Sessions from a Store using only metadata, dropping
    /// only files the `gate` proves are already captured unchanged.
    fn enumerate(&self, store_root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>>;

    /// Preserve one Session's native payload verbatim into
    /// `captures_dir/<host>/<source>/…`, returning the written Capture.
    fn capture(&self, session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture>;

    /// Native id of this Capture's direct parent, when the Source records a
    /// child-side fact. The Archive resolves the Capture; Adapters never
    /// discover sibling Capture paths themselves.
    fn direct_parent_native_id(&self, _capture: &Capture) -> Result<Option<String>> {
        Ok(None)
    }

    /// Produce a Conversation from a Capture. `Ok(None)` means the Capture is
    /// preserved but not yet extractable (subagent Sessions until slice 5).
    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>>;

    /// Read only recorded native metadata for admission; never Capture or Extract.
    fn session_working_path(&self, session: &SessionRef) -> Result<Option<String>>;

    /// Recorded working path used by Capture admission; never probe the workspace.
    fn capture_working_path(&self, capture: &Capture) -> Result<Option<String>> {
        Ok(self.extract(capture)?.and_then(|c| c.project_path))
    }

    /// Resolve auxiliary native evidence once per Source/Archive operation.
    fn prepare_extraction(&self, _archive_root: &Path) -> Result<ExtractionContext> {
        Ok(ExtractionContext::default())
    }

    /// Every Dialog Message this Capture body holds on its own — no Archive
    /// context, no parent resolution, no child gating. Supersede compares
    /// these views, never byte sizes ([`settle_changed_body`]). Only
    /// append-oriented Sources are asked; a body that yields no view is an
    /// error, never a silent fallback.
    fn body_dialog(&self, capture: &Capture) -> Result<Conversation> {
        self.extract(capture)?.with_context(|| {
            format!(
                "{} has no Dialog view of {}",
                self.source(),
                capture.body_path.display()
            )
        })
    }

    /// Extract with the Archive-resolved direct parent Capture, when one is
    /// available on the same Host and Source. Sources that do not need parent
    /// context retain the ordinary extraction path.
    fn extract_with_parent(
        &self,
        capture: &Capture,
        _direct_parent: Option<&Capture>,
    ) -> Result<Option<Conversation>> {
        self.extract(capture)
    }

    /// Project one captured native archival unit through its Source-owned
    /// seam. Agent adapters use the default Conversation path.
    fn extract_unit(
        &self,
        capture: &Capture,
        direct_parent: Option<&Capture>,
        _context: &ExtractionContext,
    ) -> Result<Option<AdapterExtraction>> {
        self.extract_with_parent(capture, direct_parent)
            .map(|value| {
                value.map(|conversation| AdapterExtraction::Conversations(vec![conversation]))
            })
    }

    /// Source-native, on-demand diagnostics for `agent-sessions info`.
    /// The default deliberately exposes nothing rather than inventing a
    /// cross-provider schema; adapters opt in with named native details.
    fn inspect(&self, _capture: &Capture) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }
}

/// The Adapter owning a Source name — how `rebuild` dispatches Captures
/// found on disk back to their Adapter's `extract`. `None` for a Source
/// whose Adapter hasn't landed yet: its Captures are preserved and simply
/// skipped until it does.
pub fn adapter_for(source: &str) -> Option<&'static dyn Adapter> {
    match source {
        "chatgpt" => Some(&crate::chatgpt::ChatGptAdapter),
        claude::SOURCE => Some(&claude::ClaudeAdapter),
        codex::SOURCE => Some(&codex::CodexAdapter),
        pi::SOURCE => Some(&pi::PiAdapter),
        cursor::SOURCE => Some(&cursor::CursorAdapter),
        t3::SOURCE => Some(&t3::T3Adapter),
        _ => None,
    }
}

pub(crate) fn write_capture_meta(path: &Path, meta: &CaptureMeta) -> Result<()> {
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, serde_json::to_string_pretty(meta)?)?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("committing sidecar {}", path.display()))?;
    Ok(())
}

/// `<name>.tmp` next to `path` — same directory so the rename is atomic.
/// Commit order is body first, sidecar second: the sidecar is the claim
/// "the body matches this mtime + size", so it must never land before the
/// body it describes. A crash between the two leaves a stale sidecar, which
/// simply fails the unchanged check and re-captures next run.
pub(crate) fn tmp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{name}.tmp"))
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
