//! Cursor Adapter (`state.vscdb`).
//!
//! Cursor owns a live WAL-mode SQLite Store. We open it read-only and Capture
//! one composer's raw KV rows as NDJSON: composerData, optional
//! composerHeaders, then every bubble row. Extraction only reads that Capture.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::sqlite::{
    FNV_OFFSET_BASIS, capture_value, hash_bytes, hash_value, open_read_only, quote_identifier,
    sqlite_i64, table_columns, value_bytes, value_type,
};
use super::{
    Adapter, Capture, CaptureMeta, CaptureOutcome, EnumerateGate, RevisionCheck, SessionKind,
    SessionRef, now_ms, revision_check, tmp_sibling, write_retry_sentinel,
};
use crate::model::{
    Conversation, Message, MessageKind, Role, SubagentParent, SubagentReference, TranscriptFacts,
};
use crate::timefmt::parse_iso_ms;

pub const SOURCE: &str = "cursor";
const ADAPTER_VERSION: u32 = 1;
const SQL_ROOT_REVISION: &str = "CASE WHEN json_valid(value) THEN CASE
    WHEN json_type(value, '$.lastUpdatedAt') IN ('integer','real')
      THEN CAST(json_extract(value, '$.lastUpdatedAt') AS INTEGER)
    WHEN json_type(value, '$.lastUpdatedAt') IS NULL
      AND json_type(value, '$.createdAt') IN ('integer','real')
      THEN CAST(json_extract(value, '$.createdAt') AS INTEGER)
    ELSE NULL END END";

/// Cursor's platform-default Store. Flatpak and Snap installs should use
/// `--cursor-root` / `cursor-root` because their config homes are sandboxed.
pub fn default_store_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        config
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    }
}

pub struct CursorAdapter;

impl Adapter for CursorAdapter {
    fn capture_stem(&self, id: &str) -> String {
        crate::adapters::sqlite::capture_stem(id)
    }
    fn capture_extension(&self) -> &'static str {
        "ndjson"
    }

    fn source(&self) -> &'static str {
        SOURCE
    }

    fn enumerate(&self, store_root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
        if !store_root.exists() {
            return Ok(Vec::new());
        }
        let path = std::fs::canonicalize(store_root).unwrap_or_else(|_| store_root.to_path_buf());
        let conn = open_read_only(&path, "Cursor")?;
        let mut refs = Vec::new();

        if gate.trusts_bookmark() && gate.since().is_some() {
            // A stateless remote cannot compare header signatures with the
            // Archive. Conservatively re-cover every header-bearing composer;
            // root-only composers retain SQL-side logical date filtering.
            let header_columns = table_columns(&conn, "composerHeaders")?;
            let header_predicate = if header_columns.iter().any(|name| name == "composerId") {
                "EXISTS (SELECT 1 FROM composerHeaders h
                         WHERE h.composerId = substr(cursorDiskKV.key, 14))"
            } else {
                "0"
            };
            let sql = format!(
                "SELECT substr(key, 14), value, {header_predicate} FROM cursorDiskKV
                 WHERE key >= 'composerData:' AND key < 'composerData;'
                   AND value IS NOT NULL
                   AND ({header_predicate} OR ({SQL_ROOT_REVISION}) > ?1
                        OR ({SQL_ROOT_REVISION}) IS NULL)
                 ORDER BY key"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query([gate.since().unwrap_or_default()])?;
            while let Some(row) = rows.next()? {
                let native_id: String = row.get(0)?;
                let header = header_snapshot(&conn, &native_id)?;
                push_composer_ref(
                    &path,
                    native_id,
                    value_bytes(row.get_ref(1)?),
                    gate,
                    header,
                    true,
                    &mut refs,
                )?;
            }
            return Ok(refs);
        }

        // Project only identity, logical revision, and byte length in SQL.
        // This keeps warm runs tiny without comparing filesystem clocks to
        // vendor timestamps (unsafe under clock skew). Full composer JSON
        // crosses into Rust only for a mismatch or missing Capture.
        let mut metadata = Vec::new();
        let sql = format!(
            "SELECT substr(key, 14), ({SQL_ROOT_REVISION}),
                    length(CAST(value AS BLOB))
             FROM cursorDiskKV
             WHERE key >= 'composerData:' AND key < 'composerData;' AND value IS NOT NULL
             ORDER BY key"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            metadata.push((id, row.get::<_, Option<i64>>(1)?, row.get::<_, u64>(2)?));
        }
        drop(rows);
        drop(stmt);
        let mut stmt = conn.prepare("SELECT value FROM cursorDiskKV WHERE key = ?1")?;
        for (native_id, root_revision, root_size) in metadata {
            let header = header_snapshot(&conn, &native_id)?;
            let revision = max_revision(root_revision, header.as_ref().and_then(|h| h.revision));
            let discriminator = logical_discriminator(root_size, header.as_ref());
            if revision
                .is_some_and(|revision| gate.skips(&native_id, revision, discriminator, None, None))
            {
                continue;
            }
            let key = format!("composerData:{native_id}");
            let root = stmt.query_row([key], |row| Ok(value_bytes(row.get_ref(0)?)))?;
            push_composer_ref(&path, native_id, root, gate, header, true, &mut refs)?;
        }
        Ok(refs)
    }

    fn session_working_path(&self, _session: &SessionRef) -> Result<Option<String>> {
        Ok(None)
    }

    fn capture_working_path(&self, _capture: &Capture) -> Result<Option<String>> {
        Ok(None)
    }

    fn capture(&self, session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
        capture_composer(session, captures_dir, host)
    }

    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>> {
        let conversation = extract_capture(capture)?;
        if capture.meta.kind == SessionKind::Subagent
            && (conversation.subagent_parent.is_none() || !conversation.has_dialog())
        {
            return Ok(None);
        }
        Ok(Some(conversation))
    }
}

fn composer_revision(root: &[u8]) -> (Option<i64>, SessionKind) {
    let value = serde_json::from_slice::<Value>(root).ok();
    let revision = value.as_ref().and_then(|v| {
        v.get("lastUpdatedAt")
            .and_then(json_i64)
            .or_else(|| v.get("createdAt").and_then(json_i64))
    });
    let subagent = value
        .as_ref()
        .and_then(|v| v.get("subagentInfo"))
        .is_some_and(|v| v.is_object() && !v.as_object().is_some_and(Map::is_empty));
    (
        revision,
        if subagent {
            SessionKind::Subagent
        } else {
            SessionKind::Main
        },
    )
}

fn push_composer_ref(
    path: &Path,
    native_id: String,
    root: Vec<u8>,
    gate: &EnumerateGate,
    header: Option<HeaderSnapshot>,
    force_candidate: bool,
    refs: &mut Vec<SessionRef>,
) -> Result<()> {
    let (root_revision, root_kind) = composer_revision(&root);
    let kind = if root_kind == SessionKind::Subagent
        || header.as_ref().is_some_and(|header| header.is_subagent)
    {
        SessionKind::Subagent
    } else {
        SessionKind::Main
    };
    let revision = max_revision(root_revision, header.as_ref().and_then(|h| h.revision));
    let mtime_ms = revision.unwrap_or(0);
    let size = logical_discriminator(root.len() as u64, header.as_ref());
    if revision.is_none() {
        tracing::warn!(native_id, "Cursor composerData lacks a logical revision");
    }
    if force_candidate || revision.is_none() || !gate.skips(&native_id, mtime_ms, size, None, None)
    {
        refs.push(SessionRef {
            native_id,
            path: path.to_path_buf(),
            additional_files: Vec::new(),
            project_dir: None,
            kind,
            mtime_ms,
            size,
            native_meta_path: None,
            native_meta_mtime_ms: None,
            native_meta_size: None,
            native_meta: None,
        });
    }
    Ok(())
}

fn max_revision(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn logical_discriminator(root_size: u64, header: Option<&HeaderSnapshot>) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_bytes(&mut hash, &root_size.to_le_bytes());
    if let Some(header) = header {
        hash_bytes(&mut hash, &header.signature.to_le_bytes());
    } else {
        hash_bytes(&mut hash, b"no-header");
    }
    // Capture revisions are persisted in SQLite INTEGER columns by the
    // projection database. Keep the discriminator in the non-negative i64
    // range while retaining 63 hash bits.
    hash & i64::MAX as u64
}

/// Safe filename stem for a Cursor native id.
///
/// UUIDs keep their established Capture names. Short non-UUID ids retain the
/// original hex encoding. Longer ids use SHA-256 so the stem plus retry/temp
/// suffixes remain within the common 255-byte filename limit. The unmodified
/// key remains in Provenance and the captured `composerData:` row.
#[cfg(test)]
pub(crate) fn capture_stem(native_id: &str) -> String {
    super::sqlite::capture_stem(native_id)
}

fn capture_composer(session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
    let mut conn = open_read_only(&session.path, "Cursor")?;
    let tx = conn.transaction()?; // one coherent WAL snapshot for this composer
    let composer_key = format!("composerData:{}", session.native_id);
    let (root, root_type) = tx
        .query_row(
            "SELECT value FROM cursorDiskKV WHERE key = ?1",
            [&composer_key],
            |row| Ok((value_bytes(row.get_ref(0)?), value_type(row.get_ref(0)?))),
        )
        .with_context(|| format!("reading Cursor composer {}", session.native_id))?;
    let (root_revision, root_kind) = composer_revision(&root);
    let header = header_snapshot(&tx, &session.native_id)?;
    let kind = if root_kind == SessionKind::Subagent
        || header.as_ref().is_some_and(|header| header.is_subagent)
    {
        SessionKind::Subagent
    } else {
        SessionKind::Main
    };
    let revision = max_revision(root_revision, header.as_ref().and_then(|h| h.revision))
        .unwrap_or(session.mtime_ms);
    let logical_size = logical_discriminator(root.len() as u64, header.as_ref());

    let base = captures_dir.join(host).join(SOURCE);
    std::fs::create_dir_all(&base)?;
    let paths = super::capture_paths(captures_dir, host, SOURCE, &session.native_id)?;
    let (body_path, meta_path) = (paths.body, paths.meta);
    let had_capture = read_meta(&meta_path).is_some() && body_path.is_file();
    if let Some(existing) = read_meta(&meta_path)
        && body_path.is_file()
    {
        match revision_check(&existing, revision, logical_size) {
            RevisionCheck::Unchanged => {
                return Ok(Capture {
                    body_path,
                    meta_path,
                    meta: existing,
                    written: false,
                    outcome: CaptureOutcome::Unchanged,
                });
            }
            RevisionCheck::Stale => {
                tracing::warn!(
                    native_id = session.native_id,
                    native_revision = revision,
                    capture_revision = existing.source_mtime_ms,
                    "Cursor composer revision moved backwards; keeping the newer Capture"
                );
                return Ok(Capture {
                    body_path,
                    meta_path,
                    meta: existing,
                    written: false,
                    outcome: CaptureOutcome::Unchanged,
                });
            }
            RevisionCheck::Changed => {}
        }
    }

    write_retry_sentinel(&meta_path)?;
    let tmp_body = tmp_sibling(&body_path);
    let write_result = (|| -> Result<()> {
        let output = BufWriter::new(std::fs::File::create(&tmp_body)?);
        let mut encoder = zstd::stream::write::Encoder::new(output, 0)?;
        write_kv_line(
            &mut encoder,
            "cursorDiskKV",
            &composer_key,
            &root,
            root_type,
        )?;
        if let Some(header) = header.as_ref() {
            write_line(
                &mut encoder,
                &CaptureLine {
                    table: "composerHeaders",
                    key: &session.native_id,
                    value: Cow::Borrowed(&header.json),
                    value_type: "row",
                    encoding: None,
                },
            )?;
        }

        let lower = format!("bubbleId:{}:", session.native_id);
        let upper = format!("bubbleId:{};", session.native_id);
        let mut stmt = tx.prepare(
            "SELECT key, value FROM cursorDiskKV
             WHERE key >= ?1 AND key < ?2 ORDER BY key",
        )?;
        let mut rows = stmt.query((&lower, &upper))?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value_ref = row.get_ref(1)?;
            write_kv_line(
                &mut encoder,
                "cursorDiskKV",
                &key,
                &value_bytes(value_ref),
                value_type(value_ref),
            )?;
        }
        let mut output = encoder.finish()?;
        output.flush()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp_body);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp_body, &body_path) {
        let _ = std::fs::remove_file(&tmp_body);
        return Err(error).with_context(|| format!("committing Capture {}", body_path.display()));
    }

    let meta = CaptureMeta {
        repository_resolution: read_meta(&meta_path).and_then(|m| m.repository_resolution),
        schema_version: 1,
        source: SOURCE.to_string(),
        native_id: session.native_id.clone(),
        host: host.to_string(),
        native_path: session.path.to_string_lossy().into_owned(),
        project_dir: None,
        kind,
        source_mtime_ms: revision,
        // For Cursor this hashes root byte length plus the complete typed
        // composerHeaders row; it is a logical discriminator, not DB size.
        source_size: logical_size,
        source_files: Vec::new(),
        native_meta_mtime_ms: None,
        native_meta_size: None,
        captured_at_ms: now_ms(),
        adapter_version: ADAPTER_VERSION,
        superseded_native_ids: Vec::new(),
        needs_retry: false,
        native_meta: None,
        native_birth_time_ms: None,
        goal_attachments: Vec::new(),
    };
    let tmp_meta = tmp_sibling(&meta_path);
    let meta_result = (|| -> Result<()> {
        std::fs::write(&tmp_meta, serde_json::to_string_pretty(&meta)?)?;
        std::fs::rename(&tmp_meta, &meta_path)?;
        Ok(())
    })();
    if let Err(error) = meta_result {
        let _ = std::fs::remove_file(&tmp_meta);
        return Err(error);
    }
    Ok(Capture {
        body_path,
        meta_path,
        meta,
        written: true,
        outcome: if had_capture {
            CaptureOutcome::Extended
        } else {
            CaptureOutcome::New
        },
    })
}

fn read_meta(path: &Path) -> Option<CaptureMeta> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

struct HeaderSnapshot {
    json: String,
    signature: u64,
    revision: Option<i64>,
    is_subagent: bool,
}

fn header_snapshot(conn: &Connection, id: &str) -> Result<Option<HeaderSnapshot>> {
    let columns = table_columns(conn, "composerHeaders")?;
    if columns.is_empty() || !columns.iter().any(|name| name == "composerId") {
        return Ok(None);
    }
    let selected = columns
        .iter()
        .map(|name| quote_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {selected} FROM composerHeaders WHERE composerId = ?1");
    conn.query_row(&sql, [id], |row| {
        let mut object = Map::new();
        let mut signature = 0xcbf2_9ce4_8422_2325_u64;
        let mut revision = None;
        let mut is_subagent = false;
        for (index, name) in columns.iter().enumerate() {
            let value = row.get_ref(index)?;
            hash_bytes(&mut signature, name.as_bytes());
            hash_value(&mut signature, value);
            if matches!(name.as_str(), "lastUpdatedAt" | "createdAt") {
                revision = max_revision(revision, sqlite_i64(value));
            }
            if name == "isSubagent" {
                is_subagent = sqlite_i64(value).is_some_and(|value| value != 0);
            }
            let captured = capture_value(value);
            let mut column = Map::new();
            column.insert("value".into(), captured.value);
            column.insert(
                "valueType".into(),
                Value::String(captured.value_type.into()),
            );
            if let Some(encoding) = captured.encoding {
                column.insert("encoding".into(), Value::String(encoding.into()));
            }
            object.insert(name.clone(), Value::Object(column));
        }
        Ok(HeaderSnapshot {
            json: Value::Object(object).to_string(),
            signature,
            revision,
            is_subagent,
        })
    })
    .optional()
    .map_err(Into::into)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureLine<'a> {
    #[serde(borrow)]
    table: &'a str,
    #[serde(borrow)]
    key: &'a str,
    #[serde(borrow)]
    value: Cow<'a, str>,
    value_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'a str>,
}

fn write_kv_line<W: Write>(
    writer: &mut W,
    table: &str,
    key: &str,
    bytes: &[u8],
    value_type: &str,
) -> Result<()> {
    let (value, encoding) = match std::str::from_utf8(bytes) {
        Ok(text) => (Cow::Borrowed(text), None),
        Err(_) => (
            Cow::Owned(base64::engine::general_purpose::STANDARD.encode(bytes)),
            Some("base64"),
        ),
    };
    write_line(
        writer,
        &CaptureLine {
            table,
            key,
            value,
            value_type,
            encoding,
        },
    )
}

fn write_line<W: Write>(writer: &mut W, row: &CaptureLine<'_>) -> Result<()> {
    serde_json::to_writer(&mut *writer, row)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug)]
struct Node {
    role: Option<Role>,
    text: String,
    timestamp_ms: Option<i64>,
    actions: u32,
    subagent_reference: Option<SubagentReference>,
}

fn extract_capture(capture: &Capture) -> Result<Conversation> {
    let file = std::fs::File::open(&capture.body_path)?;
    let decoder = zstd::stream::read::Decoder::new(BufReader::new(file))?;
    let mut lines = BufReader::new(decoder).lines();
    let first = lines.next().transpose()?.context("empty Cursor Capture")?;
    let root_row: CaptureLine<'_> = serde_json::from_str(&first)?;
    if root_row.table != "cursorDiskKV" || !root_row.key.starts_with("composerData:") {
        bail!("Cursor Capture does not begin with composerData");
    }
    let root_bytes = decode_value(&root_row)?;
    let root: Value =
        serde_json::from_slice(&root_bytes).context("parsing captured composerData")?;
    let mut transcript_facts = cursor_transcript_facts(&root);

    let headers: Vec<String> = root
        .get("fullConversationHeadersOnly")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|h| {
            h.get("bubbleId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let wanted: HashSet<&str> = headers.iter().map(String::as_str).collect();
    let inline = headers
        .is_empty()
        .then(|| {
            root.get("conversation")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
        })
        .flatten();
    let mut by_id = HashMap::new();
    let mut fallback = Vec::new();
    let mut orphan_task_references = Vec::new();
    let mut bubble_project = None;

    if inline.is_none() {
        for line in lines {
            let line = line?;
            let row: CaptureLine<'_> = match serde_json::from_str(&line) {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(capture = %capture.body_path.display(), %error, "bad Cursor Capture row");
                    continue;
                }
            };
            if !row.key.starts_with("bubbleId:") {
                continue;
            }
            if row.value_type == "null" {
                continue;
            }
            let bubble_id = row.key.rsplit(':').next().unwrap_or_default();
            let raw = decode_value(&row)?;
            let bubble: Value = match serde_json::from_slice(&raw) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(key = row.key, %error, "bad Cursor bubble JSON");
                    continue;
                }
            };
            collect_referenced_files(&bubble, &mut transcript_facts);
            if !headers.is_empty() && !wanted.contains(bubble_id) {
                if let Some(reference) = cursor_task_reference(&bubble, None) {
                    orphan_task_references.push(reference);
                }
                continue;
            }
            bubble_project = bubble_project.or_else(|| context_path(&bubble));
            let node = bubble_node(&bubble, !headers.is_empty());
            if headers.is_empty() {
                fallback.push((bubble_id.to_string(), node));
            } else {
                by_id.insert(bubble_id.to_string(), node);
            }
        }
    }

    let nodes: Vec<Node> = if !headers.is_empty() {
        let mut missing = 0usize;
        let ordered = headers
            .iter()
            .filter_map(|id| match by_id.remove(id) {
                Some(node) => Some(node),
                None => {
                    missing += 1;
                    None
                }
            })
            .collect();
        if missing > 0 {
            tracing::warn!(capture = %capture.body_path.display(), missing, "Cursor headers reference missing bubbles");
        }
        ordered
    } else if let Some(inline) = inline {
        for bubble in inline {
            bubble_project = bubble_project.or_else(|| context_path(bubble));
            collect_referenced_files(bubble, &mut transcript_facts);
        }
        inline
            .iter()
            .map(|bubble| bubble_node(bubble, false))
            .collect()
    } else {
        fallback.sort_by(|(left_id, left), (right_id, right)| {
            match (left.timestamp_ms, right.timestamp_ms) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| left_id.cmp(right_id))
        });
        fallback.into_iter().map(|(_, node)| node).collect()
    };
    let (messages, trailing_actions) = nodes_to_messages(&nodes);
    let title = root
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let created_at_ms = root.get("createdAt").and_then(json_i64);
    let updated_at_ms = root.get("lastUpdatedAt").and_then(json_i64);
    let project_path = project_path(&root).or(bubble_project);
    transcript_facts.normalize_file_paths(project_path.as_deref());
    let subagent_parent = cursor_subagent_parent(&root);
    let mut subagent_references = cursor_subagent_references(&root);
    subagent_references.extend(
        nodes
            .iter()
            .filter_map(|node| node.subagent_reference.clone()),
    );
    subagent_references.extend(orphan_task_references);
    deduplicate_task_references(&mut subagent_references);

    Ok(Conversation {
        provenance: capture.provenance(),
        title,
        project_path,
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
        trailing_actions,
    })
}

fn cursor_subagent_parent(root: &Value) -> Option<SubagentParent> {
    let info = root.get("subagentInfo")?.as_object()?;
    let parent_native_id = info.get("parentComposerId")?.as_str()?.trim();
    (!parent_native_id.is_empty()).then(|| SubagentParent {
        parent_native_id: parent_native_id.to_string(),
        description: root
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        agent_type: info
            .get("subagentTypeName")
            .and_then(Value::as_str)
            .map(str::to_string),
        instructions: None,
        depth: None,
        native_key: None,
        link_key: info
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn cursor_subagent_references(root: &Value) -> Vec<SubagentReference> {
    root.get("subagentComposerIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|child_native_id| SubagentReference {
            child_native_id: Some(child_native_id.to_string()),
            child_native_key: None,
            link_key: None,
            description: None,
            agent_type: None,
            instructions: None,
            spawned_at_ms: None,
        })
        .collect()
}

fn decode_value(row: &CaptureLine<'_>) -> Result<Vec<u8>> {
    if row.encoding == Some("base64") {
        return base64::engine::general_purpose::STANDARD
            .decode(row.value.as_bytes())
            .context("decoding Cursor Capture base64");
    }
    Ok(row.value.as_bytes().to_vec())
}

fn bubble_node(bubble: &Value, authoritative_placement: bool) -> Node {
    let role = match bubble.get("type").and_then(json_i64) {
        Some(1) => Some(Role::User),
        Some(2) => Some(Role::Assistant),
        _ => None,
    };
    let text = bubble_text(bubble);
    let timestamp_ms = bubble
        .get("createdAt")
        .and_then(timestamp)
        .or_else(|| bubble.get("updatedAt").and_then(timestamp));
    let actions = u32::from(bubble.get("toolFormerData").is_some_and(|v| !v.is_null()));
    Node {
        role,
        text,
        timestamp_ms,
        actions,
        subagent_reference: cursor_task_reference(
            bubble,
            authoritative_placement.then_some(timestamp_ms).flatten(),
        ),
    }
}

/// Cursor can retain several rewritten task bubbles for one native task.
/// Keep the current authoritative header copy before any orphan copy, and
/// leave references without a native `toolCallId` distinct rather than
/// conflating unrelated legacy tasks.
fn deduplicate_task_references(references: &mut Vec<SubagentReference>) {
    let mut seen = HashSet::new();
    references.retain(|reference| {
        let Some(child_native_id) = reference.child_native_id.as_deref() else {
            return true;
        };
        let Some(link_key) = reference.link_key.as_deref() else {
            return true;
        };
        seen.insert((child_native_id.to_owned(), link_key.to_owned()))
    });
}

fn cursor_task_reference(bubble: &Value, timestamp_ms: Option<i64>) -> Option<SubagentReference> {
    let tool = bubble.get("toolFormerData")?;
    if tool.get("name").and_then(Value::as_str) != Some("task_v2") {
        return None;
    }
    let result = match tool.get("result")? {
        Value::String(json) => serde_json::from_str::<Value>(json).ok()?,
        value => value.clone(),
    };
    let child_native_id = result.get("agentId")?.as_str()?.trim();
    if child_native_id.is_empty() {
        return None;
    }
    let params = tool.get("params").unwrap_or(&Value::Null);
    Some(SubagentReference {
        child_native_id: Some(child_native_id.to_string()),
        child_native_key: None,
        link_key: tool
            .get("toolCallId")
            .or_else(|| bubble.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        description: params
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        agent_type: params
            .get("subagentType")
            .or_else(|| params.get("type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        instructions: params
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string),
        spawned_at_ms: timestamp_ms,
    })
}

fn nodes_to_messages(nodes: &[Node]) -> (Vec<Message>, u32) {
    let mut messages = Vec::new();
    let mut pending = 0u32;
    for node in nodes {
        if let Some(role) = node.role
            && !node.text.trim().is_empty()
        {
            messages.push(Message {
                role,
                kind: MessageKind::Dialog,
                text: node.text.clone(),
                timestamp_ms: node.timestamp_ms,
                actions_before: std::mem::take(&mut pending),
            });
        }
        pending += node.actions;
    }
    (messages, pending)
}

fn bubble_text(bubble: &Value) -> String {
    for field in ["text", "richText", "markdown"] {
        let Some(value) = bubble.get(field) else {
            continue;
        };
        let text = if field == "richText" {
            rich_text(value)
        } else if field == "markdown" && !value.is_string() {
            nested_plain_text(value)
        } else {
            value.as_str().unwrap_or_default().trim().to_string()
        };
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

fn nested_plain_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(nested_plain_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(values) => values
            .values()
            .map(nested_plain_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn rich_text(value: &Value) -> String {
    let owned;
    let tree = if let Some(text) = value.as_str() {
        if !text.trim_start().starts_with('{') {
            return text.trim().to_string();
        }
        owned = match serde_json::from_str::<Value>(text) {
            Ok(value) => value,
            Err(_) => return text.trim().to_string(),
        };
        &owned
    } else {
        value
    };
    let mut parts = Vec::new();
    collect_rich_text(tree, &mut parts);
    parts.join(" ").trim().to_string()
}

fn collect_rich_text(value: &Value, out: &mut Vec<String>) {
    if value.get("type").and_then(Value::as_str) == Some("text")
        && let Some(text) = value.get("text").and_then(Value::as_str)
        && !text.is_empty()
    {
        out.push(text.to_string());
    }
    if let Some(root) = value.get("root") {
        collect_rich_text(root, out);
    }
    for field in ["children", "content"] {
        if let Some(children) = value.get(field).and_then(Value::as_array) {
            for child in children {
                collect_rich_text(child, out);
            }
        }
    }
}

fn timestamp(value: &Value) -> Option<i64> {
    json_i64(value).or_else(|| value.as_str().and_then(|s| parse_iso_ms(s).ok()))
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn project_path(root: &Value) -> Option<String> {
    let config = root
        .pointer("/workspaceIdentifier/configPath/path")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tracked = root
        .get("trackedGitRepos")
        .and_then(Value::as_array)
        .and_then(|repos| {
            repos
                .iter()
                .filter_map(|repo| {
                    let path = repo.get("repoPath").and_then(Value::as_str)?;
                    let recent = repo
                        .get("lastInteractionAt")
                        .and_then(json_i64)
                        .unwrap_or(0);
                    Some((recent, path.to_string()))
                })
                .max_by_key(|(recent, _)| *recent)
                .map(|(_, path)| path)
        });
    if config
        .as_deref()
        .is_some_and(|p| p.contains("/Cursor/glassMultiRootWorkspaces/"))
    {
        return tracked.or(config.map(strip_workspace_suffix));
    }
    config
        .map(strip_workspace_suffix)
        .or(tracked)
        .or_else(|| context_path(root))
}

fn strip_workspace_suffix(path: String) -> String {
    path.strip_suffix(".code-workspace")
        .unwrap_or(&path)
        .to_string()
}

fn context_path(value: &Value) -> Option<String> {
    let context = value.get("context").unwrap_or(value);
    if let Some(folder) = context
        .get("folderSelections")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .and_then(selection_path)
    {
        return Some(folder);
    }
    context
        .get("fileSelections")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .and_then(selection_path)
        .and_then(|path| {
            Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
        })
}

fn selection_path(value: &Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("path").and_then(Value::as_str))
        .or_else(|| value.pointer("/uri/fsPath").and_then(Value::as_str))
        .or_else(|| value.pointer("/uri/path").and_then(Value::as_str))
        .map(str::to_string)
}

/// Cursor records edit bookkeeping separately from contextual selections.
/// Keep that distinction: original/new file state is direct modified/created
/// evidence; `context.fileSelections` is only a reference.
fn cursor_transcript_facts(root: &Value) -> TranscriptFacts {
    let mut facts = TranscriptFacts::default();
    if let Some(original_states) = root.get("originalFileStates").and_then(Value::as_object) {
        facts.patched_files.extend(
            original_states
                .keys()
                .filter_map(|uri| source_file_path(uri)),
        );
    }
    if let Some(new_files) = root.get("newlyCreatedFiles").and_then(Value::as_array) {
        facts
            .patched_files
            .extend(new_files.iter().filter_map(explicit_file_path));
    }
    collect_referenced_files(root, &mut facts);
    facts
}

fn collect_referenced_files(value: &Value, facts: &mut TranscriptFacts) {
    let context = value.get("context").unwrap_or(value);
    let Some(selections) = context.get("fileSelections").and_then(Value::as_array) else {
        return;
    };
    facts
        .referenced_files
        .extend(selections.iter().filter_map(explicit_file_path));
}

fn explicit_file_path(value: &Value) -> Option<String> {
    value
        .as_str()
        .and_then(source_file_path)
        .or_else(|| selection_path(value))
}

fn source_file_path(value: &str) -> Option<String> {
    if let Some(path) = value.strip_prefix("file://localhost/") {
        return Some(format!("/{path}"));
    }
    if let Some(path) = value.strip_prefix("file://") {
        return Some(path.to_string());
    }
    Path::new(value).is_absolute().then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_stems_are_bounded_and_stable() {
        let uuid = "11111111-1111-4111-8111-111111111111";
        assert_eq!(capture_stem(uuid), uuid);
        assert_eq!(capture_stem("draft"), "id-6472616674");

        let largest_inline = "x".repeat(118);
        assert_eq!(
            capture_stem(&largest_inline).len() + ".meta.json.retry".len(),
            255
        );
        let long = "x".repeat(119);
        let stem = capture_stem(&long);
        assert!(stem.starts_with("id-sha256-"));
        assert_eq!(stem.len(), "id-sha256-".len() + 64);
        assert_eq!(stem, capture_stem(&long));
        assert_ne!(stem, capture_stem(&format!("{long}y")));
    }

    #[test]
    fn nullable_header_subagent_flag_is_false() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE composerHeaders(composerId TEXT PRIMARY KEY, isSubagent INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO composerHeaders VALUES ('11111111-1111-4111-8111-111111111111', NULL)",
            [],
        )
        .unwrap();
        let header = header_snapshot(&conn, "11111111-1111-4111-8111-111111111111")
            .unwrap()
            .unwrap();
        assert!(!header.is_subagent);
    }

    #[test]
    fn walks_both_rich_text_dialects() {
        let lexical = Value::String(
            r#"{"root":{"children":[{"type":"text","text":"hello"},{"children":[{"type":"text","text":"world"}]}]}}"#.into(),
        );
        let tiptap = Value::String(
            r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"hello"}]}]}"#.into(),
        );
        assert_eq!(rich_text(&lexical), "hello world");
        assert_eq!(rich_text(&tiptap), "hello");
        assert_eq!(rich_text(&Value::String("plain".into())), "plain");
        assert_eq!(rich_text(&Value::String("{not-json".into())), "{not-json");
    }

    #[test]
    fn tool_bubbles_leave_action_seams() {
        let nodes = vec![
            bubble_node(&serde_json::json!({"type":1,"text":"go"}), true),
            bubble_node(
                &serde_json::json!({"type":2,"toolFormerData":{"name":"run"}}),
                true,
            ),
            bubble_node(&serde_json::json!({"type":2,"text":"done"}), true),
        ];
        let (messages, trailing) = nodes_to_messages(&nodes);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].actions_before, 1);
        assert_eq!(trailing, 0);
    }

    #[test]
    fn explicit_file_bookkeeping_and_selections_become_distinct_facts() {
        let root = serde_json::json!({
            "originalFileStates": {
                "file:///work/app/src/old.rs": {"content": "before"}
            },
            "newlyCreatedFiles": ["file:///work/app/src/new.rs"],
            "context": {
                "fileSelections": [{"uri": {"fsPath": "/work/app/docs/design.md"}}]
            }
        });
        let mut facts = cursor_transcript_facts(&root);
        facts.normalize_file_paths(Some("/work/app"));
        assert_eq!(facts.patched_files, ["src/new.rs", "src/old.rs"]);
        assert_eq!(facts.referenced_files, ["docs/design.md"]);
    }
}
