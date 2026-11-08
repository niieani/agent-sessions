//! Adapter for the Pi Source (`~/.pi/agent/sessions`).
//!
//! Format reference: `docs/formats/pi.md` (Pi is open source — the upstream
//! session writer is the authority). The simplest Source — proves the
//! Adapter trait fits a third Source cheaply.
//!
//! Extraction policy (SPEC "Source-specific extraction policy"): since
//! session v2 entries form a tree via `id`/`parentId`; the Conversation is
//! the root→leaf path ending at the file's last entry — what Pi itself
//! would send to the model. Abandoned branches stay in the Capture.
//! Thinking blocks and tool calls are Actions: omitted, counted for seams.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::jsonl::open_capture;
use super::{
    Adapter, Capture, EnumerateGate, SessionKind, SessionRef, mtime_ms, write_file_capture,
};
use crate::model::{Conversation, Message, MessageKind, Role, TranscriptFacts};
use crate::timefmt::parse_iso_ms;

pub const SOURCE: &str = "pi";
const ADAPTER_VERSION: u32 = 1;

/// Default Store root on a Host: `$PI_CODING_AGENT_DIR/sessions` when set,
/// else `~/.pi/agent/sessions`.
pub fn default_store_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
        return PathBuf::from(dir).join("sessions");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".pi").join("agent").join("sessions")
}

pub struct PiAdapter;

impl Adapter for PiAdapter {
    fn source(&self) -> &'static str {
        SOURCE
    }

    /// Walk `<root>/--<encoded-cwd>--/<ts>_<id>.jsonl`, tolerating stray
    /// `.jsonl` at other depths. No date partitioning exists; the store is
    /// small, so stat-everything plus the per-file gate is the whole story.
    fn enumerate(&self, store_root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
        let store_root = &std::fs::canonicalize(store_root).unwrap_or_else(|_| store_root.into());
        let mut refs = Vec::new();
        walk_sessions(store_root, None, gate, &mut refs)?;
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

    /// Stream the Capture, then extract the leaf-path Conversation.
    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>> {
        let mut reader = open_capture(capture)?;

        let mut header_version: i64 = 1; // absent = v1 (no entry ids)
        let mut cwd: Option<String> = None;
        let mut created_at_ms: Option<i64> = None;
        let mut updated_at_ms: Option<i64> = None;
        let mut title: Option<String> = None;
        let mut saw_header = false;
        let mut entries: Vec<Entry> = Vec::new();
        let mut transcript_facts = TranscriptFacts::default();

        while let Some(record) = reader.next_record()? {
            let entry_ts = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| parse_iso_ms(s).ok());
            if let Some(ts) = entry_ts {
                created_at_ms.get_or_insert(ts);
                updated_at_ms = Some(updated_at_ms.map_or(ts, |u: i64| u.max(ts)));
            }
            match record.get("type").and_then(Value::as_str) {
                Some("model_change") => {
                    if let Some(model) = record
                        .get("model")
                        .or_else(|| record.get("newModel"))
                        .and_then(Value::as_str)
                        .filter(|model| !model.trim().is_empty())
                    {
                        transcript_facts
                            .starting_model
                            .get_or_insert_with(|| model.to_string());
                    }
                }
                Some("thinking_level_change") => {
                    if let Some(effort) = record
                        .get("thinkingLevel")
                        .or_else(|| record.get("level"))
                        .and_then(Value::as_str)
                        .filter(|effort| !effort.trim().is_empty())
                    {
                        transcript_facts
                            .starting_reasoning_effort
                            .get_or_insert_with(|| effort.to_string());
                    }
                }
                Some("message") => {
                    if let Some(model) = record
                        .pointer("/message/model")
                        .and_then(Value::as_str)
                        .filter(|model| !model.trim().is_empty())
                    {
                        transcript_facts
                            .starting_model
                            .get_or_insert_with(|| model.to_string());
                    }
                }
                _ => {}
            }
            match record.get("type").and_then(Value::as_str) {
                Some("session") if !saw_header => {
                    saw_header = true;
                    header_version = record.get("version").and_then(Value::as_i64).unwrap_or(1);
                    if let Some(id) = record.get("id").and_then(Value::as_str)
                        && id != capture.meta.native_id
                    {
                        tracing::warn!(
                            capture = %capture.body_path.display(),
                            header_id = id,
                            "session header id differs from filename identity"
                        );
                    }
                    cwd = record
                        .get("cwd")
                        .and_then(Value::as_str)
                        .filter(|c| !c.is_empty())
                        .map(str::to_string);
                }
                Some("message") => {
                    let node = message_node(&record, entry_ts);
                    entries.push(Entry::of(&record, node));
                }
                Some("session_info") => {
                    // Latest wins, INCLUDING clears (empty/absent name).
                    title = record
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.trim().is_empty())
                        .map(str::to_string);
                    entries.push(Entry::of(&record, Node::Noise));
                }
                // model_change, thinking_level_change, compaction,
                // branch_summary, custom, custom_message, label, unknown
                // future types: kept in the Capture, invisible here — but
                // they still occupy tree positions.
                _ => entries.push(Entry::of(&record, Node::Noise)),
            }
        }

        // The Conversation is the root→leaf path ending at the LAST entry
        // (what Pi would send to the model). Pre-v2 files have no entry ids:
        // file order is the only structure.
        let path: Vec<usize> =
            if header_version >= 2 && entries.last().is_some_and(|e| e.id.is_some()) {
                leaf_path(&entries)
            } else {
                (0..entries.len()).collect()
            };

        let mut messages: Vec<Message> = Vec::new();
        let mut pending_actions: u32 = 0;
        for idx in path {
            match std::mem::replace(&mut entries[idx].node, Node::Noise) {
                Node::Dialog {
                    role,
                    text,
                    timestamp_ms,
                    actions_within,
                } => {
                    if !text.trim().is_empty() {
                        messages.push(Message {
                            role,
                            kind: MessageKind::Dialog,
                            text,
                            timestamp_ms,
                            actions_before: std::mem::take(&mut pending_actions),
                        });
                    }
                    pending_actions += actions_within;
                }
                Node::Actions(n) => pending_actions += n,
                Node::Noise => {}
            }
        }

        // Message-level unix-ms timestamps can postdate the last entry's.
        if let Some(max_msg_ts) = messages.iter().filter_map(|m| m.timestamp_ms).max() {
            updated_at_ms = Some(updated_at_ms.map_or(max_msg_ts, |u| u.max(max_msg_ts)));
        }

        transcript_facts.normalize_file_paths(cwd.as_deref());
        Ok(Some(Conversation {
            provenance: capture.provenance(),
            title,
            project_path: cwd,
            native_project: None,
            created_at_ms,
            updated_at_ms,
            messages,
            transcript_facts,
            subagent_parent: None,
            inherited_context: None,
            subagent_references: Vec::new(),
            branches: Vec::new(),
            subagent_links: Vec::new(),
            trailing_actions: pending_actions,
        }))
    }
}

/// One post-header entry: its tree position plus what it contributes.
struct Entry {
    id: Option<String>,
    parent_id: Option<String>,
    node: Node,
}

impl Entry {
    fn of(record: &Value, node: Node) -> Self {
        Self {
            id: record.get("id").and_then(Value::as_str).map(str::to_string),
            parent_id: record
                .get("parentId")
                .and_then(Value::as_str)
                .map(str::to_string),
            node,
        }
    }
}

enum Node {
    Dialog {
        role: Role,
        text: String,
        timestamp_ms: Option<i64>,
        /// Thinking blocks + tool calls inside this message — Actions,
        /// omitted and counted for seams.
        actions_within: u32,
    },
    /// An entry that is purely activity (user-run `!` shell command).
    Actions(u32),
    Noise,
}

/// Classify a `message` entry per docs/formats/pi.md §4–5.
fn message_node(record: &Value, entry_ts: Option<i64>) -> Node {
    let Some(message) = record.get("message") else {
        return Node::Noise;
    };
    // Prefer the message's own unix-ms timestamp over the entry's.
    let ts = message
        .get("timestamp")
        .and_then(Value::as_i64)
        .or(entry_ts);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => Node::Dialog {
            role: Role::User,
            text: content_text(message.get("content")),
            timestamp_ms: ts,
            actions_within: 0,
        },
        Some("assistant") => {
            let mut actions: u32 = 0;
            let mut parts: Vec<String> = Vec::new();
            match message.get("content") {
                Some(Value::String(s)) => parts.push(s.clone()),
                Some(Value::Array(blocks)) => {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = block.get("text").and_then(Value::as_str)
                                    && !t.is_empty()
                                {
                                    parts.push(t.to_string());
                                }
                            }
                            Some("thinking") | Some("toolCall") => actions += 1,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            Node::Dialog {
                role: Role::Assistant,
                text: parts.join("\n\n"),
                timestamp_ms: ts,
                actions_within: actions,
            }
        }
        // A user-run `!` shell command is activity, not typed prose.
        Some("bashExecution") => Node::Actions(1),
        // toolResult, custom (hookMessage pre-v3), branchSummary,
        // compactionSummary: never Dialog.
        _ => Node::Noise,
    }
}

/// User content: a raw string, or `text` blocks joined; images become a
/// placeholder, never inline base64.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => block
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string),
                    Some("image") => Some("[image]".to_string()),
                    _ => None,
                })
                .collect();
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

/// Walk `parentId` from the last entry to the root and reverse. Orphaned
/// parents end the walk (the entry acts as a root, per upstream); a visited
/// set guards against cycles in hand-edited files.
fn leaf_path(entries: &[Entry]) -> Vec<usize> {
    let index: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.id.as_deref().map(|id| (id, i)))
        .collect();
    let mut path = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut cursor = entries.len().checked_sub(1);
    while let Some(i) = cursor {
        if !visited.insert(i) {
            tracing::warn!("cycle in pi entry tree; truncating path");
            break;
        }
        path.push(i);
        cursor = entries[i]
            .parent_id
            .as_deref()
            .and_then(|p| index.get(p).copied());
    }
    path.reverse();
    path
}

/// Recursive `.jsonl` walk. The native id is the filename after the first
/// `_` (the encoded timestamp contains none; custom ids may) — confirmed
/// against the header at Extract time. Fallback: the whole stem.
fn walk_sessions(
    dir: &Path,
    project_dir: Option<&str>,
    gate: &EnumerateGate,
    refs: &mut Vec<SessionRef>,
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
            let name = entry.file_name().to_string_lossy().into_owned();
            walk_sessions(&path, Some(&name), gate, refs)?;
        } else if ft.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let native_id = stem
                .split_once('_')
                .map(|(_, id)| id.to_string())
                .unwrap_or_else(|| stem.into_owned());
            let meta = entry.metadata()?;
            let mtime = mtime_ms(&meta);
            if gate.skips(&native_id, mtime, meta.len(), None, None)
                && gate.birth_time_matches(&native_id, &path, &meta)?
            {
                continue;
            }
            refs.push(SessionRef {
                native_id,
                path,
                additional_files: Vec::new(),
                project_dir: project_dir.map(str::to_string),
                kind: SessionKind::Main,
                mtime_ms: mtime,
                size: meta.len(),
                native_meta_path: None,
                native_meta_mtime_ms: None,
                native_meta_size: None,
                native_meta: None,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, parent: Option<&str>) -> Entry {
        Entry {
            id: Some(id.to_string()),
            parent_id: parent.map(str::to_string),
            node: Node::Noise,
        }
    }

    #[test]
    fn leaf_path_excludes_abandoned_branches() {
        // e1 ← e2 ← e3 (abandoned) ; e2 ← e4 ← e5(leaf)
        let entries = vec![
            entry("e1", None),
            entry("e2", Some("e1")),
            entry("e3", Some("e2")),
            entry("e4", Some("e2")),
            entry("e5", Some("e4")),
        ];
        assert_eq!(leaf_path(&entries), vec![0, 1, 3, 4]);
    }

    #[test]
    fn orphaned_parent_acts_as_root_and_cycles_terminate() {
        let entries = vec![entry("a", Some("missing")), entry("b", Some("a"))];
        assert_eq!(leaf_path(&entries), vec![0, 1]);
        let cycle = vec![entry("a", Some("b")), entry("b", Some("a"))];
        assert_eq!(leaf_path(&cycle), vec![0, 1]);
    }
}
