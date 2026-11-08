//! Adapter for the Claude Code Source (`~/.claude/projects`).
//!
//! Format reference: `docs/formats/claude.md` (the definitive census of the
//! Store layout, record types, noise filters, and quirks). The Store is
//! plain append-only JSONL files, strictly read-only to us.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::jsonl::{JsonlReader, open_capture};
use super::{
    Adapter, Capture, EnumerateGate, SessionKind, SessionRef, mtime_ms, write_file_capture,
};
use crate::model::{
    Conversation, GitSnapshot, Message, MessageKind, Role, SubagentParent, SubagentReference,
    TranscriptFacts,
};
use crate::timefmt::parse_iso_ms;

pub const SOURCE: &str = "claude";
const ADAPTER_VERSION: u32 = 3;

/// Default Store location on a Host.
pub fn default_store_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".claude").join("projects")
}

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
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

    fn store_retention(&self, store_root: &Path) -> Result<Option<crate::pull::StoreRetention>> {
        crate::pull::retention::claude_retention(store_root).map(Some)
    }

    fn source(&self) -> &'static str {
        SOURCE
    }

    /// Walk `<store>/*/` for `<uuid>.jsonl` main Sessions plus
    /// `agent-*.jsonl` anywhere under a `subagents/` segment (which covers
    /// the `workflows/wf_*/` layer). Skip only what the `gate` proves is
    /// already captured unchanged.
    fn enumerate(&self, store_root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
        let mut refs = Vec::new();
        // Provenance records absolute native paths; tolerate a relative root.
        let store_root = &std::fs::canonicalize(store_root).unwrap_or_else(|_| store_root.into());
        let projects = match std::fs::read_dir(store_root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(refs),
            Err(e) => {
                return Err(e).with_context(|| format!("reading store {}", store_root.display()));
            }
        };
        for project in projects {
            let project = project?;
            if !project.file_type()?.is_dir() {
                continue;
            }
            let project_dir = project.file_name().to_string_lossy().into_owned();
            for entry in std::fs::read_dir(project.path())? {
                let entry = entry?;
                let path = entry.path();
                let ft = entry.file_type()?;
                if ft.is_file() {
                    if path.extension().is_some_and(|e| e == "jsonl") {
                        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                        push_ref(
                            &mut refs,
                            &path,
                            stem.into_owned(),
                            &project_dir,
                            SessionKind::Main,
                            None,
                            gate,
                        )?;
                    }
                } else if ft.is_dir() {
                    // Per-session side directory: find subagent Sessions.
                    walk_subagents(&path, &project.path(), &project_dir, gate, &mut refs)?;
                }
            }
        }
        refs.sort_by(|a, b| a.native_id.cmp(&b.native_id));
        Ok(refs)
    }

    fn session_working_path(&self, session: &SessionRef) -> Result<Option<String>> {
        let file = std::fs::File::open(&session.path)?;
        let mut reader = super::jsonl::JsonlReader::new(
            std::io::BufReader::new(file),
            session.path.display().to_string(),
        );
        while let Some(record) = reader.next_record()? {
            if let Some(cwd) = record.get("cwd").and_then(serde_json::Value::as_str) {
                return Ok(Some(cwd.to_owned()));
            }
        }
        Ok(None)
    }

    fn capture_working_path(&self, capture: &Capture) -> Result<Option<String>> {
        let mut reader = open_capture(capture)?;
        while let Some(record) = reader.next_record()? {
            if let Some(cwd) = record.get("cwd").and_then(serde_json::Value::as_str) {
                return Ok(Some(cwd.to_owned()));
            }
        }
        Ok(None)
    }

    fn capture(&self, session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
        write_file_capture(
            SOURCE,
            session,
            captures_dir,
            host,
            "jsonl",
            ADAPTER_VERSION,
        )
    }

    /// Extract tiers 1–3 from a Capture, streaming line by line. Subagent
    /// Captures are Conversations in their own right and carry a child-side
    /// parent fact used by the global resolver.
    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>> {
        let reader = open_capture(capture)?;
        let conv = extract_conversation(reader, capture)?;
        Ok(Some(conv))
    }
}

fn push_ref(
    refs: &mut Vec<SessionRef>,
    path: &Path,
    native_id: String,
    project_dir: &str,
    kind: SessionKind,
    native_meta_path: Option<PathBuf>,
    gate: &EnumerateGate,
) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    let mtime = mtime_ms(&meta);
    let native_meta_revision = native_meta_path
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| (mtime_ms(&meta), meta.len()));
    if gate.skips(
        &native_id,
        mtime,
        meta.len(),
        native_meta_revision.map(|r| r.0),
        native_meta_revision.map(|r| r.1),
    ) && gate.birth_time_matches(&native_id, path, &meta)?
    {
        return Ok(());
    }
    refs.push(SessionRef {
        native_id,
        path: path.to_path_buf(),
        additional_files: Vec::new(),
        project_dir: Some(project_dir.to_string()),
        kind,
        mtime_ms: mtime,
        size: meta.len(),
        native_meta_path,
        native_meta_mtime_ms: native_meta_revision.map(|r| r.0),
        native_meta_size: native_meta_revision.map(|r| r.1),
        native_meta: None,
    });
    Ok(())
}

/// Recursively find `agent-*.jsonl` files that sit under a `subagents/` path
/// segment. The native id is the project-dir-relative path minus `.jsonl`,
/// preserving the parent chain and workflow layer.
fn walk_subagents(
    dir: &Path,
    project_root: &Path,
    project_dir: &str,
    gate: &EnumerateGate,
    refs: &mut Vec<SessionRef>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_subagents(&path, project_root, project_dir, gate, refs)?;
        } else if ft.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("agent-")
                && name.ends_with(".jsonl")
                && path
                    .strip_prefix(project_root)
                    .is_ok_and(|rel| rel.iter().any(|seg| seg == "subagents"))
            {
                let rel = path.strip_prefix(project_root).expect("checked above");
                let native_id = rel.with_extension("").to_string_lossy().replace('\\', "/");
                let meta_path = path.with_extension("meta.json");
                let native_meta_path = meta_path.exists().then_some(meta_path);
                push_ref(
                    refs,
                    &path,
                    native_id,
                    project_dir,
                    SessionKind::Subagent,
                    native_meta_path,
                    gate,
                )?;
            }
        }
    }
    Ok(())
}

/// Streaming extraction over JSONL records. Only `user`/`assistant` records
/// yield Messages (after noise filtering per docs/formats/claude.md);
/// `tool_use` blocks are counted as omitted Actions for seam markers.
fn extract_conversation<R: std::io::BufRead>(
    mut reader: JsonlReader<R>,
    capture: &Capture,
) -> Result<Conversation> {
    let mut messages: Vec<Message> = Vec::new();
    let mut pending_actions: u32 = 0;
    let mut ai_title: Option<String> = None;
    let mut custom_title: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut created_at_ms: Option<i64> = None;
    let mut updated_at_ms: Option<i64> = None;
    let mut tool_spawns: HashMap<String, SpawnSpec> = HashMap::new();
    let mut subagent_references = Vec::new();
    let mut transcript_facts = TranscriptFacts::default();

    while let Some(record) = reader.next_record()? {
        if cwd.is_none()
            && let Some(c) = record.get("cwd").and_then(Value::as_str)
        {
            cwd = Some(c.to_string());
        }
        let ts = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| parse_iso_ms(s).ok());
        if let Some(ts) = ts {
            created_at_ms.get_or_insert(ts);
            updated_at_ms = Some(updated_at_ms.map_or(ts, |u: i64| u.max(ts)));
        }
        let git = transcript_facts
            .git
            .get_or_insert_with(GitSnapshot::default);
        if git.branch.is_none() {
            git.branch = nonempty_field(&record, "gitBranch");
        }
        if git.commit.is_none() {
            git.commit = nonempty_field(&record, "gitCommit");
        }
        if git.repository_url.is_none() {
            git.repository_url = nonempty_field(&record, "gitRepositoryUrl");
        }
        if git.is_empty() {
            transcript_facts.git = None;
        }

        match record.get("type").and_then(Value::as_str) {
            // Control records: last occurrence wins for title-like fields.
            Some("ai-title") => {
                if let Some(t) = record.get("aiTitle").and_then(Value::as_str) {
                    ai_title = Some(t.to_string());
                }
            }
            Some("custom-title") => {
                if let Some(t) = record.get("customTitle").and_then(Value::as_str) {
                    custom_title = Some(t.to_string());
                }
            }
            Some("user") => {
                if let Some(reference) = subagent_reference(&record, &tool_spawns, ts) {
                    subagent_references.push(reference);
                }
                if is_noise_user(&record) {
                    continue;
                }
                let text = message_text(&record);
                if text.trim().is_empty() {
                    // A tool_result-only user record is Action plumbing, not
                    // Dialog; the Action was counted at its tool_use site.
                    continue;
                }
                messages.push(Message {
                    role: Role::User,
                    kind: MessageKind::Dialog,
                    text,
                    timestamp_ms: ts,
                    actions_before: std::mem::take(&mut pending_actions),
                });
            }
            Some("assistant") => {
                if let Some(model) = record
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .filter(|model| !model.is_empty() && *model != "<synthetic>")
                {
                    transcript_facts
                        .starting_model
                        .get_or_insert_with(|| model.to_string());
                }
                if let Some(effort) = record
                    .pointer("/message/effort")
                    .or_else(|| record.pointer("/message/output_config/effort"))
                    .and_then(Value::as_str)
                    .filter(|effort| !effort.is_empty())
                {
                    transcript_facts
                        .starting_reasoning_effort
                        .get_or_insert_with(|| effort.to_string());
                }
                let tool_uses = count_tool_uses(&record);
                collect_spawn_specs(&record, ts, &mut tool_spawns);
                if is_noise_assistant(&record) {
                    pending_actions += tool_uses;
                    continue;
                }
                let text = message_text(&record);
                if !text.trim().is_empty() {
                    messages.push(Message {
                        role: Role::Assistant,
                        kind: MessageKind::Dialog,
                        text,
                        timestamp_ms: ts,
                        actions_before: std::mem::take(&mut pending_actions),
                    });
                }
                pending_actions += tool_uses;
            }
            Some("attachment") => {
                let Some(text) = queued_prompt_text(&record) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                messages.push(Message {
                    role: Role::User,
                    kind: MessageKind::Dialog,
                    text,
                    timestamp_ms: ts,
                    actions_before: std::mem::take(&mut pending_actions),
                });
            }
            // Everything else (system, mode, …) is absorbed
            // silently — new native record types must cost zero code.
            _ => {}
        }
    }

    let title = custom_title.or(ai_title);

    let subagent_parent = (capture.meta.kind == SessionKind::Subagent)
        .then(|| child_parent_fact(capture, &messages))
        .transpose()?;
    transcript_facts.normalize_file_paths(cwd.as_deref());

    Ok(Conversation {
        provenance: capture.provenance(),
        title,
        project_path: cwd,
        native_project: None,
        created_at_ms,
        updated_at_ms,
        messages,
        transcript_facts,
        subagent_parent,
        inherited_context: None,
        subagent_references,
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: pending_actions,
    })
}

fn nonempty_field(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone)]
struct SpawnSpec {
    description: Option<String>,
    agent_type: Option<String>,
    instructions: Option<String>,
    spawned_at_ms: Option<i64>,
}

fn collect_spawn_specs(record: &Value, ts: Option<i64>, out: &mut HashMap<String, SpawnSpec>) {
    let Some(blocks) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = block.get("name").and_then(Value::as_str);
        if !matches!(name, Some("Task" | "Agent" | "Workflow")) {
            continue;
        }
        let input = block.get("input");
        out.insert(
            id.to_string(),
            SpawnSpec {
                description: input
                    .and_then(|v| v.get("description"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                agent_type: input
                    .and_then(|v| v.get("subagent_type"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                instructions: input
                    .and_then(|v| v.get("prompt"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                spawned_at_ms: ts,
            },
        );
    }
}

fn subagent_reference(
    record: &Value,
    spawns: &HashMap<String, SpawnSpec>,
    ts: Option<i64>,
) -> Option<SubagentReference> {
    let result = record.get("toolUseResult")?;
    let agent_id = result.get("agentId").and_then(Value::as_str)?;
    let tool_id = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks.iter().find_map(|b| {
                (b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .then(|| b.get("tool_use_id").and_then(Value::as_str))
                    .flatten()
            })
        });
    let spawn = tool_id.and_then(|id| spawns.get(id));
    Some(SubagentReference {
        child_native_id: None,
        child_native_key: Some(agent_id.to_string()),
        link_key: tool_id.map(str::to_string),
        description: spawn.and_then(|s| s.description.clone()).or_else(|| {
            result
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        agent_type: spawn.and_then(|s| s.agent_type.clone()),
        instructions: spawn.and_then(|s| s.instructions.clone()).or_else(|| {
            result
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        spawned_at_ms: spawn.and_then(|s| s.spawned_at_ms).or(ts),
    })
}

fn child_parent_fact(capture: &Capture, messages: &[Message]) -> Result<SubagentParent> {
    let (parent_native_id, agent_id) = subagent_path_parts(&capture.meta.native_id)?;
    let meta = capture.meta.native_meta.as_ref();
    Ok(SubagentParent {
        parent_native_id,
        description: meta
            .and_then(|v| v.get("description"))
            .and_then(Value::as_str)
            .map(str::to_string),
        agent_type: meta
            .and_then(|v| v.get("agentType"))
            .and_then(Value::as_str)
            .map(str::to_string),
        instructions: messages
            .iter()
            .find(|m| m.role == Role::User && !m.text.trim().is_empty())
            .map(|m| m.text.clone()),
        depth: meta
            .and_then(|v| v.get("spawnDepth"))
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .or_else(|| Some(capture.meta.native_id.matches("/subagents/").count() as u32)),
        native_key: Some(agent_id),
        link_key: meta
            .and_then(|v| v.get("toolUseId"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn subagent_path_parts(native_id: &str) -> Result<(String, String)> {
    let (parent, leaf) = native_id
        .rsplit_once("/subagents/")
        .ok_or_else(|| anyhow::anyhow!("subagent native id has no parent: {native_id}"))?;
    let agent_id = leaf
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_prefix("agent-"))
        .ok_or_else(|| anyhow::anyhow!("subagent native id has no agent leaf: {native_id}"))?;
    Ok((parent.to_string(), agent_id.to_string()))
}

/// User records that are not Dialog (docs/formats/claude.md, noise filters).
pub(super) fn is_noise_user(record: &Value) -> bool {
    if record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    let text = message_text(record);
    let t = text.trim_start();
    t.starts_with("<command-name>")
        || t.starts_with("<local-command-stdout>")
        || t.starts_with("<local-command-stderr>")
        || t.starts_with("<task-notification>")
        || t.starts_with("<teammate-message>")
        || t.starts_with("[Request interrupted by user")
}

/// Assistant records that are CLI-fabricated, not Dialog.
pub(super) fn is_noise_assistant(record: &Value) -> bool {
    if record.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    record
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        == Some("<synthetic>")
}

/// Flatten `message.content` (string or block array) to text. Text blocks
/// concatenate with a blank line; thinking/tool blocks are dropped; images
/// become a placeholder (never inline base64).
fn message_text(record: &Value) -> String {
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return String::new();
    };
    content_text(content)
}

/// A queued SDK prompt is a native user submission even though Claude stores
/// it as an attachment rather than an ordinary user record.
fn queued_prompt_text(record: &Value) -> Option<String> {
    let attachment = record.get("attachment")?;
    if attachment.get("type").and_then(Value::as_str) != Some("queued_command")
        || attachment.get("commandMode").and_then(Value::as_str) != Some("prompt")
    {
        return None;
    }
    Some(content_text(attachment.get("prompt")?))
}

/// Flatten native string or text-block-array content using the normal Claude
/// user-text rules.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            parts.push(t.to_string());
                        }
                    }
                    Some("image") => parts.push("[image]".to_string()),
                    _ => {} // thinking, tool_use, tool_result — not dialog text
                }
            }
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

fn count_blocks(record: &Value, block_type: &str) -> u32 {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some(block_type))
                .count() as u32
        })
        .unwrap_or(0)
}

fn count_tool_uses(record: &Value) -> u32 {
    count_blocks(record, "tool_use")
}

#[cfg(test)]
mod tests {
    use super::subagent_path_parts;

    #[test]
    fn subagent_paths_find_immediate_parent_without_constructing_children() {
        assert_eq!(
            subagent_path_parts("main/subagents/workflows/wf_1/agent-a1").unwrap(),
            ("main".into(), "a1".into())
        );
        assert_eq!(
            subagent_path_parts("main/subagents/agent-a1/subagents/agent-a2").unwrap(),
            ("main/subagents/agent-a1".into(), "a2".into())
        );
    }
}
