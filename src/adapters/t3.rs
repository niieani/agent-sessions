//! T3 Code Orchestrator Adapter (`userdata/statev2.sqlite`).
//!
//! Capture preserves one Thread's selected SQLite rows and safe attachment
//! bodies. Extraction into canonical Orchestrator facts is a separate phase.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};

use super::sqlite::{
    FNV_OFFSET_BASIS, capture_value, hash_bytes, hash_value, open_read_only, quote_identifier,
    table_columns,
};
use super::{
    Adapter, AdapterExtraction, Capture, CaptureMeta, CaptureOutcome, EnumerateGate, RevisionCheck,
    SessionKind, SessionRef, now_ms, tmp_sibling, write_retry_sentinel,
};
use crate::model::{
    Conversation, OrchestratorActivity, OrchestratorLinkFact, OrchestratorLinkStatus,
    OrchestratorMessage, OrchestratorProjection, OrchestratorTurn,
};
use crate::timefmt::parse_iso_ms;

mod attachments;
mod extract;
mod legacy;
mod schema;
use super::commit_staged;
use super::supersede::revision_check_for_adapter;
use attachments::*;
pub use extract::extract_projection;
use schema::{RowQuery, row_queries, validate_schema};
#[cfg(test)]
mod tests;

pub const SOURCE: &str = "t3";
/// Fixed boundary: revisions before this belong to the abandoned v1 Store.
pub(crate) const V2_ADAPTER_VERSION: u32 = 3;
pub const ADAPTER_VERSION: u32 = 5;
pub const STORE_PATH: &str = "userdata/statev2.sqlite";
const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

pub fn default_store_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".t3")
}

pub struct T3Adapter;

impl Adapter for T3Adapter {
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
        let store = store_root.join(STORE_PATH);
        ensure!(
            store.is_file(),
            "T3 root has no Store at {}",
            store.display()
        );
        let mut conn = open_t3(&store)?;
        let tx = conn.transaction()?;
        validate_schema(&tx)?;
        let mut identities = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT thread_id, project_id FROM orchestration_v2_projection_threads ORDER BY thread_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                identities.push(row?);
            }
        }
        let mut refs = Vec::new();
        for (thread_id, project_id) in identities {
            let (revision, mut discriminator) = metadata_revision(&tx, &thread_id, &project_id)?;
            hash_attachment_metadata(
                &tx,
                &thread_id,
                &store_root.join("userdata/attachments"),
                &mut discriminator,
            )?;
            if !gate.skips(&thread_id, revision, discriminator, None, None)
                || gate.requires_adapter_refresh(&thread_id, ADAPTER_VERSION)
            {
                refs.push(SessionRef {
                    native_id: thread_id,
                    path: store.clone(),
                    additional_files: Vec::new(),
                    project_dir: Some(project_id),
                    kind: SessionKind::Orchestrator,
                    mtime_ms: revision,
                    size: discriminator,
                    native_meta_path: None,
                    native_meta_mtime_ms: None,
                    native_meta_size: None,
                    native_meta: None,
                });
            }
        }
        tx.commit()?;
        Ok(refs)
    }

    fn capture(&self, session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
        capture_thread(session, captures_dir, host)
    }

    fn session_working_path(&self, session: &SessionRef) -> Result<Option<String>> {
        let conn = open_t3(&session.path)?;
        let payload: String = conn.query_row(
            "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id = ?1",
            [&session.native_id],
            |row| row.get(0),
        )?;
        let payload: Value = serde_json::from_str(&payload)?;
        Ok(payload["worktreePath"].as_str().map(str::to_owned))
    }

    fn capture_working_path(&self, capture: &Capture) -> Result<Option<String>> {
        Ok(extract_projection(capture)?.worktree)
    }

    fn extract(&self, _capture: &Capture) -> Result<Option<Conversation>> {
        Ok(None)
    }

    fn extract_unit(
        &self,
        capture: &Capture,
        _direct_parent: Option<&Capture>,
        _context: &super::ExtractionContext,
    ) -> Result<Option<AdapterExtraction>> {
        extract_projection(capture)
            .map(|projection| AdapterExtraction::Orchestrator(Box::new(projection)))
            .map(Some)
    }
}

fn open_t3(path: &Path) -> Result<Connection> {
    let conn = open_read_only(path, "T3")?;
    conn.create_scalar_function(
        "agent_sessions_typed_hash",
        -1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let mut hash = FNV_OFFSET_BASIS;
            for index in 0..ctx.len() {
                hash_value(&mut hash, ctx.get_raw(index));
            }
            Ok((hash & i64::MAX as u64) as i64)
        },
    )?;
    Ok(conn)
}

fn metadata_revision(conn: &Connection, thread_id: &str, project_id: &str) -> Result<(i64, u64)> {
    let mut revision = 0;
    let mut hash = FNV_OFFSET_BASIS;
    for query in row_queries(thread_id, project_id) {
        hash_bytes(&mut hash, query.table.as_bytes());
        let columns = table_columns(conn, query.table)?;
        for column in &columns {
            hash_bytes(&mut hash, column.as_bytes());
        }
        let hash_args = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(",");
        let revision_columns = columns
            .iter()
            .filter(|column| is_revision_column(column))
            .cloned()
            .collect::<Vec<_>>();
        let selected_revision = revision_columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(",");
        let comma = if selected_revision.is_empty() {
            ""
        } else {
            ","
        };
        let sql = format!(
            "SELECT agent_sessions_typed_hash({hash_args}){comma}{selected_revision} FROM {} WHERE {} ORDER BY {}",
            quote_identifier(query.table),
            query.predicate,
            query.order
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params![query.id])?;
        while let Some(row) = rows.next()? {
            let row_hash = row.get::<_, i64>(0)? as u64;
            hash_bytes(&mut hash, &row_hash.to_le_bytes());
            for index in 0..revision_columns.len() {
                revision = revision.max(revision_ms(row.get_ref(index + 1)?));
            }
        }
    }
    Ok((revision, hash & i64::MAX as u64))
}

fn is_revision_column(column: &str) -> bool {
    column.ends_with("_at") || matches!(column, "occurred_at" | "last_seen_at")
}

fn revision_ms(value: ValueRef<'_>) -> i64 {
    match value {
        ValueRef::Integer(value) => value,
        ValueRef::Real(value) if value.is_finite() => value as i64,
        ValueRef::Text(value) => std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse().ok().or_else(|| parse_iso_ms(value).ok()))
            .unwrap_or(0),
        _ => 0,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturedRow {
    record: &'static str,
    table: &'static str,
    primary_key: BTreeMap<String, Value>,
    columns: Vec<CapturedColumn>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturedColumn {
    name: String,
    value_type: &'static str,
    value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturedAttachment {
    record: &'static str,
    attachment_id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturedAttachmentChunk<'a> {
    record: &'static str,
    attachment_id: &'a str,
    index: u64,
    value_type: &'static str,
    encoding: &'static str,
    value: String,
}

fn capture_thread(session: &SessionRef, captures_dir: &Path, host: &str) -> Result<Capture> {
    let mut conn = open_t3(&session.path)?;
    let tx = conn.transaction()?;
    validate_schema(&tx)?;
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM orchestration_v2_projection_threads WHERE thread_id=?1",
            [&session.native_id],
            |row| row.get(0),
        )
        .with_context(|| format!("reading T3 Thread {}", session.native_id))?;
    let (revision, mut discriminator) = metadata_revision(&tx, &session.native_id, &project_id)?;
    let attachment_root = session
        .path
        .parent()
        .context("T3 Store has no userdata directory")?
        .join("attachments");
    hash_attachment_metadata(
        &tx,
        &session.native_id,
        &attachment_root,
        &mut discriminator,
    )?;
    let base = captures_dir.join(host).join(SOURCE);
    std::fs::create_dir_all(&base)?;
    let paths = super::capture_paths(captures_dir, host, SOURCE, &session.native_id)?;
    let (body_path, meta_path) = (paths.body, paths.meta);
    let existing = read_meta(&meta_path).filter(|_| body_path.is_file());
    if let Some(existing) = existing.as_ref() {
        match revision_check_for_adapter(existing, revision, discriminator, ADAPTER_VERSION) {
            RevisionCheck::Unchanged
                if existing.adapter_version == ADAPTER_VERSION
                    && !has_retryable_attachment(existing) =>
            {
                return Ok(Capture {
                    body_path,
                    meta_path,
                    meta: existing.clone(),
                    written: false,
                    outcome: CaptureOutcome::Unchanged,
                });
            }
            RevisionCheck::Unchanged => {}
            RevisionCheck::Stale => bail!(
                "T3 Thread {} logical revision moved backwards from {} to {}",
                session.native_id,
                existing.source_mtime_ms,
                revision
            ),
            RevisionCheck::Changed => {}
        }
    }
    write_retry_sentinel(&meta_path)?;
    let tmp_body = tmp_sibling(&body_path);
    let output = BufWriter::new(std::fs::File::create(&tmp_body)?);
    let mut encoder = zstd::stream::write::Encoder::new(output, 0)?;
    let mut attachment_descriptors = BTreeMap::new();
    for query in row_queries(&session.native_id, &project_id) {
        capture_rows(&tx, query, &mut encoder, &mut attachment_descriptors)?;
    }
    let mut attachments = Vec::new();
    for (_, descriptor) in attachment_descriptors {
        let captured = write_captured_attachment(&attachment_root, &descriptor, &mut encoder)?;
        attachments.push(json!({"attachmentId": captured.attachment_id, "status": captured.status, "size": captured.size}));
    }
    let mut output = encoder.finish()?;
    output.flush()?;
    let meta = CaptureMeta {
        repository_resolution: existing
            .as_ref()
            .and_then(|m| m.repository_resolution.clone()),
        schema_version: 1,
        source: SOURCE.into(),
        native_id: session.native_id.clone(),
        host: host.into(),
        native_path: session.path.to_string_lossy().into_owned(),
        project_dir: None,
        kind: SessionKind::Orchestrator,
        source_mtime_ms: revision,
        source_size: discriminator,
        source_files: Vec::new(),
        native_meta_mtime_ms: None,
        native_meta_size: None,
        captured_at_ms: now_ms(),
        adapter_version: ADAPTER_VERSION,
        superseded_native_ids: Vec::new(),
        needs_retry: false,
        native_meta: Some(
            json!({"storageSchema": 2, "projectId": project_id, "storePath": session.path, "attachments": attachments}),
        ),
        native_birth_time_ms: None,
        goal_attachments: Vec::new(),
    };
    let capture = commit_staged(existing.as_ref(), &tmp_body, &body_path, &meta_path, meta)?
        .context("T3 logical Capture was unexpectedly refused")?;
    tx.commit()?;
    Ok(capture)
}

fn capture_rows<W: Write>(
    conn: &Connection,
    query: RowQuery<'_>,
    writer: &mut W,
    attachments: &mut BTreeMap<String, AttachmentDescriptor>,
) -> Result<()> {
    let sql = format!(
        "SELECT * FROM {} WHERE {} ORDER BY {}",
        quote_identifier(query.table),
        query.predicate,
        query.order
    );
    let mut stmt = conn.prepare(&sql)?;
    let names = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let mut rows = stmt.query(params![query.id])?;
    while let Some(row) = rows.next()? {
        let mut columns = Vec::with_capacity(names.len());
        let mut primary_key = BTreeMap::new();
        for (index, name) in names.iter().enumerate() {
            let captured = capture_value(row.get_ref(index)?);
            let column = CapturedColumn {
                name: name.clone(),
                value_type: captured.value_type,
                value: captured.value,
                encoding: captured.encoding,
            };
            if query.primary_key.contains(&name.as_str()) {
                primary_key.insert(name.clone(), column.value.clone());
            }
            if query.table == "orchestration_v2_projection_messages" && name == "payload_json" {
                collect_message_attachments(&column.value, attachments)?;
            }
            columns.push(column);
        }
        serde_json::to_writer(
            &mut *writer,
            &CapturedRow {
                record: "row",
                table: query.table,
                primary_key,
                columns,
            },
        )?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn read_meta(path: &Path) -> Option<CaptureMeta> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Extract canonical Orchestrator facts from the self-contained Capture.
/// Validate the native fields needed by title planning and revalidation.
pub(crate) fn validate_title_schema(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "orchestration_v2_projection_threads")?;
    for required in ["thread_id", "title"] {
        ensure!(
            columns.iter().any(|column| column == required),
            "incompatible T3 orchestration v2 Store: orchestration_v2_projection_threads missing column {required}"
        );
    }
    Ok(())
}

pub(crate) use extract::repository_fallbacks;
