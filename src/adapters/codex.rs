//! Adapter for the Codex Source (`~/.codex`).
//!
//! Format reference: `docs/formats/codex.md`. The Store spans two trees under
//! the root: `sessions/YYYY/MM/DD/rollout-*.jsonl` (date-partitioned, LOCAL
//! creation date) and `archived_sessions/rollout-*.jsonl` (flat; files are
//! *moved* there when the user archives a thread). Single files reach ~76 MB
//! and single LINES reach ~20 MB (image blocks inside tool outputs), so
//! extraction parses each line into typed structs over a borrowed raw
//! payload — fields we do not extract (base64 images, tool outputs,
//! `dynamic_tools`, `base_instructions`) are skipped by serde without ever
//! being materialized. Bounded memory is a SPEC requirement.
//!
//! Extraction policy (SPEC "Source-specific extraction policy"):
//! - Messages come from `response_item` records only; `event_msg` records are
//!   UI-facing duplicates. The one exception is the `agent_message` fallback
//!   for old/degenerate files with zero assistant `response_item` messages.
//! - Injected context arriving as `role:"user"` is filtered by leading tag;
//!   `<turn_aborted>` is kept (real dialog context), `<subagent_notification>`
//!   spans are stripped with any surrounding text surviving.
//! - Guardian and subagent-flagged Sessions are always Captured but yield no
//!   Conversation (`Ok(None)`).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

mod composer;
mod rollout_group;

use composer::ComposedPart;
use rollout_group::{capture_rollout_group, rollout_group_is_captured};

use super::jsonl::open_capture;
use super::sqlite::{open_read_only, table_columns};
use super::{
    Adapter, Capture, CaptureMeta, CaptureOutcome, CapturedGoalAttachment, CapturedSourceFile,
    EnumerateGate, GoalAttachmentStatus, NativeFileRef, SessionKind, SessionRef, mtime_ms, now_ms,
    write_capture_meta, write_file_capture,
};
use crate::model::{
    Conversation, GitSnapshot, InheritedContext, Message, MessageKind, Role, SubagentParent,
    TranscriptFacts,
};
use crate::timefmt::parse_iso_ms;

pub const SOURCE: &str = "codex";
const ADAPTER_VERSION: u32 = 6;
const BODY_CAPTURE_ADAPTER_VERSION: u32 = 1;
const MAX_GOAL_OBJECTIVE_BYTES: u64 = 1024 * 1024;

/// Default Store root on a Host: `~/.codex`, containing both the dated
/// `sessions/` tree and the flat `archived_sessions/` tree.
pub fn default_store_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".codex")
}

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn archive_media(&self, capture: &Capture, archive_root: &Path) -> Result<()> {
        super::embedded_media::archive(capture, archive_root)
    }
    fn render_material(
        &self,
        capture: &Capture,
        archive_root: &Path,
        _context: &super::ExtractionContext,
    ) -> Result<crate::model::RenderMaterial> {
        super::embedded_media::material(capture, archive_root, self.extract(capture)?)
    }

    fn source(&self) -> &'static str {
        SOURCE
    }

    fn validates_superseded_identity(
        &self,
        canonical_native_id: &str,
        superseded_native_id: &str,
    ) -> bool {
        rollout_identity_from_filename(&format!("{superseded_native_id}.jsonl"))
            .is_some_and(|(identity, rollover)| rollover && identity == canonical_native_id)
    }

    /// Walk `archived_sessions/` (flat, always) and `sessions/YYYY/MM/DD/`
    /// with day-directory pruning against the Bookmark: a day directory whose
    /// own mtime is at or below the Bookmark gained or lost no entries since
    /// the last successful ingest, so its files need no stats — except that
    /// appends bump a file's mtime without touching its directory's, so every
    /// already-captured native path is additionally re-stat'd directly
    /// (cheap: one stat per captured Session) and enumerated on any
    /// mtime/size change. New files land in today's (unpruned) directory;
    /// restored files bump their directory's mtime by being created in it.
    fn enumerate(&self, store_root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
        let store_root = &std::fs::canonicalize(store_root).unwrap_or_else(|_| store_root.into());
        let catalog = read_thread_catalog(store_root)?;
        let mut rollouts = Vec::new();

        walk_rollouts(
            &store_root.join("archived_sessions"),
            0,
            None, // flat dir: nothing to prune
            &mut rollouts,
        )?;
        walk_rollouts(&store_root.join("sessions"), 0, gate.since(), &mut rollouts)?;

        // Growth re-stat: every physical file recorded by a captured Session
        // is checked directly, including segments inside pruned day dirs.
        if gate.since().is_some() {
            for (native_id, captured) in gate.captured_files() {
                let files: Vec<CapturedSourceFile> = if captured.source_files.is_empty() {
                    vec![CapturedSourceFile {
                        path: captured.native_path.to_string_lossy().into_owned(),
                        mtime_ms: captured.mtime_ms,
                        size: captured.size,
                        birth_time_ms: captured.native_birth_time_ms,
                    }]
                } else {
                    captured.source_files.clone()
                };
                for file in files {
                    let path = PathBuf::from(&file.path);
                    let path = std::fs::canonicalize(&path).unwrap_or(path);
                    let Ok(meta) = std::fs::metadata(&path) else {
                        continue;
                    };
                    let parsed = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(rollout_identity_from_filename);
                    rollouts.push(RolloutRef {
                        native_id: parsed
                            .as_ref()
                            .map(|(identity, _)| identity.clone())
                            .unwrap_or_else(|| native_id.to_string()),
                        is_rollover: parsed.is_some_and(|(_, rollover)| rollover),
                        file: NativeFileRef {
                            path,
                            mtime_ms: mtime_ms(&meta),
                            size: meta.len(),
                        },
                    });
                }
            }
        }

        // A rollover suffix proves that this logical Session may have older
        // physical segments below pruned day directories. Complete only
        // those groups with an unpruned pass; ordinary Sessions retain the
        // cheap dated-tree pruning path. Captured paths participate so an
        // upgrade repairs identities emitted by the older filename parser.
        let rollover_ids: HashSet<String> = rollouts
            .iter()
            .filter(|rollout| rollout.is_rollover)
            .map(|rollout| rollout.native_id.clone())
            .collect();
        if !rollover_ids.is_empty() {
            let mut all = Vec::new();
            walk_rollouts(&store_root.join("archived_sessions"), 0, None, &mut all)?;
            walk_rollouts(&store_root.join("sessions"), 0, None, &mut all)?;
            rollouts.extend(
                all.into_iter()
                    .filter(|rollout| rollover_ids.contains(&rollout.native_id)),
            );
        }

        let mut groups: BTreeMap<String, BTreeMap<String, NativeFileRef>> = BTreeMap::new();
        for rollout in rollouts {
            let filename = rollout
                .file
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let group = groups.entry(rollout.native_id).or_default();
            if let Some(existing) = group.get(&filename)
                && existing.path != rollout.file.path
            {
                anyhow::bail!(
                    "duplicate Codex rollout filename at {} and {}",
                    existing.path.display(),
                    rollout.file.path.display()
                );
            }
            group.insert(filename, rollout.file);
        }
        let mut refs = Vec::new();
        for (native_id, files) in groups {
            let mut files: Vec<NativeFileRef> = files.into_values().collect();
            files.sort_by(|left, right| {
                left.path
                    .file_name()
                    .cmp(&right.path.file_name())
                    .then_with(|| left.path.cmp(&right.path))
            });
            let mtime = files.iter().map(|file| file.mtime_ms).max().unwrap_or(0);
            let native_meta = catalog
                .names
                .get(&native_id)
                .map(|name| serde_json::json!({ "threadName": name }));
            if rollout_group_is_captured(
                gate,
                &native_id,
                &files,
                mtime,
                catalog.revision,
                native_meta.as_ref(),
            )? && !gate.requires_adapter_refresh(&native_id, ADAPTER_VERSION)
            {
                continue;
            }
            let primary = files.remove(0);
            refs.push(SessionRef {
                native_id,
                path: primary.path,
                additional_files: files,
                project_dir: None,
                kind: SessionKind::Main,
                mtime_ms: primary.mtime_ms,
                size: primary.size,
                native_meta_path: None,
                native_meta_mtime_ms: catalog.revision.map(|revision| revision.0),
                native_meta_size: catalog.revision.map(|revision| revision.1),
                native_meta,
            });
        }
        refs.sort_by(|a, b| a.native_id.cmp(&b.native_id));
        Ok(refs)
    }

    fn capture(&self, session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
        let mut capture = if session.additional_files.is_empty() {
            write_file_capture(
                SOURCE,
                session,
                captures_dir,
                host,
                "jsonl",
                // Publish the current adapter revision only after attachment
                // discovery succeeds. An interrupted enrichment remains eligible
                // for the next Capture pass.
                BODY_CAPTURE_ADAPTER_VERSION,
            )?
        } else {
            capture_rollout_group(session, captures_dir, host)?
        };
        let discovered = capture_goal_attachments(session, &capture)?;
        let merged = retain_resolved_goal_attachments(&capture.meta.goal_attachments, discovered);
        if capture.meta.adapter_version != ADAPTER_VERSION
            || capture.meta.goal_attachments != merged
        {
            capture.meta.adapter_version = ADAPTER_VERSION;
            capture.meta.goal_attachments = merged;
            capture.meta.captured_at_ms = now_ms();
            write_capture_meta(&capture.meta_path, &capture.meta)?;
            capture.written = true;
            if capture.outcome == super::CaptureOutcome::Unchanged {
                capture.outcome = super::CaptureOutcome::Extended;
            }
        }
        let mut superseded = capture.meta.superseded_native_ids.clone();
        superseded.extend(legacy_rollover_capture_ids(
            captures_dir,
            host,
            &session.native_id,
        )?);
        superseded.sort();
        superseded.dedup();
        for native_id in &superseded {
            anyhow::ensure!(
                self.validates_superseded_identity(&session.native_id, native_id),
                "invalid superseded Codex identity {native_id:?} for {:?}",
                session.native_id
            );
        }
        if capture.meta.superseded_native_ids != superseded {
            capture.meta.superseded_native_ids = superseded.clone();
            capture.meta.captured_at_ms = now_ms();
            write_capture_meta(&capture.meta_path, &capture.meta)?;
            capture.written = true;
            if capture.outcome == CaptureOutcome::Unchanged {
                capture.outcome = CaptureOutcome::Extended;
            }
        }
        for native_id in superseded {
            retire_legacy_capture(captures_dir, host, &native_id)?;
        }
        Ok(capture)
    }

    fn session_working_path(&self, session: &SessionRef) -> Result<Option<String>> {
        let file = std::fs::File::open(&session.path)?;
        recorded_cwd(&mut super::jsonl::JsonlReader::new(
            std::io::BufReader::new(file),
            session.path.display().to_string(),
        ))
    }

    fn capture_working_path(&self, capture: &Capture) -> Result<Option<String>> {
        recorded_cwd(&mut open_capture(capture)?)
    }

    /// Stream the Capture and extract tiers 1–3. Guardians and unproven
    /// inherited children remain Capture-only; direct children and children
    /// whose parent replay is identified by native response ids project their
    /// own Dialog.
    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>> {
        extract_conversation(capture, None)
    }

    /// The whole body's Dialog: guardians, gated children, and replayed
    /// parent items included, so two bodies of one Session compare equal
    /// exactly when they hold the same Messages.
    fn body_dialog(&self, capture: &Capture) -> Result<Conversation> {
        project_dialog(
            capture,
            Extraction::new(None, None, None, &capture.meta.goal_attachments),
        )
    }

    fn direct_parent_native_id(&self, capture: &Capture) -> Result<Option<String>> {
        Ok(inspect_child(capture)?
            .subagent_parent
            .map(|parent| parent.parent_native_id))
    }

    fn extract_with_parent(
        &self,
        capture: &Capture,
        direct_parent: Option<&Capture>,
    ) -> Result<Option<Conversation>> {
        extract_conversation(capture, direct_parent)
    }

    fn inspect(&self, capture: &Capture) -> Result<Value> {
        inspect_capture(capture)
    }
}

/// Read Codex's user-visible names from its WAL-aware catalog. The catalog
/// is optional on older installations; an existing but incompatible schema
/// is an explicit format error.
#[derive(Default)]
struct ThreadCatalog {
    names: BTreeMap<String, Option<String>>,
    revision: Option<(i64, u64)>,
}

fn read_thread_catalog(store_root: &Path) -> Result<ThreadCatalog> {
    let path = store_root.join("state_5.sqlite");
    if !path.is_file() {
        return Ok(ThreadCatalog::default());
    }
    for _ in 0..3 {
        let before = catalog_revision(&path);
        let conn = open_read_only(&path, "Codex")?;
        let columns = table_columns(&conn, "threads")?;
        anyhow::ensure!(
            columns.iter().any(|column| column == "id")
                && columns.iter().any(|column| column == "name"),
            "Codex Store {} has no threads(id,name) catalog",
            path.display()
        );
        let mut stmt = conn.prepare("SELECT id, name FROM threads")?;
        let names = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, name)| {
                let name = name.filter(|name| !name.trim().is_empty());
                (id, name)
            })
            .collect();
        let after = catalog_revision(&path);
        if before == after {
            return Ok(ThreadCatalog {
                names,
                revision: Some(after),
            });
        }
    }
    anyhow::bail!(
        "Codex Store {} changed repeatedly while reading its title catalog",
        path.display()
    )
}

fn catalog_revision(path: &Path) -> (i64, u64) {
    let mut revision_ms = 0;
    let mut revision_size = 0;
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        if let Ok(meta) = std::fs::metadata(candidate) {
            revision_ms = revision_ms.max(mtime_ms(&meta));
            revision_size += meta.len();
        }
    }
    (revision_ms, revision_size)
}

fn legacy_rollover_capture_ids(
    captures_dir: &Path,
    host: &str,
    canonical_id: &str,
) -> Result<Vec<String>> {
    let dir = captures_dir.join(host).join(SOURCE);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    let mut superseded = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.to_string_lossy().ends_with(".meta.json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<CaptureMeta>(&raw) else {
            continue;
        };
        if meta.native_id == canonical_id || meta.source != SOURCE {
            continue;
        }
        let Some(filename) = Path::new(&meta.native_path)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        if rollout_identity_from_filename(filename)
            .is_some_and(|(identity, rollover)| rollover && identity == canonical_id)
        {
            superseded.push(meta.native_id);
        }
    }
    superseded.sort();
    superseded.dedup();
    Ok(superseded)
}

fn retire_legacy_capture(captures_dir: &Path, host: &str, native_id: &str) -> Result<()> {
    anyhow::ensure!(
        Path::new(native_id)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(native_id),
        "unsafe superseded Codex Capture identity {native_id:?}"
    );
    let paths = super::capture_paths(captures_dir, host, SOURCE, native_id)?;
    let sources: Vec<PathBuf> = [paths.body, super::retry_sentinel(&paths.meta), paths.meta]
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    if sources.is_empty() {
        return Ok(());
    }
    let retired_dir = captures_dir
        .parent()
        .unwrap_or(captures_dir)
        .join("retired-captures")
        .join(host)
        .join(SOURCE)
        .join(format!("{native_id}-{}", now_ms()));
    std::fs::create_dir_all(&retired_dir)?;
    // Sidecar last: until it moves, interrupted retirement remains
    // discoverable; a body without a sidecar is never authoritative.
    for source in sources {
        let destination = retired_dir.join(source.file_name().unwrap_or_default());
        std::fs::rename(&source, &destination).with_context(|| {
            format!(
                "retiring superseded Codex Capture {} to {}",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

fn capture_goal_attachments(
    session: &SessionRef,
    capture: &Capture,
) -> Result<Vec<CapturedGoalAttachment>> {
    let mut references = BTreeSet::new();
    let mut reader = open_capture(capture)?;
    while let Some(line) = reader.next_raw()? {
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            continue;
        };
        if envelope.kind != Some("response_item") {
            continue;
        }
        let Some(payload) = envelope.payload else {
            continue;
        };
        let Ok(item) = serde_json::from_str::<ItemPayload>(payload.get()) else {
            continue;
        };
        if item.kind.as_deref() != Some("message") || item.role.as_deref() != Some("user") {
            continue;
        }
        for part in text_parts(parse_content(item.content).as_ref()) {
            let text = part.trim_start();
            if leading_tag(text) != Some("codex_internal_context") {
                continue;
            }
            if let Some(reference) =
                goal_context_objective(text).and_then(goal_attachment_reference)
            {
                references.insert(reference.to_string());
            }
        }
    }
    Ok(references
        .into_iter()
        .map(|reference| resolve_goal_attachment(&session.path, reference))
        .collect())
}

fn resolve_goal_attachment(session_path: &Path, native_path: String) -> CapturedGoalAttachment {
    let path = Path::new(&native_path);
    let Some(store_root) = codex_store_root(session_path) else {
        return rejected_goal_attachment(native_path);
    };
    let attachments_root = store_root.join("attachments");
    let Ok(relative) = path.strip_prefix(&attachments_root) else {
        return rejected_goal_attachment(native_path);
    };
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || components[1].as_os_str() != "goal-objective.md"
        || components[0].as_os_str().is_empty()
    {
        return rejected_goal_attachment(native_path);
    }

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(attachment = %path.display(), "goal objective attachment is missing");
            return CapturedGoalAttachment {
                native_path,
                status: GoalAttachmentStatus::Missing,
                text: None,
            };
        }
        Err(error) => {
            tracing::warn!(attachment = %path.display(), error = %error, "goal objective attachment is unreadable");
            return CapturedGoalAttachment {
                native_path,
                status: GoalAttachmentStatus::Unreadable,
                text: None,
            };
        }
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_GOAL_OBJECTIVE_BYTES {
        tracing::warn!(attachment = %path.display(), "goal objective attachment is not a safe bounded regular file");
        return rejected_goal_attachment(native_path);
    }
    let canonical_store = std::fs::canonicalize(store_root);
    let canonical_root = std::fs::canonicalize(&attachments_root);
    let canonical_path = std::fs::canonicalize(path);
    if !matches!(
        (canonical_store, canonical_root, canonical_path),
        (Ok(store), Ok(root), Ok(file))
            if root == store.join("attachments") && file.starts_with(&root)
    ) {
        return rejected_goal_attachment(native_path);
    }
    match std::fs::read_to_string(path) {
        Ok(text) => CapturedGoalAttachment {
            native_path,
            status: GoalAttachmentStatus::Resolved,
            text: Some(text),
        },
        Err(error) => {
            tracing::warn!(attachment = %path.display(), error = %error, "goal objective attachment is not valid readable UTF-8");
            CapturedGoalAttachment {
                native_path,
                status: GoalAttachmentStatus::Unreadable,
                text: None,
            }
        }
    }
}

fn rejected_goal_attachment(native_path: String) -> CapturedGoalAttachment {
    tracing::warn!(
        attachment = native_path,
        "rejecting goal objective attachment outside the Codex attachment tree"
    );
    CapturedGoalAttachment {
        native_path,
        status: GoalAttachmentStatus::Rejected,
        text: None,
    }
}

fn codex_store_root(session_path: &Path) -> Option<&Path> {
    session_path.ancestors().find_map(|ancestor| {
        matches!(
            ancestor.file_name().and_then(|name| name.to_str()),
            Some("sessions" | "archived_sessions")
        )
        .then(|| ancestor.parent())
        .flatten()
    })
}

fn retain_resolved_goal_attachments(
    existing: &[CapturedGoalAttachment],
    discovered: Vec<CapturedGoalAttachment>,
) -> Vec<CapturedGoalAttachment> {
    discovered
        .into_iter()
        .map(|candidate| {
            existing
                .iter()
                .find(|prior| {
                    prior.native_path == candidate.native_path
                        && prior.status == GoalAttachmentStatus::Resolved
                        && candidate.status != GoalAttachmentStatus::Resolved
                })
                .cloned()
                .unwrap_or(candidate)
        })
        .collect()
}

fn extract_conversation(
    capture: &Capture,
    direct_parent: Option<&Capture>,
) -> Result<Option<Conversation>> {
    let child = inspect_child(capture)?;
    if child.is_guardian || (child.is_subagent && child.subagent_parent.is_none()) {
        return Ok(None);
    }
    let parent_item_ids = if child.has_foreign_meta {
        if !child.dialog_item_ids_complete {
            return Ok(None);
        }
        let Some(parent) = direct_parent else {
            return Ok(None);
        };
        let Some(ids) = dialog_item_ids(parent)? else {
            return Ok(None);
        };
        Some(ids)
    } else {
        None
    };

    project_dialog(
        capture,
        Extraction::new(
            child.subagent_parent,
            parent_item_ids.as_ref(),
            direct_parent.map(|parent| parent.meta.native_id.as_str()),
            &capture.meta.goal_attachments,
        ),
    )
    .map(Some)
}

/// Stream every record of `capture` through `state`.
fn project_dialog(capture: &Capture, mut state: Extraction<'_>) -> Result<Conversation> {
    let mut reader = open_capture(capture)?;
    while let Some(line) = reader.next_raw()? {
        let Ok(text) = std::str::from_utf8(line) else {
            reader.warn_malformed();
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            reader.warn_malformed();
            continue;
        };
        state.record(&envelope, capture)?;
    }
    Ok(state.finish(capture))
}

// ---------------------------------------------------------------------------
// Typed line shapes: only what extraction needs; everything else is skipped
// by serde without allocation (the payload is a borrowed RawValue).

#[derive(Deserialize)]
struct Envelope<'a> {
    timestamp: Option<&'a str>,
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
    // Legacy top-level `{type:"message", role, content}` lines (pre-payload
    // era) — cheap tolerance, zero current census.
    role: Option<&'a str>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct MetaPayload {
    id: Option<String>,
    cwd: Option<String>,
    thread_source: Option<String>,
    parent_thread_id: Option<String>,
    #[serde(rename = "parentThreadId")]
    parent_thread_id_camel: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    agent_path: Option<String>,
    /// String-or-object union; only the object form carries `{"subagent":…}`.
    source: Option<Value>,
    git: Option<GitPayload>,
}

#[derive(Deserialize)]
struct TurnPayload {
    cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct GitPayload {
    branch: Option<String>,
    commit_hash: Option<String>,
    repository_url: Option<String>,
}

#[derive(Deserialize)]
struct ThreadSettingsPayload {
    model: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
struct InfoMetaPayload {
    originator: Option<String>,
    cli_version: Option<String>,
    thread_source: Option<String>,
    model_provider: Option<String>,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct InfoTurnPayload {
    model: Option<String>,
    effort: Option<String>,
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
}

/// `response_item` head. `content` stays a borrowed RawValue and is parsed
/// only for user/assistant roles — developer/system messages and non-message
/// items (tool outputs reach ~20 MB) never materialize their content.
#[derive(Deserialize)]
struct ItemPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    namespace: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    output: Option<&'a RawValue>,
}

/// `event_msg` head. `message` (assistant-text duplicates on every turn) is
/// parsed to an owned String only when the fallback actually needs it.
#[derive(Deserialize)]
struct EventPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(borrow)]
    thread_name: Option<&'a RawValue>,
    success: Option<bool>,
    #[serde(borrow)]
    changes: Option<&'a RawValue>,
    #[serde(borrow)]
    thread_settings: Option<&'a RawValue>,
}

/// Parse a RawValue holding a JSON string.
fn raw_str(raw: Option<&RawValue>) -> Option<String> {
    raw.and_then(|r| serde_json::from_str::<String>(r.get()).ok())
}

/// Extract object keys while intentionally discarding values such as diffs.
fn raw_object_keys(raw: Option<&RawValue>) -> Vec<String> {
    raw.and_then(|raw| {
        serde_json::from_str::<BTreeMap<String, serde::de::IgnoredAny>>(raw.get()).ok()
    })
    .map(|object| object.into_keys().collect())
    .unwrap_or_default()
}

/// Read a small, explicit diagnostic projection on demand. Unlike ordinary
/// extraction, this intentionally retains configuration changes and the last
/// usage report, but never instructions, tool schemas, workspace roots, or
/// arbitrary tool data.
fn inspect_capture(capture: &Capture) -> Result<Value> {
    let mut reader = open_capture(capture)?;
    let mut initial = serde_json::Map::new();
    let mut execution_context = serde_json::Map::new();
    let mut configuration_timeline = Vec::new();
    let mut latest_usage = None;
    let mut saw_meta = false;

    while let Some(raw) = reader.next_raw()? {
        let Ok(text) = std::str::from_utf8(raw) else {
            reader.warn_malformed();
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            reader.warn_malformed();
            continue;
        };
        let Some(payload) = envelope.payload else {
            continue;
        };
        match envelope.kind {
            Some("session_meta") if !saw_meta => {
                saw_meta = true;
                if let Ok(meta) = serde_json::from_str::<InfoMetaPayload>(payload.get()) {
                    if let Some(originator) = meta.originator {
                        initial.insert("originator".into(), Value::String(originator));
                    }
                    if let Some(cli_version) = meta.cli_version {
                        initial.insert("cliVersion".into(), Value::String(cli_version));
                    }
                    if let Some(thread_source) = meta.thread_source {
                        initial.insert("threadSource".into(), Value::String(thread_source));
                    }
                    if let Some(model_provider) = meta.model_provider {
                        initial.insert("modelProvider".into(), Value::String(model_provider));
                    }
                    if let Some(context_window) = meta.context_window {
                        initial.insert("contextWindow".into(), Value::from(context_window));
                    }
                }
            }
            Some("turn_context") => {
                let Ok(turn) = serde_json::from_str::<InfoTurnPayload>(payload.get()) else {
                    continue;
                };
                if let Some(approval_policy) = turn.approval_policy {
                    execution_context
                        .insert("approvalPolicy".into(), Value::String(approval_policy));
                }
                if let Some(sandbox_policy) = turn.sandbox_policy {
                    execution_context.insert("sandboxPolicy".into(), Value::String(sandbox_policy));
                }
                if turn.model.is_some() || turn.effort.is_some() {
                    configuration_timeline.push(serde_json::json!({
                        "timestamp": envelope.timestamp,
                        "model": turn.model,
                        "reasoningEffort": turn.effort,
                        "origin": "turn_context",
                    }));
                }
            }
            Some("event_msg") => {
                let Ok(event) = serde_json::from_str::<EventPayload>(payload.get()) else {
                    continue;
                };
                match event.kind.as_deref() {
                    Some("token_count") => {
                        latest_usage = serde_json::from_str::<Value>(payload.get()).ok();
                    }
                    Some("thread_settings_applied") => {
                        if let Some(raw_settings) = event.thread_settings
                            && let Ok(settings) =
                                serde_json::from_str::<ThreadSettingsPayload>(raw_settings.get())
                            && (settings.model.is_some() || settings.reasoning_effort.is_some())
                        {
                            configuration_timeline.push(serde_json::json!({
                                "timestamp": envelope.timestamp,
                                "model": settings.model,
                                "reasoningEffort": settings.reasoning_effort,
                                "origin": "thread_settings_applied",
                            }));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let mut details = serde_json::Map::new();
    if !initial.is_empty() {
        details.insert("initial".into(), Value::Object(initial));
    }
    if !configuration_timeline.is_empty() {
        details.insert(
            "configurationTimeline".into(),
            Value::Array(configuration_timeline),
        );
    }
    if !execution_context.is_empty() {
        details.insert("executionContext".into(), Value::Object(execution_context));
    }
    if let Some(latest_usage) = latest_usage {
        details.insert("latestUsage".into(), latest_usage);
    }
    Ok(Value::Object(details))
}

/// Message content: a raw string (legacy) or an array of blocks. Deserialized
/// via a manual visitor — an untagged enum would buffer the whole content
/// (base64 images included) to retry variants, defeating bounded memory.
enum ContentField {
    Text(String),
    Blocks(Vec<ContentBlock>),
    Other,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    // `image_url` (base64) and friends are skipped, never materialized.
}

impl<'de> Deserialize<'de> for ContentField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ContentField;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string or an array of content blocks")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ContentField::Text(v.to_string()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(ContentField::Text(v))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut blocks = Vec::new();
                while let Some(block) = seq.next_element::<ContentBlock>()? {
                    blocks.push(block);
                }
                Ok(ContentField::Blocks(blocks))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ContentField::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                use serde::de::IgnoredAny;
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(ContentField::Other)
            }
        }
        deserializer.deserialize_any(V)
    }
}

// ---------------------------------------------------------------------------
// Extraction state machine.

#[derive(Default)]
struct ChildInspection {
    is_subagent: bool,
    is_guardian: bool,
    has_foreign_meta: bool,
    dialog_item_ids_complete: bool,
    saw_first_meta: bool,
    subagent_parent: Option<SubagentParent>,
}

fn inspect_child(capture: &Capture) -> Result<ChildInspection> {
    let mut reader = open_capture(capture)?;
    let mut inspection = ChildInspection {
        dialog_item_ids_complete: true,
        ..Default::default()
    };
    while let Some(line) = reader.next_raw()? {
        let Ok(text) = std::str::from_utf8(line) else {
            inspection.dialog_item_ids_complete = false;
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            inspection.dialog_item_ids_complete = false;
            continue;
        };
        match (envelope.kind, envelope.payload) {
            (Some("session_meta"), Some(payload)) => {
                let Ok(meta) = serde_json::from_str::<MetaPayload>(payload.get()) else {
                    inspection.dialog_item_ids_complete = false;
                    continue;
                };
                if !inspection.saw_first_meta {
                    inspection.saw_first_meta = true;
                    if let Some(id) = &meta.id
                        && id != &capture.meta.native_id
                    {
                        tracing::warn!(
                            capture = %capture.body_path.display(),
                            meta_id = id,
                            "first session_meta id differs from filename identity"
                        );
                    }
                    inspection.is_subagent = subagent_flag(&meta).is_some();
                    inspection.is_guardian = subagent_flag(&meta) == Some("guardian");
                    inspection.subagent_parent = child_parent_fact(&meta);
                } else {
                    match meta.id.as_deref() {
                        Some(id) if id != capture.meta.native_id => {
                            inspection.has_foreign_meta = true;
                        }
                        Some(_) => {}
                        None => {
                            // An id-less later meta can be either a resume or
                            // a replayed ancestor. Never call that boundary.
                            inspection.has_foreign_meta = true;
                            inspection.dialog_item_ids_complete = false;
                        }
                    }
                }
            }
            (Some("response_item"), Some(payload)) => {
                let Ok(item) = serde_json::from_str::<ItemPayload>(payload.get()) else {
                    inspection.dialog_item_ids_complete = false;
                    continue;
                };
                if is_dialog_item(&item) && item.id.as_deref().is_none_or(str::is_empty) {
                    inspection.dialog_item_ids_complete = false;
                }
            }
            // A copied v1-style top-level Message has no response-item id.
            (Some("message"), None)
                if matches!(envelope.role, Some("user") | Some("assistant")) =>
            {
                inspection.dialog_item_ids_complete = false;
            }
            _ => {}
        }
    }
    Ok(inspection)
}

fn child_parent_fact(meta: &MetaPayload) -> Option<SubagentParent> {
    let spawn = meta.source.as_ref()?.get("subagent")?.get("thread_spawn")?;
    let parent_native_id = value_str(spawn, "parent_thread_id")
        .or_else(|| value_str(spawn, "parentThreadId"))
        .or(meta.parent_thread_id.as_deref())
        .or(meta.parent_thread_id_camel.as_deref())?
        .to_string();
    (!parent_native_id.trim().is_empty()).then(|| SubagentParent {
        parent_native_id,
        description: value_str(spawn, "agent_nickname")
            .or(meta.agent_nickname.as_deref())
            .map(str::to_string),
        agent_type: value_str(spawn, "agent_role")
            .or(meta.agent_role.as_deref())
            .map(str::to_string),
        instructions: None,
        depth: spawn
            .get("depth")
            .and_then(Value::as_u64)
            .and_then(|depth| u32::try_from(depth).ok()),
        native_key: value_str(spawn, "agent_path")
            .or(meta.agent_path.as_deref())
            .map(str::to_string),
        link_key: None,
    })
}

fn value_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn is_dialog_item(item: &ItemPayload<'_>) -> bool {
    item.kind.as_deref() == Some("message")
        && matches!(item.role.as_deref(), Some("user") | Some("assistant"))
}

fn dialog_item_ids(capture: &Capture) -> Result<Option<HashSet<String>>> {
    let mut reader = open_capture(capture)?;
    let mut ids = HashSet::new();
    while let Some(line) = reader.next_raw()? {
        let Ok(text) = std::str::from_utf8(line) else {
            return Ok(None);
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            return Ok(None);
        };
        if envelope.kind != Some("response_item") {
            continue;
        }
        let Some(payload) = envelope.payload else {
            return Ok(None);
        };
        let Ok(item) = serde_json::from_str::<ItemPayload>(payload.get()) else {
            return Ok(None);
        };
        if is_dialog_item(&item) {
            let Some(id) = item.id.as_deref().filter(|id| !id.is_empty()) else {
                return Ok(None);
            };
            ids.insert(id.to_string());
        }
    }
    Ok(Some(ids))
}

struct Extraction<'a> {
    messages: Vec<Message>,
    pending_actions: u32,
    title: Option<String>,
    cwd: Option<String>,
    created_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    saw_first_meta: bool,
    is_agent_created_thread: bool,
    /// An agent-created task may begin with injected `message` items (all
    /// omitted by normal filtering) before the delegation result. Any other
    /// response item means this is no longer that opening preamble.
    agent_task_preamble_only: bool,
    /// event_msg `agent_message` fallback: indices of provisionally inserted
    /// assistant Messages, purged the moment real assistant speech (a
    /// `response_item` message) appears.
    fallback_indices: Vec<usize>,
    saw_real_assistant: bool,
    subagent_parent: Option<SubagentParent>,
    skipped_item_ids: Option<&'a HashSet<String>>,
    inherited_source_id: Option<&'a str>,
    goal_attachments: &'a [CapturedGoalAttachment],
    active_goal: Option<String>,
    copied_item_count: u32,
    facts: TranscriptFacts,
    settings_model: Option<String>,
    settings_reasoning_effort: Option<String>,
}

impl<'a> Extraction<'a> {
    fn new(
        subagent_parent: Option<SubagentParent>,
        skipped_item_ids: Option<&'a HashSet<String>>,
        inherited_source_id: Option<&'a str>,
        goal_attachments: &'a [CapturedGoalAttachment],
    ) -> Self {
        Self {
            messages: Vec::new(),
            pending_actions: 0,
            title: None,
            cwd: None,
            created_at_ms: None,
            updated_at_ms: None,
            saw_first_meta: false,
            is_agent_created_thread: false,
            agent_task_preamble_only: true,
            fallback_indices: Vec::new(),
            saw_real_assistant: false,
            subagent_parent,
            skipped_item_ids,
            inherited_source_id,
            goal_attachments,
            active_goal: None,
            copied_item_count: 0,
            facts: TranscriptFacts::default(),
            settings_model: None,
            settings_reasoning_effort: None,
        }
    }

    fn record(&mut self, envelope: &Envelope, capture: &Capture) -> Result<()> {
        let ts = envelope.timestamp.and_then(|s| parse_iso_ms(s).ok());
        if let Some(ts) = ts {
            self.created_at_ms.get_or_insert(ts);
            self.updated_at_ms = Some(self.updated_at_ms.map_or(ts, |u: i64| u.max(ts)));
        }
        match (envelope.kind, envelope.payload) {
            (Some("session_meta"), Some(payload)) => {
                // Only the FIRST meta speaks for this file: metas recur on
                // resume, and fork files replay their ANCESTORS' metas —
                // trusting a later one would assign the parent's identity
                // and cwd (replayed metadata; docs/formats/codex.md §2).
                if self.saw_first_meta {
                    return Ok(());
                }
                self.saw_first_meta = true;
                let Ok(meta) = serde_json::from_str::<MetaPayload>(payload.get()) else {
                    return Ok(());
                };
                if let Some(id) = &meta.id
                    && id != &capture.meta.native_id
                {
                    tracing::warn!(
                        capture = %capture.body_path.display(),
                        meta_id = id,
                        "first session_meta id differs from filename identity"
                    );
                }
                if meta.cwd.is_some() {
                    self.cwd = meta.cwd;
                }
                self.is_agent_created_thread =
                    meta.thread_source.as_deref() == Some("agent_created_thread");
                if let Some(git) = meta.git {
                    let git = GitSnapshot {
                        branch: git.branch.filter(|value| !value.trim().is_empty()),
                        commit: git.commit_hash.filter(|value| !value.trim().is_empty()),
                        repository_url: git.repository_url.filter(|value| !value.trim().is_empty()),
                    };
                    if !git.is_empty() {
                        self.facts.git = Some(git);
                    }
                }
            }
            (Some("turn_context"), Some(payload)) => {
                // Per-turn cwd updates: last wins; model and effort are
                // deliberately first-wins for compact transcript metadata.
                if let Ok(turn) = serde_json::from_str::<TurnPayload>(payload.get()) {
                    if turn.cwd.is_some() {
                        self.cwd = turn.cwd;
                    }
                    if let Some(model) = turn.model.filter(|value| !value.trim().is_empty()) {
                        self.facts.starting_model.get_or_insert(model);
                    }
                    if let Some(effort) = turn.effort.filter(|value| !value.trim().is_empty()) {
                        self.facts.starting_reasoning_effort.get_or_insert(effort);
                    }
                }
            }
            (Some("response_item"), Some(payload)) => {
                let Ok(item) = serde_json::from_str::<ItemPayload>(payload.get()) else {
                    return Ok(());
                };
                if item.id.as_deref().is_some_and(|id| {
                    self.skipped_item_ids
                        .is_some_and(|parent_ids| parent_ids.contains(id))
                }) {
                    self.pending_actions = 0;
                    self.copied_item_count += 1;
                    return Ok(());
                }
                match item.kind.as_deref() {
                    Some("message") => self.message(item.role.as_deref(), item.content, ts),
                    // An agent-created task's opening prompt is delivered as
                    // its own `create_thread` result, not as a user message.
                    // It can follow only omitted injected `message` context;
                    // every other preceding response item disqualifies it.
                    Some("function_call_output")
                        if self.is_agent_created_thread
                            && self.agent_task_preamble_only
                            && self.messages.is_empty() =>
                    {
                        self.agent_task_input(&item, ts);
                        self.agent_task_preamble_only = false;
                    }
                    kind => {
                        self.agent_task_preamble_only = false;
                        // Actions: count the calls for seam markers; outputs,
                        // reasoning, and inter-agent mail are dropped silently.
                        match kind {
                            Some("function_call")
                            | Some("custom_tool_call")
                            | Some("web_search_call")
                            | Some("tool_search_call") => self.pending_actions += 1,
                            _ => {}
                        }
                    }
                }
            }
            (Some("event_msg"), Some(payload)) => {
                let Ok(event) = serde_json::from_str::<EventPayload>(payload.get()) else {
                    return Ok(());
                };
                match event.kind.as_deref() {
                    // A native title; latest wins.
                    Some("thread_name_updated") => {
                        if let Some(name) = raw_str(event.thread_name)
                            && !name.trim().is_empty()
                        {
                            self.title = Some(name);
                        }
                    }
                    Some("thread_settings_applied") => {
                        if let Some(raw) = event.thread_settings
                            && let Ok(settings) =
                                serde_json::from_str::<ThreadSettingsPayload>(raw.get())
                        {
                            if let Some(model) =
                                settings.model.filter(|value| !value.trim().is_empty())
                            {
                                self.settings_model.get_or_insert(model);
                            }
                            if let Some(effort) = settings
                                .reasoning_effort
                                .filter(|value| !value.trim().is_empty())
                            {
                                self.settings_reasoning_effort.get_or_insert(effort);
                            }
                        }
                    }
                    Some("patch_apply_end") if event.success == Some(true) => {
                        self.facts
                            .patched_files
                            .extend(raw_object_keys(event.changes));
                    }
                    // Fallback assistant speech, only while no real assistant
                    // response_item message has appeared (old/degenerate
                    // files); duplicates otherwise.
                    Some("agent_message")
                        if self.skipped_item_ids.is_none() && !self.saw_real_assistant =>
                    {
                        if let Some(text) = raw_str(event.message)
                            && !text.trim().is_empty()
                        {
                            self.fallback_indices.push(self.messages.len());
                            let actions_before = std::mem::take(&mut self.pending_actions);
                            self.messages.push(Message {
                                role: Role::Assistant,
                                kind: MessageKind::Dialog,
                                text,
                                timestamp_ms: ts,
                                actions_before,
                            });
                        }
                    }
                    _ => {}
                }
            }
            // Legacy top-level message line.
            (Some("message"), None) => {
                self.message(envelope.role, envelope.content, ts);
            }
            // compacted, world_state, inter_agent_communication_metadata,
            // and future record types are absorbed silently.
            _ => {}
        }
        Ok(())
    }

    /// One `message` payload: role dispatch, injected-context filtering, and
    /// the fallback purge on the first real assistant speech. `content` is
    /// only parsed for the two Dialog roles.
    fn message(&mut self, role: Option<&str>, content: Option<&RawValue>, ts: Option<i64>) {
        match role {
            Some("assistant") => {
                let text = text_parts(parse_content(content).as_ref()).join("\n\n");
                if text.trim().is_empty() {
                    return;
                }
                if !self.saw_real_assistant {
                    self.saw_real_assistant = true;
                    purge_fallbacks(
                        &mut self.messages,
                        &mut self.fallback_indices,
                        &mut self.pending_actions,
                    );
                }
                let actions_before = std::mem::take(&mut self.pending_actions);
                self.messages.push(Message {
                    role: Role::Assistant,
                    kind: MessageKind::Dialog,
                    text,
                    timestamp_ms: ts,
                    actions_before,
                });
            }
            Some("user") => {
                let Some(user) =
                    user_content(parse_content(content).as_ref(), self.goal_attachments)
                else {
                    return;
                };
                let (kind, text) = match user {
                    UserContent::Dialog(text) => (MessageKind::Dialog, text),
                    UserContent::Goal { text, unresolved } => {
                        let identity = text.trim().to_string();
                        if self.active_goal.as_deref() == Some(identity.as_str()) {
                            (
                                MessageKind::GoalContinuation,
                                "Continue working toward the active thread goal.".to_string(),
                            )
                        } else {
                            self.active_goal = Some(identity);
                            (
                                if unresolved {
                                    MessageKind::GoalUnresolved
                                } else {
                                    MessageKind::Goal
                                },
                                text,
                            )
                        }
                    }
                };
                let actions_before = std::mem::take(&mut self.pending_actions);
                self.messages.push(Message {
                    role: Role::User,
                    kind,
                    text,
                    timestamp_ms: ts,
                    actions_before,
                });
            }
            // developer / system: injected instructions, never Dialog.
            _ => {}
        }
    }

    fn agent_task_input(&mut self, item: &ItemPayload<'_>, ts: Option<i64>) {
        let Some(text) = agent_created_task_input(item) else {
            return;
        };
        let actions_before = std::mem::take(&mut self.pending_actions);
        self.messages.push(Message {
            role: Role::Agent,
            kind: MessageKind::Dialog,
            text,
            timestamp_ms: ts,
            actions_before,
        });
    }

    fn finish(self, capture: &Capture) -> Conversation {
        let mut facts = self.facts;
        if facts.starting_model.is_none() {
            facts.starting_model = self.settings_model;
        }
        if facts.starting_reasoning_effort.is_none() {
            facts.starting_reasoning_effort = self.settings_reasoning_effort;
        }
        facts.normalize_file_paths(self.cwd.as_deref());
        Conversation {
            provenance: capture.provenance(),
            title: capture
                .meta
                .native_meta
                .as_ref()
                .and_then(|meta| meta.get("threadName"))
                .map(|name| {
                    name.as_str()
                        .filter(|name| !name.trim().is_empty())
                        .map(str::to_string)
                })
                .unwrap_or(self.title),
            project_path: self.cwd,
            native_project: None,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            messages: self.messages,
            transcript_facts: facts,
            subagent_parent: self.subagent_parent,
            inherited_context: self
                .inherited_source_id
                .map(|source_native_id| InheritedContext {
                    source_native_id: source_native_id.to_string(),
                    copied_item_count: self.copied_item_count,
                }),
            subagent_references: Vec::new(),
            branches: Vec::new(),
            subagent_links: Vec::new(), // slice 5
            trailing_actions: self.pending_actions,
        }
    }
}

#[derive(Debug)]
struct RolloutRef {
    native_id: String,
    is_rollover: bool,
    file: NativeFileRef,
}

/// Recursive walk over `rollout-*.jsonl`. `prune_below` (set only for the
/// dated tree when a Bookmark exists) skips day directories — depth 3, the
/// only level whose own mtime reflects every entry change inside it — whose
/// mtime is at or below the Bookmark.
fn walk_rollouts(
    dir: &Path,
    depth: u32,
    prune_below: Option<i64>,
    refs: &mut Vec<RolloutRef>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if depth == 2
                && let Some(bookmark) = prune_below
                && mtime_ms(&entry.metadata()?) <= bookmark
            {
                continue; // pruned day directory; growth re-stat covers it
            }
            walk_rollouts(&path, depth + 1, prune_below, refs)?;
        } else if ft.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let (native_id, is_rollover) =
                rollout_identity_from_filename(&name).unwrap_or_else(|| {
                    tracing::warn!(file = %path.display(), "unrecognized rollout filename shape");
                    (name.trim_end_matches(".jsonl").to_string(), false)
                });
            let meta = entry.metadata()?;
            refs.push(RolloutRef {
                native_id,
                is_rollover,
                file: NativeFileRef {
                    path,
                    mtime_ms: mtime_ms(&meta),
                    size: meta.len(),
                },
            });
        }
    }
    Ok(())
}

/// `rollout-<YYYY-MM-DDThh-mm-ss>-<uuid>.jsonl` → the uuid (the native id,
/// confirmed against the first `session_meta` at Extract time).
fn rollout_identity_from_filename(name: &str) -> Option<(String, bool)> {
    let stem = name.strip_suffix(".jsonl")?;
    let rest = stem.strip_prefix("rollout-")?;
    // 19-char local timestamp, then `-`, then the uuid.
    if rest.len() < 21 || rest.as_bytes()[19] != b'-' {
        return None;
    }
    let identity = &rest[20..];
    let (uuid, segment) = identity
        .split_once('_')
        .map_or((identity, None), |(uuid, segment)| (uuid, Some(segment)));
    if !is_uuid(uuid) || segment.is_some_and(|segment| !is_uuid(segment)) {
        return None;
    }
    Some((uuid.to_string(), segment.is_some()))
}

/// Admission uses the first Session header, never replayed ancestor metadata
/// or per-turn context. Both native and archived readers share this rule.
fn recorded_cwd<R: std::io::BufRead>(
    reader: &mut super::jsonl::JsonlReader<R>,
) -> Result<Option<String>> {
    while let Some(record) = reader.next_record()? {
        if record["type"] == "session_meta" {
            return Ok(record["payload"]["cwd"].as_str().map(str::to_owned));
        }
    }
    Ok(None)
}

#[cfg(test)]
fn native_id_from_filename(name: &str) -> Option<String> {
    rollout_identity_from_filename(name).map(|(native_id, _)| native_id)
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// `"guardian"` / `"subagent"` when the first `session_meta` marks this file
/// as an automated child thread; `None` for user/automation threads.
fn subagent_flag(meta: &MetaPayload) -> Option<&'static str> {
    let subagent_source = meta.source.as_ref().and_then(|s| s.get("subagent"));
    let flagged = subagent_source.is_some() || meta.thread_source.as_deref() == Some("subagent");
    if !flagged {
        return None;
    }
    let guardian = subagent_source
        .and_then(|s| s.get("other"))
        .and_then(Value::as_str)
        == Some("guardian");
    Some(if guardian { "guardian" } else { "subagent" })
}

/// Remove provisional fallback Messages, folding their omitted-Action counts
/// into the next surviving Message (or back into the pending counter).
fn purge_fallbacks(messages: &mut Vec<Message>, indices: &mut Vec<usize>, pending: &mut u32) {
    if indices.is_empty() {
        return;
    }
    let marked: HashSet<usize> = indices.drain(..).collect();
    let mut carry: u32 = 0;
    let old = std::mem::take(messages);
    for (i, mut m) in old.into_iter().enumerate() {
        if marked.contains(&i) {
            carry += m.actions_before;
            continue;
        }
        m.actions_before += carry;
        carry = 0;
        messages.push(m);
    }
    *pending += carry;
}

/// Parse message content from its raw slice; `None` when absent or
/// unparseable.
fn parse_content(raw: Option<&RawValue>) -> Option<ContentField> {
    raw.and_then(|r| serde_json::from_str::<ContentField>(r.get()).ok())
}

/// The text parts of message content: a raw string, or the `.text` of
/// `input_text` / `output_text` / legacy `text` blocks; `input_image` becomes
/// a placeholder (its base64 was never materialized).
fn text_parts(content: Option<&ContentField>) -> Vec<String> {
    match content {
        Some(ContentField::Text(s)) => vec![s.clone()],
        Some(ContentField::Blocks(blocks)) => blocks
            .iter()
            .filter_map(|block| match block.kind.as_deref() {
                Some("input_text") | Some("output_text") | Some("text") => {
                    block.text.clone().filter(|t| !t.is_empty())
                }
                Some("input_image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

enum UserContent {
    Dialog(String),
    Goal { text: String, unresolved: bool },
}

#[derive(Deserialize)]
#[serde(rename = "realtime_delegation", deny_unknown_fields)]
struct RealtimeDelegation {
    #[serde(default)]
    source: Option<String>,
    input: String,
    #[serde(default)]
    transcript_delta: Option<String>,
}

/// The one non-Action function result that starts an agent-created Codex task.
/// It is recorded in the child Session itself as `codex_app.create_thread`
/// output, rather than as a user Message.
#[derive(Deserialize)]
#[serde(rename = "codex_delegation", deny_unknown_fields)]
struct AgentCreatedTaskDelegation {
    source_thread_id: String,
    input: String,
}

fn agent_created_task_input(item: &ItemPayload<'_>) -> Option<String> {
    if item.namespace.as_deref() != Some("codex_app")
        || item.name.as_deref() != Some("create_thread")
    {
        return None;
    }
    let output = raw_str(item.output)?;
    let output = output.trim();
    if !output.starts_with("<codex_delegation>") || !output.ends_with("</codex_delegation>") {
        return None;
    }
    let delegation = quick_xml::de::from_str::<AgentCreatedTaskDelegation>(output).ok()?;
    if delegation.source_thread_id.trim().is_empty() {
        return None;
    }
    let input = delegation.input.trim();
    (!input.is_empty()).then(|| input.to_string())
}

/// Injected-tag filtering and persistent-goal normalization for user-role
/// messages (docs/formats/codex.md §6). A Goal envelope contributes only its
/// user objective; all sibling/runtime content is discarded atomically.
fn user_content(
    content: Option<&ContentField>,
    goal_attachments: &[CapturedGoalAttachment],
) -> Option<UserContent> {
    let parts = text_parts(content);
    for part in &parts {
        let t = part.trim_start();
        if leading_tag(t) != Some("codex_internal_context") {
            continue;
        }
        let objective = goal_context_objective(t)?;
        let Some(reference) = goal_attachment_reference(objective) else {
            return Some(UserContent::Goal {
                text: objective.to_string(),
                unresolved: false,
            });
        };
        let attachment = goal_attachments
            .iter()
            .find(|attachment| attachment.native_path == reference);
        return match attachment {
            Some(CapturedGoalAttachment {
                status: GoalAttachmentStatus::Resolved,
                text: Some(text),
                ..
            }) => Some(UserContent::Goal {
                text: text.clone(),
                unresolved: false,
            }),
            _ => Some(UserContent::Goal {
                text: objective.to_string(),
                unresolved: true,
            }),
        };
    }

    if let Some(realtime) = parts
        .iter()
        .find(|part| leading_tag(part.trim_start()) == Some("realtime_delegation"))
    {
        if parts.len() != 1 {
            return None;
        }
        return realtime_delegation_input(realtime.trim_start()).map(UserContent::Dialog);
    }

    // Composer Prompts contribute their typed request; Machine Context
    // preambles are set aside and stand in only for an otherwise empty
    // Message, so no user Message vanishes.
    let mut context_only: Vec<String> = Vec::new();
    let mut parts_after_composer: Vec<String> = Vec::new();
    for part in &parts {
        match composer::parse(part) {
            ComposedPart::Verbatim(text) | ComposedPart::Request(text) => {
                parts_after_composer.push(text.to_string());
            }
            ComposedPart::ContextOnly(text) => context_only.push(text.to_string()),
        }
    }
    let parts = parts_after_composer;

    // Only explicitly classified machine tags drop a message. Unknown tags
    // survive so future vendor envelopes cannot erase user words.
    for part in &parts {
        let t = part.trim_start();
        if is_agents_md_instructions(t) {
            return None;
        }
        if is_attachment_metadata(t) {
            continue;
        }
        let Some(tag) = leading_tag(t) else { continue };
        match tag {
            "turn_aborted" | "subagent_notification" | "send_user_message_question_reply" => {}
            known if is_known_drop_tag(known) => return None,
            unknown => {
                tracing::warn!(
                    tag = unknown,
                    "keeping user message with unknown injected tag"
                );
            }
        }
    }
    let mut kept: Vec<String> = Vec::new();
    for part in &parts {
        if is_attachment_metadata(part) {
            continue;
        }
        if leading_tag(part.trim_start()) == Some("send_user_message_question_reply") {
            kept.push(question_reply_text(part.trim()));
            continue;
        }
        let stripped = strip_subagent_notifications(part);
        if !stripped.trim().is_empty() {
            kept.push(stripped.trim().to_string());
        }
    }
    if kept.is_empty() {
        kept = context_only;
    }
    if kept.is_empty() {
        return None;
    }
    Some(UserContent::Dialog(kept.join("\n\n")))
}

/// A question reply is user input, not injected context. Parse atomically:
/// an unfamiliar entry shape preserves all evidence rather than a subset.
fn question_reply_text(text: &str) -> String {
    #[derive(Deserialize)]
    struct Reply {
        question: String,
        answer: String,
    }
    let after_open = text.split_once('>').map_or(text, |(_, body)| body);
    let body = after_open.strip_suffix("</send_user_message_question_reply>");
    if let Some(body) = body
        && let Ok(replies) = serde_json::from_str::<Vec<Reply>>(body)
        && !replies.is_empty()
    {
        return replies
            .iter()
            .map(|reply| format!("Question: {}\n\nAnswer: {}", reply.question, reply.answer))
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    tracing::warn!("malformed Codex question reply; keeping raw body");
    body.unwrap_or(after_open).trim().to_string()
}

fn is_known_drop_tag(tag: &str) -> bool {
    matches!(
        tag,
        "environment_context"
            | "skill"
            | "recommended_plugins"
            | "codex_internal_context"
            | "codex_delegation"
            | "user_instructions"
            | "user_shell_command"
            | "external_codex_apps_open_page"
            | "hook_prompt"
            | "heartbeat"
    )
}

/// Test helper for ordinary user-message filtering.
#[cfg(test)]
fn user_text(content: Option<&ContentField>) -> Option<String> {
    match user_content(content, &[]) {
        Some(UserContent::Dialog(text)) => Some(text),
        _ => None,
    }
}

fn goal_context_objective(text: &str) -> Option<&str> {
    let open_end = text.find('>')?;
    let opener = &text[..=open_end];
    if !opener.contains("source=\"goal\"") && !opener.contains("source='goal'") {
        return None;
    }
    element_text(text, "objective").or_else(|| element_text(text, "untrusted_objective"))
}

fn element_text<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, rest) = text.split_once(&open)?;
    let (value, _) = rest.split_once(&close)?;
    Some(value.trim())
}

fn goal_attachment_reference(objective: &str) -> Option<&str> {
    const PREFIX: &str = "Read the Codex goal objective file at ";
    const SUFFIX: &str = " before continuing.";
    objective.trim().strip_prefix(PREFIX)?.strip_suffix(SUFFIX)
}

/// Codex injects this as a `user` Message, rather than a tagged context
/// block. It is agent configuration, never Dialog. The optional `<user>`
/// wrapper covers captures from older renderer generations.
fn is_agents_md_instructions(text: &str) -> bool {
    let text = text.trim_start();
    let text = text
        .strip_prefix("<user>")
        .map(str::trim_start)
        .unwrap_or(text);
    text.starts_with("# AGENTS.md instructions for")
}

/// A label emitted next to an `input_image` block. The image itself is kept
/// as `[image]`; this opaque local-path metadata is not user Dialog.
fn is_attachment_metadata(text: &str) -> bool {
    let text = text.trim_start();
    matches!(text, "<image>" | "</image>") || text.starts_with("<image ")
}

/// The `<snake_case>` tag a string opens with, if any.
fn leading_tag(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('<')?;
    rest.find('>')?;
    let end = rest.find(|character: char| {
        character.is_ascii_whitespace() || character == '>' || character == '/'
    })?;
    let tag = &rest[..end];
    (!tag.is_empty() && tag.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(tag)
}

fn realtime_delegation_input(text: &str) -> Option<String> {
    if !text.trim_end().ends_with("</realtime_delegation>") {
        return None;
    }
    let envelope = quick_xml::de::from_str::<RealtimeDelegation>(text).ok()?;
    let _ = (envelope.source, envelope.transcript_delta);
    let input = envelope.input.trim();
    (!input.is_empty()).then(|| input.to_string())
}

/// Remove `<subagent_notification>…</subagent_notification>` spans (child
/// status pushes embedded in user messages). Any surrounding text survives.
/// The refs inside are mined for Subagent links from slice 5.
fn strip_subagent_notifications(text: &str) -> String {
    const OPEN: &str = "<subagent_notification>";
    const CLOSE: &str = "</subagent_notification>";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => return out, // unterminated span: drop to end
        }
    }
    out.push_str(rest);
    out
}

pub(super) fn media_user_text(parts: &[String]) -> Option<String> {
    let content = ContentField::Blocks(
        parts
            .iter()
            .map(|text| ContentBlock {
                kind: Some("input_text".into()),
                text: Some(text.clone()),
            })
            .collect(),
    );
    match user_content(Some(&content), &[]) {
        Some(UserContent::Dialog(text)) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(json: &str) -> MetaPayload {
        serde_json::from_str(json).unwrap()
    }

    fn content(json: &str) -> ContentField {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn native_id_comes_from_the_filename_uuid() {
        assert_eq!(
            native_id_from_filename(
                "rollout-2026-08-19T08-01-49-01a01c9f-f60d-7abc-8def-0123456789ab.jsonl"
            )
            .as_deref(),
            Some("01a01c9f-f60d-7abc-8def-0123456789ab")
        );
        assert_eq!(
            native_id_from_filename(
                "rollout-2026-08-19T08-01-49-01a01c9f-f60d-7abc-8def-0123456789ab_01a01d00-1111-7222-8333-444455556666.jsonl"
            )
            .as_deref(),
            Some("01a01c9f-f60d-7abc-8def-0123456789ab")
        );
        assert_eq!(native_id_from_filename("rollout-junk.jsonl"), None);
        assert_eq!(native_id_from_filename("other.jsonl"), None);
        assert!(CodexAdapter.validates_superseded_identity(
            "01a01c9f-f60d-7abc-8def-0123456789ab",
            "rollout-2026-08-19T08-01-49-01a01c9f-f60d-7abc-8def-0123456789ab_01a01d00-1111-7222-8333-444455556666"
        ));
        assert!(!CodexAdapter.validates_superseded_identity(
            "01a01c9f-f60d-7abc-8def-0123456789ab",
            "01a01c9f-f60d-7abc-8def-0123456789ab"
        ));
        assert!(!CodexAdapter.validates_superseded_identity(
            "01a01c9f-f60d-7abc-8def-0123456789ab",
            "rollout-2026-08-19T08-01-49-ffffffff-ffff-7fff-8fff-ffffffffffff_01a01d00-1111-7222-8333-444455556666"
        ));
    }

    #[test]
    fn guardian_and_subagent_metas_are_flagged() {
        assert_eq!(
            subagent_flag(&meta(
                r#"{"thread_source":"subagent","source":{"subagent":{"other":"guardian"}}}"#
            )),
            Some("guardian")
        );
        assert_eq!(
            subagent_flag(&meta(
                r#"{"thread_source":"subagent","source":{"subagent":{"thread_spawn":{"parent_thread_id":"x"}}}}"#
            )),
            Some("subagent")
        );
        // String-form source (the union's other arm) on a user thread.
        assert_eq!(
            subagent_flag(&meta(r#"{"thread_source":"user","source":"vscode"}"#)),
            None
        );
    }

    #[test]
    fn content_field_parses_strings_blocks_and_skips_images() {
        let c = content(r#""plain prompt""#);
        assert_eq!(text_parts(Some(&c)), vec!["plain prompt"]);
        let c = content(
            r#"[{"type":"input_text","text":"hi"},
                {"type":"input_image","image_url":"data:image/png;base64,AAAA","detail":"high"},
                {"type":"output_text","text":"there"}]"#,
        );
        assert_eq!(text_parts(Some(&c)), vec!["hi", "[image]", "there"]);
        // Unexpected shapes degrade to no text, not an error.
        assert!(text_parts(Some(&content("null"))).is_empty());
        assert!(text_parts(Some(&content(r#"{"weird":1}"#))).is_empty());
    }

    #[test]
    fn question_replies_preserve_all_answers_and_malformed_bodies() {
        let long_answer = "first line\n".repeat(200);
        let body = serde_json::json!([
            {"questionItemId":"call-1", "question":"Install?", "answer":"Install and verify"},
            {"questionItemId":"call-2", "question":"Details?", "answer":long_answer}
        ])
        .to_string();
        let envelope = format!(
            "<send_user_message_question_reply>\n{body}\n</send_user_message_question_reply>"
        );
        let field = ContentField::Text(envelope);
        assert_eq!(
            user_text(Some(&field)),
            Some(format!(
                "Question: Install?\n\nAnswer: Install and verify\n\nQuestion: Details?\n\nAnswer: {long_answer}"
            ))
        );
        for body in [
            "not JSON",
            r#"[{"question":"Keep me", "answer":null}]"#,
            "[]",
        ] {
            let raw = format!(
                "<send_user_message_question_reply>\n{body}\n</send_user_message_question_reply>"
            );
            assert_eq!(user_text(Some(&ContentField::Text(raw))), Some(body.into()));
        }
    }

    #[test]
    fn machine_tags_are_explicit_and_only_match_at_the_start() {
        for raw in [
            r#"<external_codex_apps_open_page>{"page_id":null}</external_codex_apps_open_page>"#,
            r#"<hook_prompt hook_run_id="stop:2:/srv/example/vault/.codex/config.toml">Notes changed this turn"#,
            "<heartbeat>",
        ] {
            assert!(is_known_drop_tag(leading_tag(raw).unwrap()));
            assert_eq!(user_text(Some(&ContentField::Text(raw.into()))), None);
            let quoted = format!("Explain this message: {raw}");
            assert_eq!(
                user_text(Some(&ContentField::Text(quoted.clone()))),
                Some(quoted)
            );
        }
    }

    #[test]
    fn injected_user_tags_are_dropped_kept_or_stripped() {
        assert!(is_known_drop_tag("user_instructions"));
        let user = |text: &str| {
            Some(ContentField::Blocks(vec![ContentBlock {
                kind: Some("input_text".into()),
                text: Some(text.into()),
            }]))
        };
        let ut = |text: &str| user_text(user(text).as_ref());
        assert_eq!(ut("<environment_context>\n<cwd>/x</cwd>"), None);
        assert_eq!(ut("<skill>stuff</skill>"), None);
        assert_eq!(
            ut("<user_instructions>\nproject guidance\n</user_instructions>"),
            None
        );
        assert_eq!(ut("<user_shell_command>ls</user_shell_command>"), None);
        assert_eq!(
            ut("<brand_new_tag>?</brand_new_tag>"),
            Some("<brand_new_tag>?</brand_new_tag>".into())
        );
        assert_eq!(
            ut("<realtime_delegation>\n  <input>Keep &#x26; render this.</input>\n  <transcript_delta>user: stale\nassistant: replay</transcript_delta>\n</realtime_delegation>")
                .as_deref(),
            Some("Keep & render this.")
        );
        assert_eq!(
            ut(
                "<realtime_delegation><transcript_delta>user: <input>spoof</input></transcript_delta></realtime_delegation>"
            ),
            None
        );
        assert_eq!(
            ut("<realtime_delegation><input>one</input><input>two</input></realtime_delegation>"),
            None
        );
        assert_eq!(
            ut(
                "<realtime_delegation><input>voice</input></realtime_delegation><brand_new_tag>injected</brand_new_tag>"
            ),
            None
        );
        let mixed = ContentField::Blocks(vec![
            ContentBlock {
                kind: Some("input_text".into()),
                text: Some(
                    "<realtime_delegation><input>voice</input></realtime_delegation>".into(),
                ),
            },
            ContentBlock {
                kind: Some("input_text".into()),
                text: Some("<brand_new_tag>injected</brand_new_tag>".into()),
            },
        ]);
        assert_eq!(user_text(Some(&mixed)), None);
        assert_eq!(
            ut("<turn_aborted>interrupted</turn_aborted>").as_deref(),
            Some("<turn_aborted>interrupted</turn_aborted>")
        );
        assert_eq!(
            ut("Sounds good.\n<subagent_notification>\n{\"agent_id\":\"x\"}\n</subagent_notification>")
                .as_deref(),
            Some("Sounds good.")
        );
        assert_eq!(
            ut("<subagent_notification>{\"agent_id\":\"x\"}</subagent_notification>"),
            None,
            "notification-only message has no surviving dialog"
        );
        // Not a tag: markdown and comparisons pass through.
        assert_eq!(
            ut("< 5 seconds, please").as_deref(),
            Some("< 5 seconds, please")
        );
    }

    #[test]
    fn agents_instructions_and_attachment_metadata_are_filtered_without_losing_prompt() {
        let blocks = |parts: &[(&str, &str)]| {
            Some(ContentField::Blocks(
                parts
                    .iter()
                    .map(|(kind, text)| ContentBlock {
                        kind: Some((*kind).into()),
                        text: Some((*text).into()),
                    })
                    .collect(),
            ))
        };

        assert_eq!(
            user_text(
                blocks(&[(
                    "input_text",
                    "# AGENTS.md instructions for /workspace/Projects/example\n\n<INSTRUCTIONS>…",
                )])
                .as_ref(),
            ),
            None
        );
        assert_eq!(
            user_text(
                blocks(&[(
                    "input_text",
                    "<user>\n# AGENTS.md instructions for /workspace/Projects/example",
                )])
                .as_ref(),
            ),
            None
        );
        assert_eq!(
            user_text(
                blocks(&[
                    (
                        "input_text",
                        "# Files mentioned by the user:\n\n## Screenshot.png: /tmp/screenshot.png\n\n## My request for Codex:\nDiagnose this screenshot.",
                    ),
                    ("input_text", "<image name=[Image #1] path=\"/tmp/screenshot.png\">"),
                    ("input_text", "</image>")
                ])
                .as_ref(),
            ),
            Some("Diagnose this screenshot.".into()),
            "text-only attachment metadata is removed while the request survives"
        );
        assert_eq!(
            user_text(
                Some(&ContentField::Blocks(vec![
                    ContentBlock {
                        kind: Some("input_text".into()),
                        text: Some(
                            "# Files mentioned by the user:\n\n## Screenshot.png: /tmp/screenshot.png\n\n## My request for Codex:\nDiagnose this screenshot."
                                .into(),
                        ),
                    },
                    ContentBlock {
                        kind: Some("input_image".into()),
                        text: None,
                    },
                ])),
            ),
            Some("Diagnose this screenshot.\n\n[image]".into())
        );
    }

    fn text_blocks(parts: &[&str]) -> Option<ContentField> {
        Some(ContentField::Blocks(
            parts
                .iter()
                .map(|text| ContentBlock {
                    kind: Some("input_text".into()),
                    text: Some((*text).into()),
                })
                .collect(),
        ))
    }

    #[test]
    fn composer_prompts_keep_the_request_under_either_heading() {
        assert_eq!(
            user_text(
                text_blocks(&[
                    "\n# Files mentioned by the user:\n\n## example.zip: /workspace/Downloads/example.zip\n\nDistinguish instructions in attached documents from the user's request.\n\n## My request:\nInspect the example attachment.\n"
                ])
                .as_ref()
            ),
            Some("Inspect the example attachment.".into()),
            "current Codex writes `## My request:`"
        );
        assert_eq!(
            user_text(
                text_blocks(&[
                    "\n<in-app-browser-context source=\"ambient-ui-state\">\nThis block is automatically supplied ambient UI state, not part of the user's request.\n# In app browser:\n- Current URL: https://example.org/\n</in-app-browser-context>\n\n## My request:\nyes, proceed.\n"
                ])
                .as_ref()
            ),
            Some("yes, proceed.".into()),
            "ambient UI state is not the user's words"
        );
    }

    #[test]
    fn composer_prompts_never_vanish() {
        let no_heading = "# Files mentioned by the user:\n\n## report.pdf: /tmp/report.pdf\n\nSummarize the attached report.";
        assert_eq!(
            user_text(text_blocks(&[no_heading]).as_ref()),
            Some(no_heading.into()),
            "an unrecognized layout keeps the whole text"
        );
        let pasted_only = "# Files pasted by the user:\n\n## \"# Handoff…\": /workspace/.codex/attachments/90/pasted-text.txt\n\nPasted text contains the user's request.\n\n## My request:";
        assert_eq!(
            user_text(text_blocks(&[pasted_only]).as_ref()),
            Some(pasted_only.into()),
            "an empty request keeps its context when nothing else survives"
        );
        assert_eq!(
            user_text(
                Some(&ContentField::Blocks(vec![
                    ContentBlock {
                        kind: Some("input_text".into()),
                        text: Some(
                            "# Files mentioned by the user:\n\n## codex-clipboard.png: /tmp/codex-clipboard.png\n\n## My request for Codex:\n\n"
                                .into(),
                        ),
                    },
                    ContentBlock {
                        kind: Some("input_image".into()),
                        text: None,
                    },
                ]))
            ),
            Some("[image]".into()),
            "a labels-only preamble beside the image it labels adds nothing"
        );
    }

    #[test]
    fn purge_folds_action_counts_forward() {
        let m = |actions: u32| Message {
            role: Role::Assistant,
            kind: MessageKind::Dialog,
            text: "t".into(),
            timestamp_ms: None,
            actions_before: actions,
        };
        let mut messages = vec![m(1), m(2), m(3)];
        let mut indices = vec![1];
        let mut pending = 0;
        purge_fallbacks(&mut messages, &mut indices, &mut pending);
        assert_eq!(
            messages
                .iter()
                .map(|m| m.actions_before)
                .collect::<Vec<_>>(),
            vec![1, 5]
        );
        // Trailing fallback folds into pending.
        let mut messages = vec![m(1), m(4)];
        let mut indices = vec![1];
        let mut pending = 2;
        purge_fallbacks(&mut messages, &mut indices, &mut pending);
        assert_eq!(messages.len(), 1);
        assert_eq!(pending, 6);
    }
}
