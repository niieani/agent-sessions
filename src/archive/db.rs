//! The regenerable SQLite projection: sessions, hosts, messages, Bookmarks.
//!
//! Native projections rebuild from Captures; assignment indexes rebuild from
//! durable Project Assignment files. The schema is free to
//! evolve. Session identity is (Source, native id) per ADR-0004 — upserts are
//! idempotent on that key, and Hosts accumulate in `session_hosts` as
//! provenance, never as identity.

mod branches;
mod host_pulls;
mod repositories;
pub use host_pulls::HostPullRecord;
mod render_state;
pub use render_state::RenderFileState;
mod native_projects;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::adapters::{CaptureMeta, CaptureOrdering, SessionKind, capture_ordering};
use crate::model::{
    Augmentation, Conversation, GitSnapshot, InheritedContext, Message, MessageKind, NativeProject,
    OrchestratorMessage, OrchestratorProjection, Role, SubagentLink, SubagentParent,
    TranscriptFacts,
};

mod assignments;
mod leads;
mod origins;
mod recovery;

pub const SCHEMA_VERSION: i64 = 19;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_origins (
 source TEXT NOT NULL, native_id TEXT NOT NULL, record_json TEXT NOT NULL,
 PRIMARY KEY (source,native_id)
);
CREATE TABLE IF NOT EXISTS session_leads (
 source TEXT NOT NULL, native_id TEXT NOT NULL, record_json TEXT NOT NULL,
 PRIMARY KEY (source,native_id)
);
CREATE TABLE IF NOT EXISTS project_assignments (
  source TEXT NOT NULL,
  native_id TEXT NOT NULL,
  project TEXT NOT NULL,
  assigned_at_ms INTEGER NOT NULL,
  reason TEXT,
  PRIMARY KEY (source, native_id)
);
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id               INTEGER PRIMARY KEY,
  source           TEXT NOT NULL,
  native_id        TEXT NOT NULL,
  title            TEXT,
  project          TEXT,
  project_path     TEXT,
  created_at_ms    INTEGER,
  updated_at_ms    INTEGER,
  message_count    INTEGER NOT NULL DEFAULT 0,
  trailing_actions INTEGER NOT NULL DEFAULT 0,
  captured_at_ms   INTEGER NOT NULL DEFAULT 0,
  starting_model TEXT,
  starting_reasoning_effort TEXT,
  git_branch TEXT,
  git_commit TEXT,
  git_repository_url TEXT,
  patched_files_json TEXT NOT NULL DEFAULT '[]',
  referenced_files_json TEXT NOT NULL DEFAULT '[]',
  inherited_context_source_id TEXT,
  inherited_context_copied_item_count INTEGER,
  has_dialog       INTEGER NOT NULL DEFAULT 0,
  winner_host      TEXT,
  winner_source_mtime_ms INTEGER,
  winner_source_size INTEGER,
  winner_native_meta_mtime_ms INTEGER,
  winner_native_meta_size INTEGER,
  winner_adapter_version INTEGER,
  UNIQUE (source, native_id)
);
CREATE TABLE IF NOT EXISTS session_hosts (
  session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  host        TEXT NOT NULL,
  native_path TEXT,
  UNIQUE (session_id, host)
);
CREATE TABLE IF NOT EXISTS messages (
  session_id     INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  idx            INTEGER NOT NULL,
  role           TEXT NOT NULL,
  kind           TEXT NOT NULL DEFAULT 'dialog',
  text           TEXT NOT NULL,
  timestamp_ms   INTEGER,
  actions_before INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, idx)
);
CREATE INDEX IF NOT EXISTS messages_timestamp_idx ON messages(session_id, timestamp_ms);
CREATE TABLE IF NOT EXISTS session_revisions (
  source                       TEXT NOT NULL,
  native_id                    TEXT NOT NULL,
  host                         TEXT NOT NULL,
  source_mtime_ms              INTEGER NOT NULL,
  source_size                  INTEGER NOT NULL,
  native_meta_mtime_ms         INTEGER,
  native_meta_size             INTEGER,
  adapter_version              INTEGER NOT NULL,
  PRIMARY KEY (source, native_id, host)
);
CREATE TABLE IF NOT EXISTS session_projections (
  source           TEXT NOT NULL,
  native_id        TEXT NOT NULL,
  host             TEXT NOT NULL,
  title            TEXT,
  project          TEXT,
  project_path     TEXT,
  created_at_ms    INTEGER,
  updated_at_ms    INTEGER,
  message_count    INTEGER NOT NULL,
  trailing_actions INTEGER NOT NULL,
  captured_at_ms   INTEGER NOT NULL DEFAULT 0,
  starting_model TEXT,
  starting_reasoning_effort TEXT,
  git_branch TEXT,
  git_commit TEXT,
  git_repository_url TEXT,
  patched_files_json TEXT NOT NULL DEFAULT '[]',
  referenced_files_json TEXT NOT NULL DEFAULT '[]',
  inherited_context_source_id TEXT,
  inherited_context_copied_item_count INTEGER,
  has_dialog       INTEGER NOT NULL,
  PRIMARY KEY (source, native_id, host)
);
CREATE TABLE IF NOT EXISTS projection_messages (
  source         TEXT NOT NULL,
  native_id      TEXT NOT NULL,
  host           TEXT NOT NULL,
  idx            INTEGER NOT NULL,
  role           TEXT NOT NULL,
  kind           TEXT NOT NULL DEFAULT 'dialog',
  text           TEXT NOT NULL,
  timestamp_ms   INTEGER,
  actions_before INTEGER NOT NULL,
  PRIMARY KEY (source, native_id, host, idx)
);
CREATE TABLE IF NOT EXISTS subagent_parent_facts (
  fact_host         TEXT NOT NULL,
  child_source      TEXT NOT NULL,
  child_native_id   TEXT NOT NULL,
  parent_native_id  TEXT NOT NULL,
  description       TEXT,
  agent_type        TEXT,
  instructions      TEXT,
  depth             INTEGER,
  native_key        TEXT,
  link_key          TEXT,
  PRIMARY KEY (fact_host, child_source, child_native_id)
);
CREATE TABLE IF NOT EXISTS subagent_reference_facts (
  fact_host         TEXT NOT NULL,
  parent_source     TEXT NOT NULL,
  parent_native_id  TEXT NOT NULL,
  idx               INTEGER NOT NULL,
  child_native_id   TEXT,
  child_native_key  TEXT,
  link_key          TEXT,
  description       TEXT,
  agent_type        TEXT,
  instructions      TEXT,
  spawned_at_ms     INTEGER,
  PRIMARY KEY (fact_host, parent_source, parent_native_id, idx)
);
CREATE TABLE IF NOT EXISTS subagent_links (
  parent_source     TEXT NOT NULL,
  parent_native_id  TEXT NOT NULL,
  child_source      TEXT NOT NULL,
  child_native_id   TEXT NOT NULL,
  description       TEXT,
  agent_type        TEXT,
  instructions      TEXT,
  spawned_at_ms     INTEGER,
  depth             INTEGER,
  PRIMARY KEY (parent_source, parent_native_id, child_source, child_native_id)
);
CREATE TABLE IF NOT EXISTS orchestrator_thread_projections (
  source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL,
  native_path TEXT NOT NULL, logical_revision_ms INTEGER NOT NULL,
  discriminator INTEGER NOT NULL, adapter_version INTEGER NOT NULL,
  title TEXT, project_id TEXT, project_title TEXT, workspace_root TEXT,
  provider_name TEXT, provider_instance_id TEXT, model TEXT,
  runtime_mode TEXT, interaction_mode TEXT, branch TEXT, worktree TEXT,
  created_at_ms INTEGER, updated_at_ms INTEGER,
  archived INTEGER NOT NULL, pinned INTEGER NOT NULL, settled INTEGER NOT NULL,
  linked_pull_request TEXT, checkpoint_count INTEGER NOT NULL,
  PRIMARY KEY (source, native_id, host)
);
CREATE TABLE IF NOT EXISTS orchestrator_projection_link_facts (
  source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL, idx INTEGER NOT NULL,
  status TEXT NOT NULL, provider_source TEXT, provider_native_id TEXT, reason TEXT,
  PRIMARY KEY (source, native_id, host, idx),
  FOREIGN KEY (source, native_id, host) REFERENCES orchestrator_thread_projections(source, native_id, host) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_projection_messages (
  source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL, idx INTEGER NOT NULL,
  role TEXT NOT NULL, text TEXT NOT NULL, timestamp_ms INTEGER,
  PRIMARY KEY (source, native_id, host, idx),
  FOREIGN KEY (source, native_id, host) REFERENCES orchestrator_thread_projections(source, native_id, host) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_projection_turns (
  source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL, idx INTEGER NOT NULL,
  turn_native_id TEXT NOT NULL, state TEXT, checkpoint_id TEXT, payload_json TEXT,
  PRIMARY KEY (source, native_id, host, idx),
  FOREIGN KEY (source, native_id, host) REFERENCES orchestrator_thread_projections(source, native_id, host) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_projection_activities (
  source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL, idx INTEGER NOT NULL,
  activity_native_id TEXT NOT NULL, kind TEXT, payload_json TEXT,
  PRIMARY KEY (source, native_id, host, idx),
  FOREIGN KEY (source, native_id, host) REFERENCES orchestrator_thread_projections(source, native_id, host) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_threads (
  source TEXT NOT NULL, native_id TEXT NOT NULL, winner_host TEXT NOT NULL,
  native_path TEXT NOT NULL, logical_revision_ms INTEGER NOT NULL,
  discriminator INTEGER NOT NULL, adapter_version INTEGER NOT NULL,
  title TEXT, project_id TEXT, project_title TEXT, workspace_root TEXT,
  provider_name TEXT, provider_instance_id TEXT, model TEXT,
  runtime_mode TEXT, interaction_mode TEXT, branch TEXT, worktree TEXT,
  created_at_ms INTEGER, updated_at_ms INTEGER,
  archived INTEGER NOT NULL, pinned INTEGER NOT NULL, settled INTEGER NOT NULL,
  linked_pull_request TEXT, turn_count INTEGER NOT NULL, checkpoint_count INTEGER NOT NULL,
  PRIMARY KEY (source, native_id)
);
CREATE TABLE IF NOT EXISTS orchestrator_link_facts (
  source TEXT NOT NULL, native_id TEXT NOT NULL, idx INTEGER NOT NULL,
  status TEXT NOT NULL, provider_source TEXT, provider_native_id TEXT, reason TEXT,
  PRIMARY KEY (source, native_id, idx),
  FOREIGN KEY (source, native_id) REFERENCES orchestrator_threads(source, native_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_messages (
  source TEXT NOT NULL, native_id TEXT NOT NULL, idx INTEGER NOT NULL,
  role TEXT NOT NULL, text TEXT NOT NULL, timestamp_ms INTEGER,
  PRIMARY KEY (source, native_id, idx),
  FOREIGN KEY (source, native_id) REFERENCES orchestrator_threads(source, native_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_turns (
  source TEXT NOT NULL, native_id TEXT NOT NULL, idx INTEGER NOT NULL,
  turn_native_id TEXT NOT NULL, state TEXT, checkpoint_id TEXT, payload_json TEXT,
  PRIMARY KEY (source, native_id, idx),
  FOREIGN KEY (source, native_id) REFERENCES orchestrator_threads(source, native_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_activities (
  source TEXT NOT NULL, native_id TEXT NOT NULL, idx INTEGER NOT NULL,
  activity_native_id TEXT NOT NULL, kind TEXT, payload_json TEXT,
  PRIMARY KEY (source, native_id, idx),
  FOREIGN KEY (source, native_id) REFERENCES orchestrator_threads(source, native_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS orchestrator_links (
  orchestrator_source TEXT NOT NULL, orchestrator_native_id TEXT NOT NULL,
  session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  provider_source TEXT NOT NULL, provider_native_id TEXT NOT NULL,
  PRIMARY KEY (orchestrator_source, orchestrator_native_id),
  FOREIGN KEY (orchestrator_source, orchestrator_native_id) REFERENCES orchestrator_threads(source, native_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS orchestrator_links_session_idx ON orchestrator_links(session_id);
CREATE TABLE IF NOT EXISTS bookmarks (
  host         TEXT NOT NULL,
  source       TEXT NOT NULL,
  bookmark_ms INTEGER NOT NULL,
  PRIMARY KEY (host, source)
);
CREATE TABLE IF NOT EXISTS digest_cache (
  source       TEXT NOT NULL,
  native_id    TEXT NOT NULL,
  workday      TEXT NOT NULL,
  last_idx     INTEGER,
  title        TEXT NOT NULL,
  path         TEXT NOT NULL,
  PRIMARY KEY (source, native_id, path)
);
";

fn migrate_subagent_facts_per_host(conn: &Connection) -> Result<()> {
    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names.iter().any(|name| name == column))
    }
    if !has_column(conn, "subagent_parent_facts", "fact_host")? {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE subagent_parent_facts_new (
               fact_host TEXT NOT NULL, child_source TEXT NOT NULL,
               child_native_id TEXT NOT NULL, parent_native_id TEXT NOT NULL,
               description TEXT, agent_type TEXT, instructions TEXT, depth INTEGER,
               native_key TEXT, link_key TEXT,
               PRIMARY KEY (fact_host, child_source, child_native_id));
             INSERT INTO subagent_parent_facts_new
               SELECT '', child_source, child_native_id, parent_native_id, description,
                      agent_type, instructions, depth, native_key, link_key
               FROM subagent_parent_facts;
             DROP TABLE subagent_parent_facts;
             ALTER TABLE subagent_parent_facts_new RENAME TO subagent_parent_facts;
             COMMIT;",
        )?;
    }
    if !has_column(conn, "subagent_reference_facts", "fact_host")? {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE subagent_reference_facts_new (
               fact_host TEXT NOT NULL, parent_source TEXT NOT NULL,
               parent_native_id TEXT NOT NULL, idx INTEGER NOT NULL,
               child_native_id TEXT, child_native_key TEXT, link_key TEXT,
               description TEXT, agent_type TEXT, instructions TEXT, spawned_at_ms INTEGER,
               PRIMARY KEY (fact_host, parent_source, parent_native_id, idx));
             INSERT INTO subagent_reference_facts_new
               SELECT '', parent_source, parent_native_id, idx, child_native_id,
                      child_native_key, link_key, description, agent_type,
                      instructions, spawned_at_ms
               FROM subagent_reference_facts;
             DROP TABLE subagent_reference_facts;
             ALTER TABLE subagent_reference_facts_new RENAME TO subagent_reference_facts;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn migrate_session_winner(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, ty) in [
        ("winner_host", "TEXT"),
        ("winner_source_mtime_ms", "INTEGER"),
        ("winner_source_size", "INTEGER"),
        ("winner_native_meta_mtime_ms", "INTEGER"),
        ("winner_native_meta_size", "INTEGER"),
        ("winner_adapter_version", "INTEGER"),
    ] {
        if !columns.iter().any(|column| column == name) {
            conn.execute(&format!("ALTER TABLE sessions ADD COLUMN {name} {ty}"), [])?;
        }
    }
    Ok(())
}

fn migrate_inherited_context(conn: &Connection) -> Result<()> {
    for table in ["sessions", "session_projections"] {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        for (name, ty) in [
            ("inherited_context_source_id", "TEXT"),
            ("inherited_context_copied_item_count", "INTEGER"),
        ] {
            if !columns.iter().any(|column| column == name) {
                conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"), [])?;
            }
        }
    }
    Ok(())
}

fn migrate_message_kind(conn: &Connection) -> Result<()> {
    for table in ["messages", "projection_messages"] {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.iter().any(|column| column == "kind") {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN kind TEXT NOT NULL DEFAULT 'dialog'"),
                [],
            )?;
        }
    }
    Ok(())
}

fn migrate_transcript_facts(conn: &Connection) -> Result<()> {
    for table in ["sessions", "session_projections"] {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        for (name, ty) in [
            ("starting_model", "TEXT"),
            ("starting_reasoning_effort", "TEXT"),
            ("git_branch", "TEXT"),
            ("git_commit", "TEXT"),
            ("git_repository_url", "TEXT"),
            ("patched_files_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("referenced_files_json", "TEXT NOT NULL DEFAULT '[]'"),
        ] {
            if !columns.iter().any(|column| column == name) {
                conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"), [])?;
            }
        }
    }
    Ok(())
}

fn pragma_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(stmt
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?)
}

fn migrate_captured_at(conn: &Connection, captures_dir: Option<&Path>) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let mut added = false;
    for table in ["sessions", "session_projections"] {
        let columns = pragma_columns(&tx, table)?;
        if columns.is_empty() {
            continue;
        }
        if !columns.iter().any(|column| column == "captured_at_ms") {
            tx.execute(
                &format!(
                    "ALTER TABLE {table} ADD COLUMN captured_at_ms INTEGER NOT NULL DEFAULT 0"
                ),
                [],
            )?;
            added = true;
        }
    }
    if added && let Some(captures_dir) = captures_dir.filter(|path| path.is_dir()) {
        backfill_captured_at(&tx, captures_dir)?;
    }
    tx.commit()?;
    Ok(())
}

fn capture_times_by_identity(
    captures_dir: &Path,
) -> Result<BTreeMap<(String, String, String), i64>> {
    let mut paths = Vec::new();
    crate::adapters::collect_sidecars(captures_dir, &mut paths)?;
    let mut capture_times: BTreeMap<(String, String, String), i64> = BTreeMap::new();
    for path in paths {
        let Some(value) = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        else {
            continue;
        };
        let Some(source) = value.get("source").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(native_id) = value.get("nativeId").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(host) = value.get("host").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(captured_at_ms) = value.get("capturedAtMs").and_then(|value| value.as_i64())
        else {
            continue;
        };
        capture_times
            .entry((source.to_owned(), native_id.to_owned(), host.to_owned()))
            .and_modify(|current| *current = (*current).max(captured_at_ms))
            .or_insert(captured_at_ms);
    }
    Ok(capture_times)
}

fn backfill_captured_at(conn: &Connection, captures_dir: &Path) -> Result<()> {
    let capture_times = capture_times_by_identity(captures_dir)?;
    let session_columns = pragma_columns(conn, "sessions")?;
    if session_columns.iter().any(|column| column == "source")
        && session_columns.iter().any(|column| column == "native_id")
        && session_columns
            .iter()
            .any(|column| column == "captured_at_ms")
    {
        let sql = if session_columns.iter().any(|column| column == "winner_host") {
            "SELECT source, native_id, winner_host FROM sessions"
        } else {
            "SELECT source, native_id, NULL FROM sessions"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for (source, native_id, winner_host) in rows {
            let host = winner_host.filter(|host| !host.is_empty()).or_else(|| {
                conn.query_row(
                    "SELECT host FROM session_hosts
                     JOIN sessions ON sessions.id = session_hosts.session_id
                     WHERE sessions.source = ?1 AND sessions.native_id = ?2
                     ORDER BY host LIMIT 1",
                    params![source, native_id],
                    |row| row.get(0),
                )
                .ok()
            });
            let Some(host) = host else {
                continue;
            };
            if let Some(captured_at_ms) =
                capture_times.get(&(source.clone(), native_id.clone(), host))
            {
                conn.execute(
                    "UPDATE sessions SET captured_at_ms = ?1 WHERE source = ?2 AND native_id = ?3",
                    params![captured_at_ms, source, native_id],
                )?;
            }
        }
    }

    let projection_columns = pragma_columns(conn, "session_projections")?;
    if ["source", "native_id", "host", "captured_at_ms"]
        .iter()
        .all(|column| projection_columns.iter().any(|name| name == column))
    {
        let mut stmt = conn.prepare("SELECT source, native_id, host FROM session_projections")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for (source, native_id, host) in rows {
            if let Some(captured_at_ms) =
                capture_times.get(&(source.clone(), native_id.clone(), host.clone()))
            {
                conn.execute(
                    "UPDATE session_projections
                     SET captured_at_ms = ?1
                     WHERE source = ?2 AND native_id = ?3 AND host = ?4",
                    params![captured_at_ms, source, native_id, host],
                )?;
            }
        }
    }
    Ok(())
}

fn migrate_session_projections(conn: &Connection) -> Result<()> {
    // Schema v4 retained only the elected canonical projection. Seed that
    // winner into the new per-Host projection tables; all loser projections
    // self-fill the next time their settled Captures are extracted.
    conn.execute_batch(
        "INSERT OR IGNORE INTO session_projections
           (source, native_id, host, title, project, project_path, created_at_ms,
            updated_at_ms, message_count, trailing_actions, captured_at_ms,
            starting_model, starting_reasoning_effort, git_branch, git_commit,
            git_repository_url, patched_files_json, referenced_files_json,
            inherited_context_source_id, inherited_context_copied_item_count, has_dialog)
         SELECT source, native_id, winner_host, title, project, project_path,
                created_at_ms, updated_at_ms, message_count, trailing_actions,
                captured_at_ms,
                starting_model, starting_reasoning_effort, git_branch, git_commit,
                git_repository_url, patched_files_json, referenced_files_json,
                inherited_context_source_id, inherited_context_copied_item_count, has_dialog
         FROM sessions WHERE winner_host IS NOT NULL;
         INSERT OR IGNORE INTO projection_messages
           (source, native_id, host, idx, role, kind, text, timestamp_ms, actions_before)
         SELECT s.source, s.native_id, s.winner_host, m.idx, m.role, m.kind, m.text,
                m.timestamp_ms, m.actions_before
         FROM sessions s JOIN messages m ON m.session_id = s.id
         WHERE s.winner_host IS NOT NULL;
         DELETE FROM session_revisions
         WHERE NOT EXISTS (
           SELECT 1 FROM session_projections p
           WHERE p.source = session_revisions.source
             AND p.native_id = session_revisions.native_id
             AND p.host = session_revisions.host
         );",
    )?;
    Ok(())
}

/// An open archive database.
pub struct Db {
    conn: Connection,
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureRevision, Db, SCHEMA_VERSION, compare_revisions, migrate_captured_at, pragma_columns,
    };
    use crate::adapters::{CaptureMeta, SessionKind};
    use crate::model::{
        Conversation, Message, MessageKind, OrchestratorActivity, OrchestratorLinkFact,
        OrchestratorLinkStatus, OrchestratorMessage, OrchestratorProjection, OrchestratorTurn,
        Provenance, Role, SubagentParent, SubagentReference,
    };

    pub(super) fn conversation(id: &str) -> Conversation {
        Conversation {
            provenance: Provenance {
                source: "claude".into(),
                host: "host".into(),
                native_id: id.into(),
                native_path: format!("/{id}.jsonl"),
            },
            title: None,
            project_path: None,
            native_project: None,
            created_at_ms: None,
            updated_at_ms: None,
            messages: Vec::new(),
            transcript_facts: Default::default(),
            subagent_parent: None,
            inherited_context: None,
            subagent_references: Vec::new(),
            branches: Vec::new(),
            subagent_links: Vec::new(),
            trailing_actions: 0,
        }
    }

    #[test]
    fn populated_legacy_database_adds_capture_time_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY);
             CREATE TABLE session_projections (id INTEGER PRIMARY KEY);
             INSERT INTO sessions DEFAULT VALUES;",
        )
        .unwrap();
        migrate_captured_at(&conn, None).unwrap();
        for table in ["sessions", "session_projections"] {
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                columns.iter().any(|column| column == "captured_at_ms"),
                "{table} should gain captured_at_ms"
            );
        }
        let value: i64 = conn
            .query_row("SELECT captured_at_ms FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 0);
    }

    #[test]
    fn populated_legacy_database_backfills_capture_time_from_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let captures = directory.path().join("captures/host/claude");
        std::fs::create_dir_all(&captures).unwrap();
        let native_id = "11111111-1111-4111-8111-111111111111";
        std::fs::write(
            captures.join(format!("{native_id}.meta.json")),
            format!(
                r#"{{"source":"claude","nativeId":"{native_id}","host":"host","capturedAtMs":123456789}}"#
            ),
        )
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE sessions (
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, native_id TEXT NOT NULL,
               winner_host TEXT, UNIQUE(source, native_id));
             CREATE TABLE session_projections (
               source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL,
               has_dialog INTEGER NOT NULL, PRIMARY KEY(source, native_id, host));
             INSERT INTO sessions (source, native_id, winner_host)
               VALUES ('claude', '{native_id}', 'host');
             INSERT INTO session_projections (source, native_id, host, has_dialog)
               VALUES ('claude', '{native_id}', 'host', 1);"
        ))
        .unwrap();
        migrate_captured_at(&conn, Some(&directory.path().join("captures"))).unwrap();
        let session: i64 = conn
            .query_row("SELECT captured_at_ms FROM sessions", [], |row| row.get(0))
            .unwrap();
        let projection: i64 = conn
            .query_row(
                "SELECT captured_at_ms FROM session_projections",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session, 123456789);
        assert_eq!(projection, 123456789);
    }

    #[test]
    fn capture_time_backfill_uses_nested_file_capture_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let native_id = "parent/subagents/agent-child";
        let captures = directory
            .path()
            .join("captures/host/claude/parent/subagents");
        std::fs::create_dir_all(&captures).unwrap();
        std::fs::write(
            captures.join("agent-child.meta.json"),
            format!(
                r#"{{"source":"claude","nativeId":"{native_id}","host":"host","capturedAtMs":123456789}}"#
            ),
        )
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE sessions (
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, native_id TEXT NOT NULL,
               winner_host TEXT, UNIQUE(source, native_id));
             CREATE TABLE session_projections (
               source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL,
               has_dialog INTEGER NOT NULL, PRIMARY KEY(source, native_id, host));
             INSERT INTO sessions (source, native_id, winner_host)
               VALUES ('claude', '{native_id}', 'host');
             INSERT INTO session_projections (source, native_id, host, has_dialog)
               VALUES ('claude', '{native_id}', 'host', 1);"
        ))
        .unwrap();

        migrate_captured_at(&conn, Some(&directory.path().join("captures"))).unwrap();

        assert_eq!(
            conn.query_row("SELECT captured_at_ms FROM sessions", [], |row| row
                .get::<_, i64>(0)),
            Ok(123456789),
        );
        assert_eq!(
            conn.query_row(
                "SELECT captured_at_ms FROM session_projections",
                [],
                |row| { row.get::<_, i64>(0) }
            ),
            Ok(123456789),
        );
    }

    #[test]
    fn failed_capture_time_backfill_rolls_back_and_retries() {
        let directory = tempfile::tempdir().unwrap();
        let captures = directory.path().join("captures/host/claude");
        std::fs::create_dir_all(&captures).unwrap();
        let native_id = "11111111-1111-4111-8111-111111111111";
        std::fs::write(
            captures.join(format!("{native_id}.meta.json")),
            format!(
                r#"{{"source":"claude","nativeId":"{native_id}","host":"host","capturedAtMs":123456789}}"#
            ),
        )
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE sessions (
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, native_id TEXT NOT NULL,
               winner_host TEXT, UNIQUE(source, native_id));
             CREATE TABLE session_projections (
               source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL,
               has_dialog INTEGER NOT NULL, PRIMARY KEY(source, native_id, host));
             INSERT INTO sessions (source, native_id, winner_host)
               VALUES ('claude', '{native_id}', 'host');
             INSERT INTO session_projections (source, native_id, host, has_dialog)
               VALUES ('claude', '{native_id}', 'host', 1);
             CREATE TRIGGER reject_capture_time
               BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT, 'stop migration'); END;"
        ))
        .unwrap();

        assert!(migrate_captured_at(&conn, Some(&directory.path().join("captures"))).is_err());
        for table in ["sessions", "session_projections"] {
            assert!(
                !pragma_columns(&conn, table)
                    .unwrap()
                    .iter()
                    .any(|column| column == "captured_at_ms"),
                "{table} schema change should roll back"
            );
        }

        conn.execute("DROP TRIGGER reject_capture_time", [])
            .unwrap();
        migrate_captured_at(&conn, Some(&directory.path().join("captures"))).unwrap();
        assert_eq!(
            conn.query_row("SELECT captured_at_ms FROM sessions", [], |row| row
                .get::<_, i64>(0)),
            Ok(123456789),
        );
        assert_eq!(
            conn.query_row(
                "SELECT captured_at_ms FROM session_projections",
                [],
                |row| { row.get::<_, i64>(0) }
            ),
            Ok(123456789),
        );
    }

    pub(super) fn meta(conv: &Conversation, mtime: i64, size: u64) -> CaptureMeta {
        CaptureMeta {
            repository_resolution: None,
            schema_version: 2,
            source: conv.provenance.source.clone(),
            native_id: conv.provenance.native_id.clone(),
            host: conv.provenance.host.clone(),
            native_path: conv.provenance.native_path.clone(),
            project_dir: None,
            kind: SessionKind::Main,
            source_mtime_ms: mtime,
            source_size: size,
            source_files: Vec::new(),
            native_meta_mtime_ms: None,
            native_meta_size: None,
            captured_at_ms: mtime,
            adapter_version: 1,
            superseded_native_ids: Vec::new(),
            needs_retry: false,
            native_meta: None,
            native_birth_time_ms: None,
            goal_attachments: Vec::new(),
        }
    }

    pub(super) fn orchestrator(
        thread_id: &str,
        host: &str,
        revision: i64,
    ) -> OrchestratorProjection {
        OrchestratorProjection {
            repository_url: None,
            source: "t3".into(),
            native_id: thread_id.into(),
            host: host.into(),
            native_path: "/store/state.sqlite".into(),
            logical_revision_ms: revision,
            discriminator: revision as u64,
            adapter_version: 1,
            title: Some(format!("Thread {thread_id}")),
            project_id: Some("project-1".into()),
            project_title: Some("Project".into()),
            workspace_root: Some("/work/project".into()),
            provider_name: Some("claudeAgent".into()),
            provider_instance_id: Some("instance-1".into()),
            model: Some("claude-fable-5".into()),
            runtime_mode: Some("local".into()),
            interaction_mode: Some("default".into()),
            branch: Some("main".into()),
            worktree: None,
            created_at_ms: Some(1),
            updated_at_ms: Some(revision),
            archived: false,
            pinned: true,
            settled: false,
            linked_pull_request: None,
            checkpoint_count: 1,
            link_facts: vec![OrchestratorLinkFact {
                status: OrchestratorLinkStatus::Candidate,
                provider_source: Some("claude".into()),
                provider_native_id: Some("provider-1".into()),
                reason: None,
            }],
            messages: vec![OrchestratorMessage {
                role: "user".into(),
                text: "hello".into(),
                timestamp_ms: Some(2),
            }],
            turns: vec![OrchestratorTurn {
                native_id: "turn-1".into(),
                state: Some("completed".into()),
                checkpoint_id: Some("checkpoint-1".into()),
                payload_json: Some("{\"future\":true}".into()),
            }],
            activities: vec![OrchestratorActivity {
                native_id: "activity-1".into(),
                kind: Some("future-kind".into()),
                payload_json: Some("{\"unknown\":true}".into()),
            }],
        }
    }

    #[test]
    fn richer_host_projection_wins_independent_of_ingest_order() {
        let mut rich = conversation("shared");
        rich.provenance.host = "z-rich".into();
        rich.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "complete dialog".into(),
            timestamp_ms: Some(10),
            actions_before: 0,
        });
        let rich_meta = meta(&rich, 10, 1_000);
        let mut stale = conversation("shared");
        stale.provenance.host = "a-stale".into();
        stale.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "truncated".into(),
            timestamp_ms: Some(20),
            actions_before: 0,
        });
        let stale_meta = meta(&stale, 20, 100);

        let mut dumps = Vec::new();
        for rich_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
            if rich_first {
                db.upsert_conversation_from_capture(&rich, &rich_meta)
                    .unwrap();
                db.upsert_conversation_from_capture(&stale, &stale_meta)
                    .unwrap();
            } else {
                db.upsert_conversation_from_capture(&stale, &stale_meta)
                    .unwrap();
                db.upsert_conversation_from_capture(&rich, &rich_meta)
                    .unwrap();
            }
            let session = db.sessions_with_dialog().unwrap().remove(0);
            assert_eq!(session.winner_host.as_deref(), Some("z-rich"));
            assert_eq!(session.hosts, ["a-stale", "z-rich"]);
            assert_eq!(db.messages(session.id).unwrap()[0].text, "complete dialog");
            dumps.push(db.content_dump().unwrap());
        }
        assert_eq!(dumps[0], dumps[1]);
    }

    #[test]
    fn nonwinning_nonextractable_host_cannot_erase_the_elected_projection() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        let mut winner = conversation("shared");
        winner.provenance.host = "winner".into();
        winner.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "elected dialog".into(),
            timestamp_ms: Some(10),
            actions_before: 0,
        });
        let winner_meta = meta(&winner, 10, 1_000);
        let mut loser = conversation("shared");
        loser.provenance.host = "loser".into();
        let loser_meta = meta(&loser, 20, 100);

        db.upsert_conversation_from_capture(&winner, &winner_meta)
            .unwrap();
        db.upsert_conversation_from_capture(&loser, &loser_meta)
            .unwrap();
        db.upsert_nonextractable_from_capture(&loser_meta).unwrap();

        let session = db.sessions_with_dialog().unwrap().remove(0);
        assert_eq!(session.winner_host.as_deref(), Some("winner"));
        assert_eq!(db.messages(session.id).unwrap()[0].text, "elected dialog");
    }

    #[test]
    fn revision_order_is_source_aware_and_host_ties_are_stable() {
        let revision = |host: &str, mtime, size| CaptureRevision {
            host: host.into(),
            source_mtime_ms: mtime,
            source_size: size,
            native_meta_mtime_ms: None,
            native_meta_size: None,
            adapter_version: 1,
        };
        let older_large = revision("z", 10, 1_000);
        let newer_small = revision("a", 20, 100);
        assert!(compare_revisions("claude", &older_large, &newer_small).is_gt());
        assert!(compare_revisions("cursor", &newer_small, &older_large).is_gt());
        assert!(compare_revisions("t3", &newer_small, &older_large).is_gt());
        assert!(compare_revisions("t3", &older_large, &newer_small).is_lt());
        assert!(
            compare_revisions("claude", &revision("a", 10, 10), &revision("z", 10, 10)).is_gt()
        );
    }

    #[test]
    fn settled_same_host_projection_replaces_stale_database_revision() {
        let mut stale = conversation("same");
        stale.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "database-only stale projection".into(),
            timestamp_ms: Some(100),
            actions_before: 0,
        });
        let stale_meta = meta(&stale, 100, 1_000);
        let mut settled = conversation("same");
        settled.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "settled Capture".into(),
            timestamp_ms: Some(10),
            actions_before: 0,
        });
        let settled_meta = meta(&settled, 10, 100);

        let mut dumps = Vec::new();
        for include_stale in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
            if include_stale {
                db.upsert_conversation_from_capture(&stale, &stale_meta)
                    .unwrap();
            }
            db.upsert_conversation_from_capture(&settled, &settled_meta)
                .unwrap();
            let session = db.sessions_with_dialog().unwrap().remove(0);
            assert_eq!(db.messages(session.id).unwrap()[0].text, "settled Capture");
            dumps.push(db.content_dump().unwrap());
        }
        assert_eq!(dumps[0], dumps[1]);
    }

    #[test]
    fn resolution_is_independent_of_ingest_order_and_keeps_orphan_facts() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        let mut child = conversation("parent/subagents/workflows/wf_1/agent-a1");
        child.subagent_parent = Some(SubagentParent {
            parent_native_id: "parent".into(),
            description: Some("Explore".into()),
            agent_type: Some("Explore".into()),
            instructions: Some("Map it".into()),
            depth: Some(1),
            native_key: Some("a1".into()),
            link_key: Some("toolu_1".into()),
        });
        db.upsert_conversation(&child).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 0);
        assert!(db.sessions_with_dialog().unwrap().is_empty());

        let mut parent = conversation("parent");
        parent.subagent_references.push(SubagentReference {
            child_native_id: None,
            child_native_key: Some("a1".into()),
            link_key: Some("toolu_1".into()),
            description: Some("Map codebase".into()),
            agent_type: None,
            instructions: Some("Map it".into()),
            spawned_at_ms: Some(42),
        });
        let exact = SubagentReference {
            child_native_id: Some(child.provenance.native_id.clone()),
            child_native_key: None,
            link_key: None,
            description: Some("Exact child".into()),
            agent_type: None,
            instructions: None,
            spawned_at_ms: Some(41),
        };
        parent.subagent_references.push(exact.clone());
        parent.subagent_references.push(exact); // identical facts dedupe
        db.upsert_conversation(&parent).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        let links = db.subagent_links("claude", "parent").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link.child_native_id, child.provenance.native_id);
        assert_eq!(links[0].link.description.as_deref(), Some("Exact child"));
        assert_eq!(links[0].link.spawned_at_ms, Some(41));

        // A later, stale copy from another Host replaces only that Host's
        // facts; it cannot erase the richer facts already contributed.
        let mut stale_child = child.clone();
        stale_child.provenance.host = "stale-host".into();
        stale_child.subagent_parent.as_mut().unwrap().description = None;
        stale_child.subagent_parent.as_mut().unwrap().agent_type = None;
        stale_child.subagent_parent.as_mut().unwrap().link_key = None;
        db.upsert_conversation(&stale_child).unwrap();
        let mut stale_parent = conversation("parent");
        stale_parent.provenance.host = "stale-host".into();
        db.upsert_conversation(&stale_parent).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        let link = db
            .subagent_links("claude", "parent")
            .unwrap()
            .remove(0)
            .link;
        assert_eq!(link.description.as_deref(), Some("Exact child"));
        assert_eq!(link.agent_type.as_deref(), Some("Explore"));
    }

    #[test]
    fn legacy_facts_remain_fallback_until_named_projection_replaces_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, native_id TEXT NOT NULL,
               title TEXT, project TEXT, project_path TEXT, created_at_ms INTEGER,
               updated_at_ms INTEGER, message_count INTEGER NOT NULL DEFAULT 0,
               trailing_actions INTEGER NOT NULL DEFAULT 0,
               captured_at_ms INTEGER NOT NULL DEFAULT 1,
               has_dialog INTEGER NOT NULL DEFAULT 0, UNIQUE(source, native_id));
             CREATE TABLE messages (
               session_id INTEGER NOT NULL, idx INTEGER NOT NULL, role TEXT NOT NULL,
               text TEXT NOT NULL, timestamp_ms INTEGER, actions_before INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(session_id, idx));
             CREATE TABLE subagent_parent_facts (
               child_source TEXT NOT NULL, child_native_id TEXT NOT NULL,
               parent_native_id TEXT NOT NULL, description TEXT, agent_type TEXT,
               instructions TEXT, depth INTEGER, native_key TEXT, link_key TEXT,
               PRIMARY KEY (child_source, child_native_id));
             CREATE TABLE subagent_reference_facts (
               parent_source TEXT NOT NULL, parent_native_id TEXT NOT NULL,
               idx INTEGER NOT NULL, child_native_id TEXT, child_native_key TEXT,
               link_key TEXT, description TEXT, agent_type TEXT, instructions TEXT,
               spawned_at_ms INTEGER,
               PRIMARY KEY (parent_source, parent_native_id, idx));
             INSERT INTO subagent_parent_facts
               VALUES ('claude', 'child', 'parent', 'legacy name', 'Explore', NULL, 1, 'a1', NULL);
             INSERT INTO subagent_reference_facts
               VALUES ('claude', 'parent', 0, 'child', NULL, NULL,
                       'legacy name', 'Explore', NULL, 42);
             INSERT INTO sessions (id, source, native_id, message_count, has_dialog)
               VALUES (1, 'claude', 'parent', 1, 1), (2, 'claude', 'child', 1, 1);
             INSERT INTO messages VALUES (1, 0, 'user', 'parent dialog', 1, 0);
             INSERT INTO messages VALUES (2, 0, 'user', 'child dialog', 2, 0);",
        )
        .unwrap();
        drop(conn);

        let mut db = Db::open(&path).unwrap();
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |row| { row.get::<_, String>(0) }
                )
                .unwrap(),
            SCHEMA_VERSION.to_string()
        );
        assert_eq!(db.messages(1).unwrap()[0].text, "parent dialog");
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        assert_eq!(db.subagent_links("claude", "parent").unwrap().len(), 1);
        let child_row = db
            .sessions_with_dialog()
            .unwrap()
            .into_iter()
            .find(|session| session.native_id == "child")
            .unwrap();
        assert!(child_row.subagent_parent.is_some());
        assert!(
            db.content_dump()
                .unwrap()
                .contains("subagent-parent host= claude")
        );

        let mut child = conversation("child");
        child.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "named child".into(),
            timestamp_ms: Some(2),
            actions_before: 0,
        });
        child.subagent_parent = Some(SubagentParent {
            parent_native_id: "parent".into(),
            description: Some("named".into()),
            agent_type: Some("Explore".into()),
            instructions: None,
            depth: Some(1),
            native_key: Some("a1".into()),
            link_key: None,
        });
        let mut parent = conversation("parent");
        parent.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "named parent".into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        });
        parent.subagent_references.push(SubagentReference {
            child_native_id: Some("child".into()),
            child_native_key: None,
            link_key: None,
            description: Some("named".into()),
            agent_type: Some("Explore".into()),
            instructions: None,
            spawned_at_ms: Some(42),
        });
        db.upsert_conversation(&child).unwrap();
        db.upsert_conversation(&parent).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        let dump = db.content_dump().unwrap();
        assert!(!dump.contains("subagent-parent host= claude"));
        assert!(!dump.contains("subagent-ref host= claude"));
    }

    #[test]
    fn schema_v3_named_facts_resolve_deterministically_before_winner_population() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, native_id TEXT NOT NULL,
               title TEXT, project TEXT, project_path TEXT, created_at_ms INTEGER,
               updated_at_ms INTEGER, message_count INTEGER NOT NULL DEFAULT 0,
               trailing_actions INTEGER NOT NULL DEFAULT 0,
               captured_at_ms INTEGER NOT NULL DEFAULT 1,
               has_dialog INTEGER NOT NULL DEFAULT 0, UNIQUE(source, native_id));
             CREATE TABLE messages (
               session_id INTEGER NOT NULL, idx INTEGER NOT NULL, role TEXT NOT NULL,
               text TEXT NOT NULL, timestamp_ms INTEGER, actions_before INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(session_id, idx));
             CREATE TABLE subagent_parent_facts (
               fact_host TEXT NOT NULL, child_source TEXT NOT NULL,
               child_native_id TEXT NOT NULL, parent_native_id TEXT NOT NULL,
               description TEXT, agent_type TEXT, instructions TEXT, depth INTEGER,
               native_key TEXT, link_key TEXT,
               PRIMARY KEY(fact_host, child_source, child_native_id));
             CREATE TABLE subagent_reference_facts (
               fact_host TEXT NOT NULL, parent_source TEXT NOT NULL,
               parent_native_id TEXT NOT NULL, idx INTEGER NOT NULL,
               child_native_id TEXT, child_native_key TEXT, link_key TEXT,
               description TEXT, agent_type TEXT, instructions TEXT, spawned_at_ms INTEGER,
               PRIMARY KEY(fact_host, parent_source, parent_native_id, idx));
             INSERT INTO sessions (id, source, native_id, message_count, has_dialog)
               VALUES (1, 'claude', 'parent', 1, 1), (2, 'claude', 'child', 1, 1);
             INSERT INTO messages VALUES (1, 0, 'user', 'parent dialog', 1, 0);
             INSERT INTO messages VALUES (2, 0, 'user', 'child dialog', 2, 0);
             INSERT INTO subagent_parent_facts
               VALUES ('host-a', 'claude', 'child', 'parent', NULL, NULL,
                       NULL, NULL, 'a1', NULL);
             INSERT INTO subagent_parent_facts
               VALUES ('host-b', 'claude', 'child', 'parent', 'rich child', 'Explore',
                       'instructions', 1, 'a1', 'toolu_1');
             INSERT INTO subagent_reference_facts
               VALUES ('host-a', 'claude', 'parent', 0, 'child', NULL, NULL,
                       'parent reference', 'Explore', NULL, 42);",
        )
        .unwrap();
        drop(conn);

        let mut db = Db::open(&path).unwrap();
        let child = db
            .sessions_with_dialog()
            .unwrap()
            .into_iter()
            .find(|session| session.native_id == "child")
            .unwrap();
        assert_eq!(
            child.subagent_parent.unwrap().description.as_deref(),
            Some("rich child")
        );
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        let link = db.subagent_links("claude", "parent").unwrap().remove(0);
        assert_eq!(link.link.description.as_deref(), Some("parent reference"));
        assert_eq!(link.link.agent_type.as_deref(), Some("Explore"));
    }

    #[test]
    fn orchestrator_winner_is_complete_and_stale_loser_cannot_mix_fields() {
        let mut winner = orchestrator("thread-1", "z-winner", 20);
        winner.title = Some("winning title".into());
        let mut loser = orchestrator("thread-1", "a-loser", 10);
        loser.title = Some("stale title".into());
        loser.project_title = Some("stale project".into());
        loser.messages[0].text = "stale dialog".into();

        let mut dumps = Vec::new();
        for winner_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
            let ordered = if winner_first {
                [&winner, &loser]
            } else {
                [&loser, &winner]
            };
            for projection in ordered {
                db.upsert_orchestrator_projection(projection).unwrap();
            }
            let row = db.orchestrator_thread("t3", "thread-1").unwrap().unwrap();
            assert_eq!(row.winner_host, "z-winner");
            assert_eq!(row.title.as_deref(), Some("winning title"));
            assert_eq!(row.project_title.as_deref(), Some("Project"));
            assert_eq!(
                db.orchestrator_messages("t3", "thread-1").unwrap()[0].text,
                "hello"
            );
            dumps.push(db.content_dump().unwrap());
        }
        assert_eq!(dumps[0], dumps[1]);
    }

    #[test]
    fn orchestrator_election_within_generation_orders_revision_discriminator_adapter_then_host() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        let mut a = orchestrator("thread-1", "z-host", 10);
        a.discriminator = 10;
        a.adapter_version = 9;
        let mut b = orchestrator("thread-1", "y-host", 10);
        b.discriminator = 11;
        b.adapter_version = crate::adapters::t3::V2_ADAPTER_VERSION;
        let mut c = orchestrator("thread-1", "a-host", 10);
        c.discriminator = 11;
        c.adapter_version = crate::adapters::t3::V2_ADAPTER_VERSION;
        db.upsert_orchestrator_projection(&a).unwrap();
        db.upsert_orchestrator_projection(&b).unwrap();
        db.upsert_orchestrator_projection(&c).unwrap();
        assert_eq!(
            db.orchestrator_thread("t3", "thread-1")
                .unwrap()
                .unwrap()
                .winner_host,
            "a-host"
        );
    }

    #[test]
    fn exact_orchestrator_link_recovers_then_real_capture_supersedes() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        let mut projection = orchestrator("thread-1", "host", 10);
        projection.messages.push(OrchestratorMessage {
            role: "assistant".into(),
            text: "world".into(),
            timestamp_ms: Some(3),
        });
        db.upsert_orchestrator_projection(&projection).unwrap();
        assert_eq!(db.resolve_orchestrator_links().unwrap(), 1);
        let recovered = db
            .lookup_session_by_orchestrator_id("t3", "thread-1")
            .unwrap()
            .unwrap();
        let row = db.session_row(recovered.session_id).unwrap().unwrap();
        assert_eq!(row.source, "claude");
        assert_eq!(row.project.as_deref(), Some("Project"));
        assert_eq!(row.hosts, ["host"]);
        assert_eq!(db.messages(row.id).unwrap().len(), 2);
        assert_eq!(
            row.transcript_facts.recovered_via.unwrap().thread_id,
            "thread-1"
        );
        assert!(db.session_native_path(row.id, "host").unwrap().is_none());

        let mut provider = conversation("provider-1");
        // Physical provider order may regress in timestamp when a queued
        // prompt record is appended after the assistant record it preceded.
        provider.messages.push(Message {
            role: Role::Assistant,
            kind: MessageKind::Dialog,
            text: "world".into(),
            timestamp_ms: Some(3),
            actions_before: 0,
        });
        provider.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "hello".into(),
            timestamp_ms: Some(2),
            actions_before: 0,
        });
        db.upsert_conversation(&provider).unwrap();
        assert_eq!(db.resolve_orchestrator_links().unwrap(), 1);
        let lookup = db
            .lookup_session_by_orchestrator_id("t3", "thread-1")
            .unwrap()
            .unwrap();
        assert_eq!(lookup.session_source, "claude");
        assert_eq!(lookup.session_native_id, "provider-1");
        assert_eq!(lookup.orchestrator_native_id, "thread-1");
        assert!(
            db.session_row(lookup.session_id)
                .unwrap()
                .unwrap()
                .transcript_facts
                .recovered_via
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(db.messages(lookup.session_id).unwrap()).unwrap(),
            serde_json::to_value(&provider.messages).unwrap()
        );
        let augmentations = db.augmentations_for_session(lookup.session_id).unwrap();
        assert_eq!(augmentations.len(), 1);
        assert_eq!(augmentations[0].thread_id, "thread-1");
        assert_eq!(
            augmentations[0].provider_name.as_deref(),
            Some("claudeAgent")
        );
        let reports = db.orchestrator_fidelity_reports().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].orchestrator_user_messages, 1);
        assert_eq!(reports[0].provider_user_messages, 1);
        assert_eq!(reports[0].orchestrator_assistant_messages, 1);
        assert_eq!(reports[0].provider_assistant_messages, 1);
        assert!(reports[0].ordered_role_text_equal);
    }

    #[test]
    fn ambiguous_winning_candidates_and_unsupported_facts_never_link() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        for id in ["provider-1", "provider-2"] {
            db.upsert_conversation(&conversation(id)).unwrap();
        }
        let mut ambiguous = orchestrator("ambiguous", "host", 10);
        ambiguous.link_facts.push(OrchestratorLinkFact {
            status: OrchestratorLinkStatus::Candidate,
            provider_source: Some("claude".into()),
            provider_native_id: Some("provider-2".into()),
            reason: None,
        });
        db.upsert_orchestrator_projection(&ambiguous).unwrap();
        let mut unsupported = orchestrator("unsupported", "host", 10);
        unsupported.link_facts = vec![OrchestratorLinkFact {
            status: OrchestratorLinkStatus::Unsupported,
            provider_source: None,
            provider_native_id: None,
            reason: Some("unsupported provider cursor".into()),
        }];
        db.upsert_orchestrator_projection(&unsupported).unwrap();

        assert_eq!(db.resolve_orchestrator_links().unwrap(), 0);
        assert!(
            db.lookup_session_by_orchestrator_id("t3", "ambiguous")
                .unwrap()
                .is_none()
        );
        assert!(
            db.lookup_session_by_orchestrator_id("t3", "unsupported")
                .unwrap()
                .is_none()
        );
        let dump = db.content_dump().unwrap();
        assert!(dump.contains("unsupported provider cursor"));
        assert!(dump.contains("orchestrator-message"));
        assert!(dump.contains("orchestrator-turn"));
        assert!(dump.contains("orchestrator-activity"));
    }

    #[test]
    fn exact_codex_orchestrator_candidate_resolves_without_source_special_case() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        let mut provider = conversation("codex-thread");
        provider.provenance.source = "codex".into();
        db.upsert_conversation(&provider).unwrap();
        let mut projection = orchestrator("t3-thread", "host", 10);
        projection.provider_name = Some("codex".into());
        projection.link_facts = vec![OrchestratorLinkFact {
            status: OrchestratorLinkStatus::Candidate,
            provider_source: Some("codex".into()),
            provider_native_id: Some("codex-thread".into()),
            reason: None,
        }];
        db.upsert_orchestrator_projection(&projection).unwrap();

        assert_eq!(db.resolve_orchestrator_links().unwrap(), 1);
        let lookup = db
            .lookup_session_by_orchestrator_id("t3", "t3-thread")
            .unwrap()
            .unwrap();
        assert_eq!(lookup.session_source, "codex");
        assert_eq!(lookup.session_native_id, "codex-thread");
    }

    #[test]
    fn multiple_orchestrator_threads_can_augment_one_provider_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("archive.db")).unwrap();
        db.upsert_conversation(&conversation("provider-1")).unwrap();
        db.upsert_orchestrator_projection(&orchestrator("thread-b", "host", 10))
            .unwrap();
        db.upsert_orchestrator_projection(&orchestrator("thread-a", "host", 10))
            .unwrap();
        assert_eq!(db.resolve_orchestrator_links().unwrap(), 2);
        let lookup = db
            .lookup_session_by_orchestrator_id("t3", "thread-a")
            .unwrap()
            .unwrap();
        let augmentations = db.augmentations_for_session(lookup.session_id).unwrap();
        assert_eq!(
            augmentations
                .iter()
                .map(|a| a.thread_id.as_str())
                .collect::<Vec<_>>(),
            ["thread-a", "thread-b"]
        );
    }
}

/// One session row as needed by Render.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub repository_resolution: Option<crate::repository_resolution::Fact>,
    pub id: i64,
    pub source: String,
    pub native_id: String,
    pub title: Option<String>,
    pub project: Option<String>,
    pub project_path: Option<String>,
    pub native_project: Option<NativeProject>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub message_count: i64,
    pub trailing_actions: i64,
    pub captured_at_ms: i64,
    pub transcript_facts: TranscriptFacts,
    pub inherited_context: Option<InheritedContext>,
    pub hosts: Vec<String>,
    pub winner_host: Option<String>,
    pub host_attribution: Option<crate::origins::HostAttribution>,
    pub subagent_parent: Option<SubagentParent>,
}

/// Map a `sessions` row selected with the canonical column order used by
/// `sessions_with_dialog` and `session_row`.
fn session_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let git = GitSnapshot {
        branch: row.get(12)?,
        commit: row.get(13)?,
        repository_url: row.get(14)?,
    };
    let inherited_context_source_id: Option<String> = row.get(17)?;
    let inherited_context_copied_item_count: Option<i64> = row.get(18)?;
    Ok(SessionRow {
        repository_resolution: None,
        id: row.get(0)?,
        source: row.get(1)?,
        native_id: row.get(2)?,
        title: row.get(3)?,
        project: row.get(4)?,
        project_path: row.get(5)?,
        native_project: None,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        message_count: row.get(8)?,
        trailing_actions: row.get(9)?,
        transcript_facts: TranscriptFacts {
            recovered_via: None,
            starting_model: row.get(10)?,
            starting_reasoning_effort: row.get(11)?,
            git: (!git.is_empty()).then_some(git),
            patched_files: decode_file_list(row.get(15)?, 15)?,
            referenced_files: decode_file_list(row.get(16)?, 16)?,
        },
        inherited_context: inherited_context_source_id.map(|source_native_id| InheritedContext {
            source_native_id,
            copied_item_count: inherited_context_copied_item_count.unwrap_or_default() as u32,
        }),
        hosts: Vec::new(),
        winner_host: row.get(19)?,
        host_attribution: None,
        captured_at_ms: row.get(20)?,
        subagent_parent: None,
    })
}

#[derive(Debug, Clone)]
pub struct SubagentLinkRow {
    pub child_session_id: i64,
    pub link: SubagentLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestCacheRow {
    pub source: String,
    pub native_id: String,
    /// Empty for `session.json`; otherwise the Session Day Workday.
    pub workday: String,
    pub last_idx: Option<usize>,
    pub title: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorThreadRow {
    pub source: String,
    pub native_id: String,
    pub winner_host: String,
    pub title: Option<String>,
    pub project_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorLookup {
    pub orchestrator_source: String,
    pub orchestrator_native_id: String,
    pub orchestrator_host: String,
    pub session_id: i64,
    pub session_source: String,
    pub session_native_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorFidelityReport {
    pub orchestrator_source: String,
    pub orchestrator_native_id: String,
    pub session_source: String,
    pub session_native_id: String,
    pub orchestrator_user_messages: usize,
    pub orchestrator_assistant_messages: usize,
    pub provider_user_messages: usize,
    pub provider_assistant_messages: usize,
    pub ordered_role_text_equal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorLinkDiagnostic {
    pub source: String,
    pub native_id: String,
    pub host: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CaptureRevision {
    host: String,
    source_mtime_ms: i64,
    source_size: u64,
    native_meta_mtime_ms: Option<i64>,
    native_meta_size: Option<u64>,
    adapter_version: u32,
}

fn compare_revisions(
    source: &str,
    candidate: &CaptureRevision,
    winner: &CaptureRevision,
) -> std::cmp::Ordering {
    compare_revision_fields(
        source,
        RevisionFields::from_revision(candidate),
        RevisionFields::from_revision(winner),
    )
}

/// Source-specific winner election shared by database projections and
/// capture-backed inspection. Larger means `candidate` wins.
pub(crate) fn compare_capture_revisions(
    source: &str,
    candidate: &CaptureMeta,
    winner: &CaptureMeta,
) -> std::cmp::Ordering {
    compare_revision_fields(
        source,
        RevisionFields::from_meta(candidate),
        RevisionFields::from_meta(winner),
    )
}

#[derive(Clone, Copy)]
struct RevisionFields<'a> {
    host: &'a str,
    mtime: i64,
    size: u64,
    meta_mtime: Option<i64>,
    meta_size: Option<u64>,
    adapter: u32,
}

impl<'a> RevisionFields<'a> {
    fn from_revision(revision: &'a CaptureRevision) -> Self {
        Self {
            host: &revision.host,
            mtime: revision.source_mtime_ms,
            size: revision.source_size,
            meta_mtime: revision.native_meta_mtime_ms,
            meta_size: revision.native_meta_size,
            adapter: revision.adapter_version,
        }
    }

    fn from_meta(meta: &'a CaptureMeta) -> Self {
        Self {
            host: &meta.host,
            mtime: meta.source_mtime_ms,
            size: meta.source_size,
            meta_mtime: meta.native_meta_mtime_ms,
            meta_size: meta.native_meta_size,
            adapter: meta.adapter_version,
        }
    }
}

fn compare_revision_fields(
    source: &str,
    candidate: RevisionFields<'_>,
    winner: RevisionFields<'_>,
) -> std::cmp::Ordering {
    let body = match capture_ordering(source) {
        CaptureOrdering::LogicalRevisionFirst => candidate
            .mtime
            .cmp(&winner.mtime)
            .then_with(|| candidate.size.cmp(&winner.size)),
        CaptureOrdering::AppendBodyFirst => candidate
            .size
            .cmp(&winner.size)
            .then_with(|| candidate.mtime.cmp(&winner.mtime)),
    };
    body.then_with(|| candidate.meta_mtime.cmp(&winner.meta_mtime))
        .then_with(|| candidate.meta_size.cmp(&winner.meta_size))
        .then_with(|| candidate.adapter.cmp(&winner.adapter))
        // The lexically smaller Host wins an otherwise exact tie.
        .then_with(|| winner.host.cmp(candidate.host))
}

fn decode_file_list(raw: String, column: usize) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

impl Db {
    fn repository_facts(
        &self,
    ) -> Result<BTreeMap<(String, String, String), crate::repository_resolution::Fact>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='repository_resolutions')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(BTreeMap::new());
        }
        let mut stmt = self
            .conn
            .prepare("SELECT host,source,native_id,fact FROM repository_resolutions")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .map(|row| {
            let (host, source, id, fact) = row?;
            Ok(((host, source, id), serde_json::from_str(&fact)?))
        })
        .collect()
    }
    pub(crate) fn index_repository_fact(&self, meta: &CaptureMeta) -> Result<()> {
        let Some(fact) = &meta.repository_resolution else {
            self.conn.execute(
                "DELETE FROM repository_resolutions WHERE host=?1 AND source=?2 AND native_id=?3",
                params![meta.host, meta.source, meta.native_id],
            )?;
            return Ok(());
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO repository_resolutions VALUES (?1,?2,?3,?4)",
            params![
                meta.host,
                meta.source,
                meta.native_id,
                serde_json::to_string(fact)?
            ],
        )?;
        Ok(())
    }
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening archive db at {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(branches::SCHEMA)?;
        conn.execute_batch(native_projects::SCHEMA)?;
        conn.execute_batch(render_state::SCHEMA)?;
        conn.execute_batch(host_pulls::SCHEMA)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS t3_repository_evidence (source TEXT, native_id TEXT, host TEXT, evidence TEXT, PRIMARY KEY(source,native_id,host))")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS repository_resolutions (host TEXT, source TEXT, native_id TEXT, fact TEXT, PRIMARY KEY(host,source,native_id))")?;
        let captures_dir = path.parent().map(|parent| parent.join("captures"));
        migrate_captured_at(&conn, captures_dir.as_deref())?;
        migrate_subagent_facts_per_host(&conn)?;
        migrate_session_winner(&conn)?;
        migrate_inherited_context(&conn)?;
        migrate_message_kind(&conn)?;
        migrate_transcript_facts(&conn)?;
        migrate_session_projections(&conn)?;
        conn.execute_batch(recovery::SCHEMA)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(Self { conn })
    }

    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening archive db read-only at {}", path.display()))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self { conn })
    }

    /// Test convenience wrapper. Production ingestion must use
    /// [`Self::upsert_conversation_from_capture`] so its real Capture
    /// revision participates in projection election.
    pub fn upsert_conversation(&mut self, conv: &Conversation) -> Result<i64> {
        let source_size = conv
            .messages
            .iter()
            .map(|message| message.text.len() as u64)
            .sum();
        let meta = CaptureMeta {
            repository_resolution: None,
            schema_version: 0,
            source: conv.provenance.source.clone(),
            native_id: conv.provenance.native_id.clone(),
            host: conv.provenance.host.clone(),
            native_path: conv.provenance.native_path.clone(),
            project_dir: None,
            kind: if conv.subagent_parent.is_some() {
                SessionKind::Subagent
            } else {
                SessionKind::Main
            },
            source_mtime_ms: conv.updated_at_ms.unwrap_or_default(),
            source_size,
            source_files: Vec::new(),
            native_meta_mtime_ms: None,
            native_meta_size: None,
            captured_at_ms: 0,
            adapter_version: 0,
            superseded_native_ids: Vec::new(),
            needs_retry: false,
            native_meta: None,
            native_birth_time_ms: None,
            goal_attachments: Vec::new(),
        };
        self.upsert_conversation_from_capture(conv, &meta)
    }

    /// Idempotently record one Host projection, keyed canonically by
    /// (Source, native id). All Hosts remain provenance, but only the elected
    /// Capture supplies canonical Session fields, Messages, and graph facts.
    pub fn upsert_conversation_from_capture(
        &mut self,
        conv: &Conversation,
        meta: &CaptureMeta,
    ) -> Result<i64> {
        anyhow::ensure!(
            meta.source == conv.provenance.source
                && meta.native_id == conv.provenance.native_id
                && meta.host == conv.provenance.host,
            "Capture metadata does not match extracted Conversation provenance"
        );
        let tx = self.conn.transaction()?;
        recovery::supersede(&tx, &meta.source, &meta.native_id)?;
        let project = conv
            .project_path
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let patched_files_json = serde_json::to_string(&conv.transcript_facts.patched_files)
            .context("serializing patched transcript files")?;
        let referenced_files_json = serde_json::to_string(&conv.transcript_facts.referenced_files)
            .context("serializing referenced transcript files")?;
        let git = conv.transcript_facts.git.as_ref();
        tx.execute(
            "INSERT INTO sessions
               (source, native_id, title, project, project_path,
                created_at_ms, updated_at_ms, message_count, trailing_actions,
                starting_model, starting_reasoning_effort, git_branch, git_commit,
                git_repository_url, patched_files_json, referenced_files_json, has_dialog,
                captured_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(source, native_id) DO NOTHING",
            params![
                conv.provenance.source,
                conv.provenance.native_id,
                conv.title,
                project,
                conv.project_path,
                conv.created_at_ms,
                conv.updated_at_ms,
                conv.messages.len() as i64,
                conv.trailing_actions as i64,
                conv.transcript_facts.starting_model,
                conv.transcript_facts.starting_reasoning_effort,
                git.and_then(|value| value.branch.as_deref()),
                git.and_then(|value| value.commit.as_deref()),
                git.and_then(|value| value.repository_url.as_deref()),
                patched_files_json,
                referenced_files_json,
                conv.has_dialog(),
                meta.captured_at_ms,
            ],
        )?;
        let session_id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE source = ?1 AND native_id = ?2",
            params![conv.provenance.source, conv.provenance.native_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO session_hosts (session_id, host, native_path) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, host) DO UPDATE SET native_path = excluded.native_path",
            params![
                session_id,
                conv.provenance.host,
                conv.provenance.native_path
            ],
        )?;
        tx.execute(
            "INSERT INTO session_revisions
               (source, native_id, host, source_mtime_ms, source_size,
                native_meta_mtime_ms, native_meta_size, adapter_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(source, native_id, host) DO UPDATE SET
               source_mtime_ms = excluded.source_mtime_ms,
               source_size = excluded.source_size,
               native_meta_mtime_ms = excluded.native_meta_mtime_ms,
               native_meta_size = excluded.native_meta_size,
               adapter_version = excluded.adapter_version",
            params![
                meta.source,
                meta.native_id,
                meta.host,
                meta.source_mtime_ms,
                meta.source_size,
                meta.native_meta_mtime_ms,
                meta.native_meta_size,
                meta.adapter_version,
            ],
        )?;
        tx.execute(
            "INSERT INTO session_projections
               (source, native_id, host, title, project, project_path, created_at_ms,
                updated_at_ms, message_count, trailing_actions,
                starting_model, starting_reasoning_effort, git_branch, git_commit,
                git_repository_url, patched_files_json, referenced_files_json,
                inherited_context_source_id, inherited_context_copied_item_count, has_dialog,
                captured_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             ON CONFLICT(source, native_id, host) DO UPDATE SET
               title = excluded.title, project = excluded.project,
               project_path = excluded.project_path, created_at_ms = excluded.created_at_ms,
               updated_at_ms = excluded.updated_at_ms,
               message_count = excluded.message_count,
               trailing_actions = excluded.trailing_actions,
               starting_model = excluded.starting_model,
               starting_reasoning_effort = excluded.starting_reasoning_effort,
               git_branch = excluded.git_branch, git_commit = excluded.git_commit,
               git_repository_url = excluded.git_repository_url,
               patched_files_json = excluded.patched_files_json,
               referenced_files_json = excluded.referenced_files_json,
               inherited_context_source_id = excluded.inherited_context_source_id,
               inherited_context_copied_item_count = excluded.inherited_context_copied_item_count,
               has_dialog = excluded.has_dialog,
               captured_at_ms = excluded.captured_at_ms",
            params![
                meta.source,
                meta.native_id,
                meta.host,
                conv.title,
                project,
                conv.project_path,
                conv.created_at_ms,
                conv.updated_at_ms,
                conv.messages.len() as i64,
                conv.trailing_actions as i64,
                conv.transcript_facts.starting_model,
                conv.transcript_facts.starting_reasoning_effort,
                git.and_then(|value| value.branch.as_deref()),
                git.and_then(|value| value.commit.as_deref()),
                git.and_then(|value| value.repository_url.as_deref()),
                patched_files_json,
                referenced_files_json,
                conv.inherited_context
                    .as_ref()
                    .map(|context| &context.source_native_id),
                conv.inherited_context
                    .as_ref()
                    .map(|context| i64::from(context.copied_item_count)),
                conv.has_dialog(),
                meta.captured_at_ms,
            ],
        )?;
        tx.execute(
            "DELETE FROM projection_messages WHERE source = ?1 AND native_id = ?2 AND host = ?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO projection_messages
                   (source, native_id, host, idx, role, kind, text, timestamp_ms, actions_before)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (idx, message) in conv.messages.iter().enumerate() {
                stmt.execute(params![
                    meta.source,
                    meta.native_id,
                    meta.host,
                    idx as i64,
                    message.role.as_str(),
                    message.kind.as_str(),
                    message.text,
                    message.timestamp_ms,
                    message.actions_before as i64,
                ])?;
            }
        }
        branches::replace(&tx, meta, &conv.branches)?;
        native_projects::replace(&tx, meta, conv.native_project.as_ref())?;
        // A named projection supersedes migration-only facts for this exact
        // identity. Other legacy identities remain usable without Store IO.
        tx.execute(
            "DELETE FROM subagent_parent_facts
             WHERE child_source = ?1 AND child_native_id = ?2
               AND fact_host IN ('', ?3)",
            params![meta.source, meta.native_id, meta.host],
        )?;
        if let Some(parent) = &conv.subagent_parent {
            tx.execute(
                "INSERT INTO subagent_parent_facts
                   (fact_host, child_source, child_native_id, parent_native_id, description,
                    agent_type, instructions, depth, native_key, link_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    conv.provenance.host,
                    conv.provenance.source,
                    conv.provenance.native_id,
                    parent.parent_native_id,
                    parent.description,
                    parent.agent_type,
                    parent.instructions,
                    parent.depth.map(i64::from),
                    parent.native_key,
                    parent.link_key,
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM subagent_reference_facts
             WHERE parent_source = ?1 AND parent_native_id = ?2
               AND fact_host IN ('', ?3)",
            params![meta.source, meta.native_id, meta.host],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO subagent_reference_facts
                   (fact_host, parent_source, parent_native_id, idx, child_native_id,
                    child_native_key, link_key, description, agent_type,
                    instructions, spawned_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for (idx, reference) in conv.subagent_references.iter().enumerate() {
                stmt.execute(params![
                    conv.provenance.host,
                    conv.provenance.source,
                    conv.provenance.native_id,
                    idx as i64,
                    reference.child_native_id,
                    reference.child_native_key,
                    reference.link_key,
                    reference.description,
                    reference.agent_type,
                    reference.instructions,
                    reference.spawned_at_ms,
                ])?;
            }
        }
        let winner = {
            let mut stmt = tx.prepare(
                "SELECT host, source_mtime_ms, source_size, native_meta_mtime_ms,
                        native_meta_size, adapter_version
                 FROM session_revisions WHERE source = ?1 AND native_id = ?2",
            )?;
            stmt.query_map(params![meta.source, meta.native_id], |row| {
                Ok(CaptureRevision {
                    host: row.get(0)?,
                    source_mtime_ms: row.get(1)?,
                    source_size: row.get(2)?,
                    native_meta_mtime_ms: row.get(3)?,
                    native_meta_size: row.get(4)?,
                    adapter_version: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max_by(|a, b| compare_revisions(&meta.source, a, b))
            .expect("just inserted a revision")
        };
        let winner_is_extractable: bool = tx.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM session_projections
               WHERE source = ?1 AND native_id = ?2 AND host = ?3
             )",
            params![meta.source, meta.native_id, winner.host],
            |row| row.get(0),
        )?;
        if !winner_is_extractable {
            tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
            tx.commit()?;
            return Ok(session_id);
        }
        tx.execute(
            "UPDATE sessions SET
               title = p.title, project = p.project, project_path = p.project_path,
               created_at_ms = p.created_at_ms, updated_at_ms = p.updated_at_ms,
               message_count = p.message_count, trailing_actions = p.trailing_actions,
               captured_at_ms = p.captured_at_ms,
               starting_model = p.starting_model,
               starting_reasoning_effort = p.starting_reasoning_effort,
               git_branch = p.git_branch, git_commit = p.git_commit,
               git_repository_url = p.git_repository_url,
               patched_files_json = p.patched_files_json,
               referenced_files_json = p.referenced_files_json,
               inherited_context_source_id = p.inherited_context_source_id,
               inherited_context_copied_item_count = p.inherited_context_copied_item_count,
               has_dialog = p.has_dialog, winner_host = ?4,
               winner_source_mtime_ms = ?5, winner_source_size = ?6,
               winner_native_meta_mtime_ms = ?7, winner_native_meta_size = ?8,
               winner_adapter_version = ?9
             FROM session_projections p
             WHERE sessions.id = ?1 AND p.source = ?2 AND p.native_id = ?3 AND p.host = ?4",
            params![
                session_id,
                meta.source,
                meta.native_id,
                winner.host,
                winner.source_mtime_ms,
                winner.source_size,
                winner.native_meta_mtime_ms,
                winner.native_meta_size,
                winner.adapter_version,
            ],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "INSERT INTO messages (session_id, idx, role, kind, text, timestamp_ms, actions_before)
             SELECT ?1, idx, role, kind, text, timestamp_ms, actions_before
             FROM projection_messages
             WHERE source = ?2 AND native_id = ?3 AND host = ?4 ORDER BY idx",
            params![session_id, meta.source, meta.native_id, winner.host],
        )?;
        tx.commit()?;
        Ok(session_id)
    }

    /// Remove derived state for an identity that an Adapter has proven was
    /// produced by an older parser rather than by the native Source.
    pub fn retire_session_identity(&mut self, source: &str, native_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM sessions WHERE source = ?1 AND native_id = ?2",
            params![source, native_id],
        )?;
        for table in [
            "capture_presence",
            "session_revisions",
            "session_projections",
            "projection_messages",
            "conversation_branches",
            "conversation_native_projects",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source = ?1 AND native_id = ?2"),
                params![source, native_id],
            )?;
        }
        tx.execute(
            "DELETE FROM subagent_parent_facts
             WHERE child_source = ?1 AND child_native_id = ?2",
            params![source, native_id],
        )?;
        tx.execute(
            "DELETE FROM subagent_reference_facts
             WHERE parent_source = ?1 AND parent_native_id = ?2",
            params![source, native_id],
        )?;
        tx.execute(
            "DELETE FROM subagent_links
             WHERE (parent_source = ?1 AND parent_native_id = ?2)
                OR (child_source = ?1 AND child_native_id = ?2)",
            params![source, native_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record that this Host's settled Capture no longer has an independently
    /// extractable Conversation. The revision remains election evidence: a
    /// non-winning Host cannot erase the canonical projection, while an
    /// elected non-extractable Capture retracts the stale standalone Session.
    pub(crate) fn capture_exclusions_changed(&self, patterns: &[String]) -> Result<bool> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='capture_exclusions'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match stored {
            Some(value) => value != serde_json::to_string(patterns)?,
            None => !patterns.is_empty(),
        })
    }

    pub(crate) fn set_capture_exclusions(&self, patterns: &[String]) -> Result<()> {
        self.conn.execute("INSERT INTO meta(key,value) VALUES ('capture_exclusions',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(patterns)?])?;
        Ok(())
    }

    pub fn upsert_nonextractable_from_capture(&mut self, meta: &CaptureMeta) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO session_revisions
               (source, native_id, host, source_mtime_ms, source_size,
                native_meta_mtime_ms, native_meta_size, adapter_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(source, native_id, host) DO UPDATE SET
               source_mtime_ms = excluded.source_mtime_ms,
               source_size = excluded.source_size,
               native_meta_mtime_ms = excluded.native_meta_mtime_ms,
               native_meta_size = excluded.native_meta_size,
               adapter_version = excluded.adapter_version",
            params![
                meta.source,
                meta.native_id,
                meta.host,
                meta.source_mtime_ms,
                meta.source_size,
                meta.native_meta_mtime_ms,
                meta.native_meta_size,
                meta.adapter_version,
            ],
        )?;
        tx.execute(
            "DELETE FROM session_projections
             WHERE source = ?1 AND native_id = ?2 AND host = ?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
        tx.execute(
            "DELETE FROM projection_messages
             WHERE source = ?1 AND native_id = ?2 AND host = ?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
        tx.execute(
            "DELETE FROM conversation_branches WHERE source=?1 AND native_id=?2 AND host=?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
        native_projects::replace(&tx, meta, None)?;
        tx.execute(
            "DELETE FROM subagent_parent_facts
             WHERE child_source = ?1 AND child_native_id = ?2 AND fact_host = ?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
        tx.execute(
            "DELETE FROM subagent_reference_facts
             WHERE parent_source = ?1 AND parent_native_id = ?2 AND fact_host = ?3",
            params![meta.source, meta.native_id, meta.host],
        )?;

        let winner = {
            let mut stmt = tx.prepare(
                "SELECT host, source_mtime_ms, source_size, native_meta_mtime_ms,
                        native_meta_size, adapter_version
                 FROM session_revisions WHERE source = ?1 AND native_id = ?2",
            )?;
            stmt.query_map(params![meta.source, meta.native_id], |row| {
                Ok(CaptureRevision {
                    host: row.get(0)?,
                    source_mtime_ms: row.get(1)?,
                    source_size: row.get(2)?,
                    native_meta_mtime_ms: row.get(3)?,
                    native_meta_size: row.get(4)?,
                    adapter_version: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max_by(|a, b| compare_revisions(&meta.source, a, b))
            .expect("just inserted a revision")
        };
        let winner_is_extractable: bool = tx.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM session_projections
               WHERE source = ?1 AND native_id = ?2 AND host = ?3
             )",
            params![meta.source, meta.native_id, winner.host],
            |row| row.get(0),
        )?;
        if !winner_is_extractable {
            tx.execute(
                "DELETE FROM sessions WHERE source = ?1 AND native_id = ?2",
                params![meta.source, meta.native_id],
            )?;
        } else if let Some(session_id) = tx
            .query_row(
                "SELECT id FROM sessions WHERE source = ?1 AND native_id = ?2",
                params![meta.source, meta.native_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            tx.execute(
                "UPDATE sessions SET
                   title = p.title, project = p.project, project_path = p.project_path,
                   created_at_ms = p.created_at_ms, updated_at_ms = p.updated_at_ms,
                   message_count = p.message_count, trailing_actions = p.trailing_actions,
                   captured_at_ms = p.captured_at_ms,
                   starting_model = p.starting_model,
                   starting_reasoning_effort = p.starting_reasoning_effort,
                   git_branch = p.git_branch, git_commit = p.git_commit,
                   git_repository_url = p.git_repository_url,
                   patched_files_json = p.patched_files_json,
                   referenced_files_json = p.referenced_files_json,
                   has_dialog = p.has_dialog, winner_host = ?4,
                   winner_source_mtime_ms = ?5, winner_source_size = ?6,
                   winner_native_meta_mtime_ms = ?7, winner_native_meta_size = ?8,
                   winner_adapter_version = ?9
                 FROM session_projections p
                 WHERE sessions.id = ?1 AND p.source = ?2 AND p.native_id = ?3
                   AND p.host = ?4",
                params![
                    session_id,
                    meta.source,
                    meta.native_id,
                    winner.host,
                    winner.source_mtime_ms,
                    winner.source_size,
                    winner.native_meta_mtime_ms,
                    winner.native_meta_size,
                    winner.adapter_version,
                ],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute(
                "INSERT INTO messages
                   (session_id, idx, role, text, timestamp_ms, actions_before)
                 SELECT ?1, idx, role, text, timestamp_ms, actions_before
                 FROM projection_messages
                 WHERE source = ?2 AND native_id = ?3 AND host = ?4 ORDER BY idx",
                params![session_id, meta.source, meta.native_id, winner.host],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// All sessions with Dialog, for rendering.
    pub fn sessions_with_dialog(&self) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, native_id, title, project, project_path,
                    created_at_ms, updated_at_ms, message_count, trailing_actions,
                    starting_model, starting_reasoning_effort, git_branch, git_commit,
                    git_repository_url, patched_files_json, referenced_files_json,
                    inherited_context_source_id, inherited_context_copied_item_count, winner_host,
                    captured_at_ms
             FROM sessions WHERE has_dialog = 1
             ORDER BY source, native_id",
        )?;
        let mut rows = stmt
            .query_map([], session_row_from)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut host_stmt = self
            .conn
            .prepare("SELECT host FROM session_hosts WHERE session_id = ?1 ORDER BY host")?;
        let repository_facts = self.repository_facts()?;
        for row in &mut rows {
            row.hosts = host_stmt
                .query_map(params![row.id], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            row.subagent_parent = self.subagent_parent(&row.source, &row.native_id)?;
            row.transcript_facts.recovered_via = self.recovered_via(row.id)?;
            row.host_attribution = self.host_attribution(&row.source, &row.native_id)?;
            row.native_project = self.native_project(row.id)?;
            row.repository_resolution = repository_facts
                .get(&(
                    row.winner_host.clone().unwrap_or_default(),
                    row.source.clone(),
                    row.native_id.clone(),
                ))
                .cloned();
        }
        Ok(rows)
    }

    fn subagent_parent(
        &self,
        source: &str,
        child_native_id: &str,
    ) -> Result<Option<SubagentParent>> {
        let read = |row: &rusqlite::Row<'_>| {
            Ok(SubagentParent {
                parent_native_id: row.get(0)?,
                description: row.get(1)?,
                agent_type: row.get(2)?,
                instructions: row.get(3)?,
                depth: row.get::<_, Option<i64>>(4)?.map(|n| n as u32),
                native_key: row.get(5)?,
                link_key: row.get(6)?,
            })
        };
        let resolved = self
            .conn
            .query_row(
                "SELECT parent_native_id, description, agent_type, instructions, depth,
                    NULL, NULL
             FROM subagent_links WHERE child_source = ?1 AND child_native_id = ?2",
                params![source, child_native_id],
                read,
            )
            .optional()?;
        if resolved.is_some() {
            return Ok(resolved);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT parent_native_id, description, agent_type, instructions, depth,
                    native_key, link_key
             FROM subagent_parent_facts f
             JOIN sessions s ON s.source = f.child_source
                            AND s.native_id = f.child_native_id
                            AND (s.winner_host = f.fact_host
                                 OR s.winner_host IS NULL)
             WHERE child_source = ?1 AND child_native_id = ?2
             ORDER BY ((description IS NOT NULL) + (agent_type IS NOT NULL)
                     + (instructions IS NOT NULL) + (depth IS NOT NULL)
                     + (native_key IS NOT NULL) + (link_key IS NOT NULL)) DESC,
                      fact_host
             LIMIT 1",
                params![source, child_native_id],
                read,
            )
            .optional()?)
    }

    pub fn subagent_links(
        &self,
        source: &str,
        parent_native_id: &str,
    ) -> Result<Vec<SubagentLinkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT child.id, l.child_native_id, l.description, l.agent_type,
                    l.spawned_at_ms, l.instructions, l.depth
             FROM subagent_links l
             JOIN sessions child ON child.source = l.child_source
                                AND child.native_id = l.child_native_id
             WHERE l.parent_source = ?1 AND l.parent_native_id = ?2
             ORDER BY l.spawned_at_ms, l.child_native_id",
        )?;
        Ok(stmt
            .query_map(params![source, parent_native_id], |row| {
                Ok(SubagentLinkRow {
                    child_session_id: row.get(0)?,
                    link: SubagentLink {
                        parent_native_id: parent_native_id.to_string(),
                        child_native_id: row.get(1)?,
                        description: row.get(2)?,
                        agent_type: row.get(3)?,
                        spawned_at_ms: row.get(4)?,
                        instructions: row.get(5)?,
                        depth: row.get::<_, Option<i64>>(6)?.map(|n| n as u32),
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Rebuild the resolved bidirectional graph from independently extracted
    /// parent and child facts. This is intentionally global: native formats
    /// use link keys that are not canonical Session identities.
    pub fn resolve_subagent_links(&mut self) -> Result<usize> {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct ParentFact {
            source: String,
            child_id: String,
            parent_id: String,
            description: Option<String>,
            agent_type: Option<String>,
            instructions: Option<String>,
            depth: Option<i64>,
            native_key: Option<String>,
            link_key: Option<String>,
        }
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct ReferenceFact {
            source: String,
            parent_id: String,
            child_id: Option<String>,
            native_key: Option<String>,
            link_key: Option<String>,
            description: Option<String>,
            agent_type: Option<String>,
            instructions: Option<String>,
            spawned_at_ms: Option<i64>,
        }

        let parents = {
            let mut stmt = self.conn.prepare(
                "SELECT f.child_source, f.child_native_id, f.parent_native_id,
                        description, agent_type, instructions, depth, native_key, link_key
                 FROM subagent_parent_facts f
                 JOIN sessions child ON child.source = f.child_source
                                    AND child.native_id = f.child_native_id
                                    AND (child.winner_host = f.fact_host
                                         OR child.winner_host IS NULL)
                 ORDER BY f.child_source, f.child_native_id",
            )?;
            stmt.query_map([], |r| {
                Ok(ParentFact {
                    source: r.get(0)?,
                    child_id: r.get(1)?,
                    parent_id: r.get(2)?,
                    description: r.get(3)?,
                    agent_type: r.get(4)?,
                    instructions: r.get(5)?,
                    depth: r.get(6)?,
                    native_key: r.get(7)?,
                    link_key: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let references = {
            let mut stmt = self.conn.prepare(
                "SELECT f.parent_source, f.parent_native_id, child_native_id,
                        child_native_key, link_key, description, agent_type,
                        instructions, spawned_at_ms
                 FROM subagent_reference_facts f
                 JOIN sessions parent ON parent.source = f.parent_source
                                     AND parent.native_id = f.parent_native_id
                                     AND (parent.winner_host = f.fact_host
                                          OR parent.winner_host IS NULL)
                 ORDER BY f.parent_source, f.parent_native_id, idx",
            )?;
            stmt.query_map([], |r| {
                Ok(ReferenceFact {
                    source: r.get(0)?,
                    parent_id: r.get(1)?,
                    child_id: r.get(2)?,
                    native_key: r.get(3)?,
                    link_key: r.get(4)?,
                    description: r.get(5)?,
                    agent_type: r.get(6)?,
                    instructions: r.get(7)?,
                    spawned_at_ms: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        fn parent_richness(fact: &ParentFact) -> usize {
            [
                fact.description.is_some(),
                fact.agent_type.is_some(),
                fact.instructions.is_some(),
                fact.depth.is_some(),
                fact.native_key.is_some(),
                fact.link_key.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count()
        }
        let mut parent_groups: std::collections::BTreeMap<(String, String), Vec<ParentFact>> =
            std::collections::BTreeMap::new();
        for fact in parents {
            parent_groups
                .entry((fact.source.clone(), fact.child_id.clone()))
                .or_default()
                .push(fact);
        }
        let parents = parent_groups
            .into_values()
            .map(|mut facts| {
                facts.sort_by(|a, b| {
                    parent_richness(b)
                        .cmp(&parent_richness(a))
                        .then_with(|| format!("{a:?}").cmp(&format!("{b:?}")))
                });
                facts.remove(0)
            })
            .collect::<Vec<_>>();

        fn reference_richness(fact: &ReferenceFact) -> usize {
            [
                fact.child_id.is_some(),
                fact.native_key.is_some(),
                fact.link_key.is_some(),
                fact.description.is_some(),
                fact.agent_type.is_some(),
                fact.instructions.is_some(),
                fact.spawned_at_ms.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count()
        }
        fn choose_reference<'a>(
            parent: &ParentFact,
            references: &'a [ReferenceFact],
        ) -> (Option<&'a ReferenceFact>, bool, bool) {
            let scoped = |reference: &&ReferenceFact| {
                reference.source == parent.source && reference.parent_id == parent.parent_id
            };
            let mut tier = references
                .iter()
                .filter(scoped)
                .filter(|r| r.child_id.as_deref() == Some(parent.child_id.as_str()))
                .collect::<Vec<_>>();
            if tier.is_empty() && parent.link_key.is_some() {
                tier = references
                    .iter()
                    .filter(scoped)
                    .filter(|r| r.link_key == parent.link_key)
                    .collect();
            }
            if tier.is_empty() && parent.native_key.is_some() {
                tier = references
                    .iter()
                    .filter(scoped)
                    .filter(|r| r.native_key == parent.native_key)
                    .collect();
            }
            let parent_side_match = !tier.is_empty();
            tier.sort_by_key(|fact| format!("{fact:?}"));
            tier.dedup_by(|a, b| *a == *b);
            let Some(max_richness) = tier.iter().map(|fact| reference_richness(fact)).max() else {
                return (None, false, false);
            };
            let richest = tier
                .into_iter()
                .filter(|fact| reference_richness(fact) == max_richness)
                .collect::<Vec<_>>();
            if richest.len() == 1 {
                (Some(richest[0]), parent_side_match, false)
            } else {
                (None, parent_side_match, true)
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM subagent_links", [])?;
        let session_exists = |source: &str, id: &str| -> Result<bool> {
            Ok(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE source = ?1 AND native_id = ?2)",
                params![source, id],
                |r| r.get(0),
            )?)
        };
        let mut inserted = 0;
        for parent in &parents {
            if !session_exists(&parent.source, &parent.parent_id)?
                || !session_exists(&parent.source, &parent.child_id)?
            {
                continue;
            }
            let (reference, parent_side_match, ambiguous) = choose_reference(parent, &references);
            if ambiguous {
                tracing::debug!(
                    source = parent.source,
                    parent = parent.parent_id,
                    child = parent.child_id,
                    "ambiguous Subagent reference; child Parent fact resolves the edge without parent-side enrichment"
                );
            }
            // Cursor's child root proves a recoverable child Transcript but
            // does not prove that the elected parent still contains a spawn
            // action. Do not manufacture a parent `<subagent>` placement
            // from child-side evidence alone.
            if parent.source == "cursor" && !parent_side_match {
                continue;
            }
            tx.execute(
                "INSERT INTO subagent_links
                   (parent_source, parent_native_id, child_source, child_native_id,
                    description, agent_type, instructions, spawned_at_ms, depth)
                 VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    parent.source,
                    parent.parent_id,
                    parent.child_id,
                    reference
                        .and_then(|r| r.description.as_ref())
                        .or(parent.description.as_ref()),
                    reference
                        .and_then(|r| r.agent_type.as_ref())
                        .or(parent.agent_type.as_ref()),
                    reference
                        .and_then(|r| r.instructions.as_ref())
                        .or(parent.instructions.as_ref()),
                    reference.and_then(|r| r.spawned_at_ms),
                    parent.depth,
                ],
            )?;
            inserted += 1;
        }
        // Exact parent-side UUID references can resolve without a child fact.
        let parent_fact_children = parents
            .iter()
            .map(|parent| {
                (
                    (parent.source.clone(), parent.child_id.clone()),
                    parent.parent_id.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut exact_groups: std::collections::BTreeMap<(String, String), Vec<&ReferenceFact>> =
            std::collections::BTreeMap::new();
        for reference in references.iter().filter(|r| r.child_id.is_some()) {
            exact_groups
                .entry((
                    reference.source.clone(),
                    reference.child_id.clone().expect("filtered"),
                ))
                .or_default()
                .push(reference);
        }
        for ((source, child), mut candidates) in exact_groups {
            // A child Parent fact identifies its one parent. It owns this
            // edge and any parent-side enrichment was handled above; cloned
            // Cursor composers can retain stale exact child references.
            if parent_fact_children.contains_key(&(source.clone(), child.clone())) {
                continue;
            }
            let parent_ids = candidates
                .iter()
                .map(|candidate| candidate.parent_id.as_str())
                .collect::<BTreeSet<_>>();
            if parent_ids.len() != 1 {
                tracing::warn!(
                    source,
                    child,
                    parents = ?parent_ids,
                    "conflicting exact Subagent references; link remains unresolved"
                );
                continue;
            }
            candidates.sort_by(|a, b| {
                reference_richness(b)
                    .cmp(&reference_richness(a))
                    .then_with(|| format!("{a:?}").cmp(&format!("{b:?}")))
            });
            let reference = candidates[0];
            if !session_exists(&reference.source, &reference.parent_id)?
                || !session_exists(&reference.source, &child)?
            {
                continue;
            }
            inserted += tx.execute(
                "INSERT OR IGNORE INTO subagent_links
                   (parent_source, parent_native_id, child_source, child_native_id,
                    description, agent_type, instructions, spawned_at_ms)
                 VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6, ?7)",
                params![
                    source,
                    reference.parent_id,
                    child,
                    reference.description,
                    reference.agent_type,
                    reference.instructions,
                    reference.spawned_at_ms
                ],
            )?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Messages of one session, in conversation order.
    pub fn messages(&self, session_id: i64) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, kind, text, timestamp_ms, actions_before
             FROM messages WHERE session_id = ?1 ORDER BY idx",
        )?;
        let msgs = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        msgs.into_iter()
            .map(|(role, kind, text, timestamp_ms, actions_before)| {
                Ok(Message {
                    role: role.parse::<Role>()?,
                    kind: kind.parse::<MessageKind>()?,
                    text,
                    timestamp_ms,
                    actions_before: actions_before as u32,
                })
            })
            .collect()
    }

    /// Sessions with at least one timestamped Message in `[start_ms, end_ms)`,
    /// with the full ordered Message list of each. Candidate selection for
    /// Workday activity queries; attribution happens in `crate::workday`.
    pub fn sessions_with_messages_between(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<(SessionRow, Vec<Message>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT session_id FROM messages
             WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
        )?;
        let ids = stmt
            .query_map(params![start_ms, end_ms], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(session) = self.session_row(id)? else {
                continue;
            };
            let messages = self.messages(id)?;
            out.push((session, messages));
        }
        Ok(out)
    }

    /// One session row by surrogate id, or `None` when absent.
    pub fn session_row(&self, session_id: i64) -> Result<Option<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, native_id, title, project, project_path,
                    created_at_ms, updated_at_ms, message_count, trailing_actions,
                    starting_model, starting_reasoning_effort, git_branch, git_commit,
                    git_repository_url, patched_files_json, referenced_files_json,
                    inherited_context_source_id, inherited_context_copied_item_count, winner_host,
                    captured_at_ms
             FROM sessions WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![session_id], session_row_from)
            .optional()?;
        let Some(mut row) = row else {
            return Ok(None);
        };
        row.hosts = self
            .conn
            .prepare("SELECT host FROM session_hosts WHERE session_id = ?1 ORDER BY host")?
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        row.subagent_parent = self.subagent_parent(&row.source, &row.native_id)?;
        row.transcript_facts.recovered_via = self.recovered_via(row.id)?;
        row.host_attribution = self.host_attribution(&row.source, &row.native_id)?;
        row.native_project = self.native_project(row.id)?;
        row.repository_resolution = self
            .repository_facts()?
            .get(&(
                row.winner_host.clone().unwrap_or_default(),
                row.source.clone(),
                row.native_id.clone(),
            ))
            .cloned();
        Ok(Some(row))
    }

    /// Local membership is independent of the elected Orchestrator Projection winner.
    pub(crate) fn orchestrator_on_host(
        &self,
        source: &str,
        native_id: &str,
        host: &str,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_thread_projections WHERE source=?1 AND native_id=?2 AND host=?3)",
            params![source, native_id, host], |row| row.get(0),
        )?)
    }

    /// Native path recorded for one Session on one Host.
    pub(crate) fn session_native_path(
        &self,
        session_id: i64,
        host: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT native_path FROM session_hosts WHERE session_id = ?1 AND host = ?2",
                params![session_id, host],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Count of session rows (test/diagnostic aid).
    pub fn session_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?)
    }

    /// The Bookmark for (Host, Source): the highest native mtime already
    /// ingested, or `None` before the first successful ingest.
    pub fn bookmark(&self, host: &str, source: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT bookmark_ms FROM bookmarks WHERE host = ?1 AND source = ?2",
                params![host, source],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Explicit operator backfill: reopen one Host/Source enumeration window.
    pub fn reset_bookmark(&self, host: &str, source: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM bookmarks WHERE host=?1 AND source=?2",
            params![host, source],
        )?;
        Ok(())
    }

    /// Advance the Bookmark for (Host, Source). Monotonic: a Bookmark never
    /// moves backwards, so replaying an old Bundle can't reopen the window.
    pub fn advance_bookmark(&self, host: &str, source: &str, bookmark_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (host, source, bookmark_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(host, source) DO UPDATE SET
               bookmark_ms = MAX(bookmark_ms, excluded.bookmark_ms)",
            params![host, source, bookmark_ms],
        )?;
        Ok(())
    }

    pub fn replace_digest_cache(&mut self, rows: &[DigestCacheRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM digest_cache", [])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO digest_cache
                 (source, native_id, workday, last_idx, title, path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for row in rows {
                insert.execute(params![
                    row.source,
                    row.native_id,
                    row.workday,
                    row.last_idx.map(|value| value as i64),
                    row.title,
                    row.path,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Deterministic textual dump of everything the database holds, minus
    /// surrogate row ids: sessions (ordered by identity), their hosts,
    /// messages, and Bookmarks. Two databases with the same dump are
    /// content-equivalent — this is the `rebuild` acceptance check.
    pub fn content_dump(&self) -> Result<String> {
        use std::fmt::Write as _;
        let mut out = String::new();
        native_projects::append_dump(&self.conn, &mut out)?;
        {
            let mut statement=self.conn.prepare("SELECT source,native_id,host,payload FROM conversation_branches ORDER BY source,native_id,host")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                writeln!(
                    out,
                    "branches {}/{} host={} payload={}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?
                )?;
            }
        }

        let mut presence = self.conn.prepare(
            "SELECT source,native_id,host FROM capture_presence ORDER BY source,native_id,host",
        )?;
        let mut present = presence.query([])?;
        while let Some(row) = present.next()? {
            writeln!(
                out,
                "capture-presence {}/{} host={}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?
            )?;
        }
        let mut sessions = self.conn.prepare(
            "SELECT id, source, native_id, title, project, project_path,
                    created_at_ms, updated_at_ms, message_count, trailing_actions, has_dialog,
                    starting_model, starting_reasoning_effort, git_branch, git_commit,
                    git_repository_url, patched_files_json, referenced_files_json,
                    inherited_context_source_id, inherited_context_copied_item_count,
                    winner_host, winner_source_mtime_ms, winner_source_size,
                    winner_native_meta_mtime_ms, winner_native_meta_size,
                    winner_adapter_version, captured_at_ms
             FROM sessions ORDER BY source, native_id",
        )?;
        let mut hosts = self.conn.prepare(
            "SELECT host, native_path FROM session_hosts WHERE session_id = ?1 ORDER BY host",
        )?;
        let mut messages = self.conn.prepare(
            "SELECT idx, role, kind, text, timestamp_ms, actions_before
             FROM messages WHERE session_id = ?1 ORDER BY idx",
        )?;
        let mut rows = sessions.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let _ = writeln!(
                out,
                "session {}/{} title={:?} project={:?} project_path={:?} created={:?} updated={:?} messages={} trailing_actions={} captured_at_ms={} transcript_facts=(model={:?},effort={:?},git=({:?},{:?},{:?}),patched={:?},referenced={:?}) inherited_context=({:?},{:?}) has_dialog={} winner={:?} body_rev=({:?},{:?}) meta_rev=({:?},{:?}) adapter={:?}",
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(26)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, String>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<i64>>(19)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<String>>(20)?,
                row.get::<_, Option<i64>>(21)?,
                row.get::<_, Option<u64>>(22)?,
                row.get::<_, Option<i64>>(23)?,
                row.get::<_, Option<u64>>(24)?,
                row.get::<_, Option<u32>>(25)?,
            );
            if let Some(via) = self.recovered_via(id)? {
                writeln!(
                    out,
                    "  recovered-via {}/{} host={}",
                    via.source, via.thread_id, via.host
                )?;
            }
            let mut host_rows = hosts.query(params![id])?;
            while let Some(h) = host_rows.next()? {
                let _ = writeln!(
                    out,
                    "  host {} native_path={:?}",
                    h.get::<_, String>(0)?,
                    h.get::<_, Option<String>>(1)?,
                );
            }
            let mut msg_rows = messages.query(params![id])?;
            while let Some(m) = msg_rows.next()? {
                let _ = writeln!(
                    out,
                    "  message {} {} kind={} ts={:?} actions_before={} text={:?}",
                    m.get::<_, i64>(0)?,
                    m.get::<_, String>(1)?,
                    m.get::<_, String>(2)?,
                    m.get::<_, Option<i64>>(4)?,
                    m.get::<_, i64>(5)?,
                    m.get::<_, String>(3)?,
                );
            }
        }
        let mut revisions = self.conn.prepare(
            "SELECT source, native_id, host, source_mtime_ms, source_size,
                    native_meta_mtime_ms, native_meta_size, adapter_version
             FROM session_revisions ORDER BY source, native_id, host",
        )?;
        let mut rows = revisions.query([])?;
        while let Some(r) = rows.next()? {
            let _ = writeln!(
                out,
                "revision {}/{} host={} body=({},{}) meta=({:?},{:?}) adapter={}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<u64>>(6)?,
                r.get::<_, u32>(7)?,
            );
        }
        let mut projections = self.conn.prepare(
            "SELECT source, native_id, host, title, project, project_path, created_at_ms,
                    updated_at_ms, message_count, trailing_actions,
                    starting_model, starting_reasoning_effort, git_branch, git_commit,
                    git_repository_url, patched_files_json, referenced_files_json,
                    inherited_context_source_id, inherited_context_copied_item_count, has_dialog,
                    captured_at_ms
             FROM session_projections ORDER BY source, native_id, host",
        )?;
        let mut projection_messages = self.conn.prepare(
            "SELECT idx, role, kind, text, timestamp_ms, actions_before
             FROM projection_messages
             WHERE source = ?1 AND native_id = ?2 AND host = ?3 ORDER BY idx",
        )?;
        let mut rows = projections.query([])?;
        while let Some(r) = rows.next()? {
            let source: String = r.get(0)?;
            let native_id: String = r.get(1)?;
            let host: String = r.get(2)?;
            let _ = writeln!(
                out,
                "projection {source}/{native_id} host={host} title={:?} project={:?} project_path={:?} created={:?} updated={:?} messages={} trailing_actions={} captured_at_ms={} transcript_facts=(model={:?},effort={:?},git=({:?},{:?},{:?}),patched={:?},referenced={:?}) inherited_context=({:?},{:?}) has_dialog={}",
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(20)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, String>(15)?,
                r.get::<_, String>(16)?,
                r.get::<_, Option<String>>(17)?,
                r.get::<_, Option<i64>>(18)?,
                r.get::<_, i64>(19)?,
            );
            let mut messages = projection_messages.query(params![source, native_id, host])?;
            while let Some(m) = messages.next()? {
                let _ = writeln!(
                    out,
                    "  projection-message {} {} kind={} ts={:?} actions_before={} text={:?}",
                    m.get::<_, i64>(0)?,
                    m.get::<_, String>(1)?,
                    m.get::<_, String>(2)?,
                    m.get::<_, Option<i64>>(4)?,
                    m.get::<_, i64>(5)?,
                    m.get::<_, String>(3)?,
                );
            }
        }
        let mut parent_facts = self.conn.prepare(
            "SELECT fact_host, child_source, child_native_id, parent_native_id, description,
                    agent_type, instructions, depth, native_key, link_key
             FROM subagent_parent_facts ORDER BY fact_host, child_source, child_native_id",
        )?;
        let mut rows = parent_facts.query([])?;
        while let Some(r) = rows.next()? {
            let _ = writeln!(
                out,
                "subagent-parent host={} {}/{} parent={} description={:?} type={:?} instructions={:?} depth={:?} native_key={:?} link_key={:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?
            );
        }
        let mut refs = self.conn.prepare(
            "SELECT fact_host, parent_source, parent_native_id, idx, child_native_id,
                    child_native_key, link_key, description, agent_type,
                    instructions, spawned_at_ms
             FROM subagent_reference_facts ORDER BY fact_host, parent_source, parent_native_id, idx",
        )?;
        let mut rows = refs.query([])?;
        while let Some(r) = rows.next()? {
            let _ = writeln!(
                out,
                "subagent-ref host={} {}/{} #{} child={:?} native_key={:?} link_key={:?} description={:?} type={:?} instructions={:?} spawned={:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<i64>>(10)?
            );
        }
        let mut links = self.conn.prepare(
            "SELECT parent_source, parent_native_id, child_source, child_native_id,
                    description, agent_type, instructions, spawned_at_ms, depth
             FROM subagent_links ORDER BY parent_source, parent_native_id, child_source, child_native_id",
        )?;
        let mut rows = links.query([])?;
        while let Some(r) = rows.next()? {
            let _ = writeln!(
                out,
                "subagent-link {}/{} -> {}/{} description={:?} type={:?} instructions={:?} spawned={:?} depth={:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?
            );
        }
        self.dump_orchestrators(&mut out)?;
        let mut bookmarks = self
            .conn
            .prepare("SELECT host, source, bookmark_ms FROM bookmarks ORDER BY host, source")?;
        let mut wm_rows = bookmarks.query([])?;
        while let Some(w) = wm_rows.next()? {
            let _ = writeln!(
                out,
                "bookmark {}/{} {}",
                w.get::<_, String>(0)?,
                w.get::<_, String>(1)?,
                w.get::<_, i64>(2)?,
            );
        }
        Ok(out)
    }
}

impl Db {
    /// Replace one Host's complete Orchestrator projection, then elect and
    /// rematerialize the canonical winner as one indivisible snapshot.
    pub fn upsert_orchestrator_projection(
        &mut self,
        projection: &OrchestratorProjection,
    ) -> Result<()> {
        let discriminator = i64::try_from(projection.discriminator)
            .context("Orchestrator discriminator exceeds SQLite INTEGER")?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO orchestrator_thread_projections
               (source,native_id,host,native_path,logical_revision_ms,discriminator,
                adapter_version,title,project_id,project_title,workspace_root,
                provider_name,provider_instance_id,model,runtime_mode,interaction_mode,
                branch,worktree,created_at_ms,updated_at_ms,archived,pinned,settled,
                linked_pull_request,checkpoint_count)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                     ?17,?18,?19,?20,?21,?22,?23,?24,?25)
             ON CONFLICT(source,native_id,host) DO UPDATE SET
               native_path=excluded.native_path, logical_revision_ms=excluded.logical_revision_ms,
               discriminator=excluded.discriminator, adapter_version=excluded.adapter_version,
               title=excluded.title, project_id=excluded.project_id,
               project_title=excluded.project_title, workspace_root=excluded.workspace_root,
               provider_name=excluded.provider_name,
               provider_instance_id=excluded.provider_instance_id, model=excluded.model,
               runtime_mode=excluded.runtime_mode, interaction_mode=excluded.interaction_mode,
               branch=excluded.branch, worktree=excluded.worktree,
               created_at_ms=excluded.created_at_ms, updated_at_ms=excluded.updated_at_ms,
               archived=excluded.archived, pinned=excluded.pinned, settled=excluded.settled,
               linked_pull_request=excluded.linked_pull_request,
               checkpoint_count=excluded.checkpoint_count",
            params![
                projection.source,
                projection.native_id,
                projection.host,
                projection.native_path,
                projection.logical_revision_ms,
                discriminator,
                projection.adapter_version,
                projection.title,
                projection.project_id,
                projection.project_title,
                projection.workspace_root,
                projection.provider_name,
                projection.provider_instance_id,
                projection.model,
                projection.runtime_mode,
                projection.interaction_mode,
                projection.branch,
                projection.worktree,
                projection.created_at_ms,
                projection.updated_at_ms,
                projection.archived,
                projection.pinned,
                projection.settled,
                projection.linked_pull_request,
                projection.checkpoint_count,
            ],
        )?;
        for table in [
            "orchestrator_projection_link_facts",
            "orchestrator_projection_messages",
            "orchestrator_projection_turns",
            "orchestrator_projection_activities",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source=?1 AND native_id=?2 AND host=?3"),
                params![projection.source, projection.native_id, projection.host],
            )?;
        }
        for (idx, fact) in projection.link_facts.iter().enumerate() {
            tx.execute(
                "INSERT INTO orchestrator_projection_link_facts
                   (source,native_id,host,idx,status,provider_source,provider_native_id,reason)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    projection.source,
                    projection.native_id,
                    projection.host,
                    idx as i64,
                    fact.status.as_str(),
                    fact.provider_source,
                    fact.provider_native_id,
                    fact.reason
                ],
            )?;
        }
        for (idx, message) in projection.messages.iter().enumerate() {
            tx.execute(
                "INSERT INTO orchestrator_projection_messages
                   (source,native_id,host,idx,role,text,timestamp_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    projection.source,
                    projection.native_id,
                    projection.host,
                    idx as i64,
                    message.role,
                    message.text,
                    message.timestamp_ms
                ],
            )?;
        }
        for (idx, turn) in projection.turns.iter().enumerate() {
            tx.execute(
                "INSERT INTO orchestrator_projection_turns
                   (source,native_id,host,idx,turn_native_id,state,checkpoint_id,payload_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    projection.source,
                    projection.native_id,
                    projection.host,
                    idx as i64,
                    turn.native_id,
                    turn.state,
                    turn.checkpoint_id,
                    turn.payload_json
                ],
            )?;
        }
        for (idx, activity) in projection.activities.iter().enumerate() {
            tx.execute(
                "INSERT INTO orchestrator_projection_activities
                   (source,native_id,host,idx,activity_native_id,kind,payload_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    projection.source,
                    projection.native_id,
                    projection.host,
                    idx as i64,
                    activity.native_id,
                    activity.kind,
                    activity.payload_json
                ],
            )?;
        }
        materialize_orchestrator_winner(&tx, &projection.source, &projection.native_id)?;
        tx.commit()?;
        self.index_t3_repository(projection)?;
        Ok(())
    }

    pub fn orchestrator_thread(
        &self,
        source: &str,
        native_id: &str,
    ) -> Result<Option<OrchestratorThreadRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source,native_id,winner_host,title,project_title
             FROM orchestrator_threads WHERE source=?1 AND native_id=?2",
                params![source, native_id],
                |row| {
                    Ok(OrchestratorThreadRow {
                        source: row.get(0)?,
                        native_id: row.get(1)?,
                        winner_host: row.get(2)?,
                        title: row.get(3)?,
                        project_title: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn orchestrator_messages(
        &self,
        source: &str,
        native_id: &str,
    ) -> Result<Vec<OrchestratorMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT role,text,timestamp_ms FROM orchestrator_messages
             WHERE source=?1 AND native_id=?2 ORDER BY idx",
        )?;
        Ok(stmt
            .query_map(params![source, native_id], |row| {
                Ok(OrchestratorMessage {
                    role: row.get(0)?,
                    text: row.get(1)?,
                    timestamp_ms: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Recompute all exact Orchestrator Links from canonical winner facts.
    /// Missing provider Sessions recover from exact Thread Dialog; ambiguous
    /// candidates do not contribute recovery or Augmentation.
    pub fn resolve_orchestrator_links(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM orchestrator_links", [])?;
        recovery::materialize(&tx)?;
        let identities = {
            let mut stmt = tx.prepare(
                "SELECT source,native_id FROM orchestrator_threads ORDER BY source,native_id",
            )?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut resolved = 0;
        for (source, native_id) in identities {
            let candidates = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT provider_source,provider_native_id
                     FROM orchestrator_link_facts
                     WHERE source=?1 AND native_id=?2 AND status='candidate'
                       AND provider_source IS NOT NULL AND provider_source<>''
                       AND provider_native_id IS NOT NULL AND provider_native_id<>''
                     ORDER BY provider_source,provider_native_id",
                )?;
                stmt.query_map(params![source, native_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
            };
            if candidates.len() != 1 {
                continue;
            }
            let (provider_source, provider_native_id) = &candidates[0];
            let session_id = tx
                .query_row(
                    "SELECT id FROM sessions WHERE source=?1 AND native_id=?2",
                    params![provider_source, provider_native_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(session_id) = session_id {
                tx.execute(
                    "INSERT INTO orchestrator_links
                       (orchestrator_source,orchestrator_native_id,session_id,
                        provider_source,provider_native_id)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        source,
                        native_id,
                        session_id,
                        provider_source,
                        provider_native_id
                    ],
                )?;
                resolved += 1;
            }
        }
        tx.commit()?;
        Ok(resolved)
    }

    pub fn lookup_session_by_orchestrator_id(
        &self,
        source: &str,
        native_id: &str,
    ) -> Result<Option<OrchestratorLookup>> {
        Ok(self
            .conn
            .query_row(
                "SELECT l.orchestrator_source,l.orchestrator_native_id,t.winner_host,
                    s.id,s.source,s.native_id
             FROM orchestrator_links l
             JOIN orchestrator_threads t ON t.source=l.orchestrator_source
                                         AND t.native_id=l.orchestrator_native_id
             JOIN sessions s ON s.id=l.session_id
             WHERE l.orchestrator_source=?1 AND l.orchestrator_native_id=?2",
                params![source, native_id],
                |row| {
                    Ok(OrchestratorLookup {
                        orchestrator_source: row.get(0)?,
                        orchestrator_native_id: row.get(1)?,
                        orchestrator_host: row.get(2)?,
                        session_id: row.get(3)?,
                        session_source: row.get(4)?,
                        session_native_id: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn augmentations_for_session(&self, session_id: i64) -> Result<Vec<Augmentation>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.source,t.native_id,t.winner_host,t.title,t.project_id,t.project_title,
                    t.workspace_root,t.provider_name,t.provider_instance_id,t.model,
                    t.runtime_mode,t.interaction_mode,t.branch,t.worktree,t.created_at_ms,
                    t.updated_at_ms,t.archived,t.pinned,t.settled,t.linked_pull_request,
                    t.turn_count,t.checkpoint_count
             FROM orchestrator_links l JOIN orchestrator_threads t
               ON t.source=l.orchestrator_source AND t.native_id=l.orchestrator_native_id
             WHERE l.session_id=?1 ORDER BY t.source,t.winner_host,t.native_id",
        )?;
        Ok(stmt
            .query_map([session_id], |row| {
                Ok(Augmentation {
                    source: row.get(0)?,
                    thread_id: row.get(1)?,
                    host: row.get(2)?,
                    title: row.get(3)?,
                    project_id: row.get(4)?,
                    project_title: row.get(5)?,
                    workspace_root: row.get(6)?,
                    provider_name: row.get(7)?,
                    provider_instance_id: row.get(8)?,
                    model: row.get(9)?,
                    runtime_mode: row.get(10)?,
                    interaction_mode: row.get(11)?,
                    branch: row.get(12)?,
                    worktree: row.get(13)?,
                    created_at_ms: row.get(14)?,
                    updated_at_ms: row.get(15)?,
                    archived: row.get(16)?,
                    pinned: row.get(17)?,
                    settled: row.get(18)?,
                    linked_pull_request: row.get(19)?,
                    turn_count: row.get::<_, i64>(20)? as u32,
                    checkpoint_count: row.get::<_, i64>(21)? as u32,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Compare resolved Orchestrator and provider Dialog without mutating
    /// either side. The report is diagnostic/audit evidence only.
    pub fn orchestrator_fidelity_reports(&self) -> Result<Vec<OrchestratorFidelityReport>> {
        let mut links = self.conn.prepare(
            "SELECT l.orchestrator_source,l.orchestrator_native_id,s.id,s.source,s.native_id
             FROM orchestrator_links l JOIN sessions s ON s.id=l.session_id
             ORDER BY l.orchestrator_source,l.orchestrator_native_id",
        )?;
        let rows = links
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut reports = Vec::with_capacity(rows.len());
        for (
            orchestrator_source,
            orchestrator_native_id,
            session_id,
            session_source,
            session_native_id,
        ) in rows
        {
            let orchestrator =
                self.orchestrator_messages(&orchestrator_source, &orchestrator_native_id)?;
            let provider = self.messages(session_id)?;
            let chronological = |mut stream: Vec<(Option<i64>, usize, String, String)>| {
                stream.sort_by_key(|(timestamp, index, _, _)| {
                    (timestamp.unwrap_or(i64::MAX), *index)
                });
                stream
                    .into_iter()
                    .map(|(_, _, role, text)| (role, text))
                    .collect::<Vec<_>>()
            };
            let orchestrator_stream = chronological(
                orchestrator
                    .iter()
                    .enumerate()
                    .map(|(index, message)| {
                        (
                            message.timestamp_ms,
                            index,
                            message.role.clone(),
                            message.text.clone(),
                        )
                    })
                    .collect(),
            );
            let provider_stream = chronological(
                provider
                    .iter()
                    .filter(|message| message.kind == crate::model::MessageKind::Dialog)
                    .enumerate()
                    .map(|(index, message)| {
                        (
                            message.timestamp_ms,
                            index,
                            message.role.as_str().to_owned(),
                            message.text.clone(),
                        )
                    })
                    .collect(),
            );
            let count_role = |stream: &[(String, String)], role: &str| {
                stream
                    .iter()
                    .filter(|(candidate, _)| *candidate == role)
                    .count()
            };
            reports.push(OrchestratorFidelityReport {
                orchestrator_source,
                orchestrator_native_id,
                session_source,
                session_native_id,
                orchestrator_user_messages: count_role(&orchestrator_stream, "user"),
                orchestrator_assistant_messages: count_role(&orchestrator_stream, "assistant"),
                provider_user_messages: count_role(&provider_stream, "user"),
                provider_assistant_messages: count_role(&provider_stream, "assistant"),
                ordered_role_text_equal: orchestrator_stream == provider_stream,
            });
        }
        Ok(reports)
    }

    /// Explain canonical Orchestrator facts that did not produce a Link.
    pub fn orchestrator_link_diagnostics(&self) -> Result<Vec<OrchestratorLinkDiagnostic>> {
        type Fact = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        );
        let mut stmt = self.conn.prepare(
            "SELECT f.source,f.native_id,t.winner_host,f.status,f.provider_source,
                    f.provider_native_id,f.reason,
                    EXISTS(SELECT 1 FROM orchestrator_links l
                           WHERE l.orchestrator_source=f.source
                             AND l.orchestrator_native_id=f.native_id)
             FROM orchestrator_link_facts f
             JOIN orchestrator_threads t ON t.source=f.source AND t.native_id=f.native_id
             ORDER BY f.source,f.native_id,f.idx",
        )?;
        let facts = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })?
            .collect::<Result<Vec<Fact>, _>>()?;
        let mut grouped = BTreeMap::<(String, String, String), Vec<Fact>>::new();
        for fact in facts {
            grouped
                .entry((fact.0.clone(), fact.1.clone(), fact.2.clone()))
                .or_default()
                .push(fact);
        }
        let mut diagnostics = Vec::new();
        for ((source, native_id, host), facts) in grouped {
            let candidates = facts
                .iter()
                .filter(|fact| fact.3 == "candidate")
                .filter_map(|fact| Some((fact.4.clone()?, fact.5.clone()?)))
                .collect::<BTreeSet<_>>();
            let resolved = facts.iter().any(|fact| fact.7);
            for fact in facts.iter().filter(|fact| fact.3 != "candidate") {
                diagnostics.push(OrchestratorLinkDiagnostic {
                    source: source.clone(),
                    native_id: native_id.clone(),
                    host: host.clone(),
                    status: fact.3.clone(),
                    reason: fact
                        .6
                        .clone()
                        .unwrap_or_else(|| "unresolved Link fact".into()),
                });
            }
            if candidates.len() > 1 {
                diagnostics.push(OrchestratorLinkDiagnostic {
                    source,
                    native_id,
                    host,
                    status: "ambiguous".into(),
                    reason: format!("{} conflicting provider identities", candidates.len()),
                });
            } else if candidates.len() == 1 && !resolved {
                let (provider_source, provider_native_id) =
                    candidates.into_iter().next().expect("one candidate");
                diagnostics.push(OrchestratorLinkDiagnostic {
                    source,
                    native_id,
                    host,
                    status: "pending".into(),
                    reason: format!(
                        "provider Session {provider_source}/{provider_native_id} is not captured"
                    ),
                });
            }
        }
        Ok(diagnostics)
    }

    fn dump_orchestrators(&self, out: &mut String) -> Result<()> {
        use std::fmt::Write as _;
        let mut projections = self.conn.prepare(
            "SELECT source,native_id,host,native_path,logical_revision_ms,discriminator,
                    adapter_version,title,project_id,project_title,workspace_root,provider_name,
                    provider_instance_id,model,runtime_mode,interaction_mode,branch,worktree,
                    created_at_ms,updated_at_ms,archived,pinned,settled,linked_pull_request,
                    checkpoint_count
             FROM orchestrator_thread_projections ORDER BY source,native_id,host",
        )?;
        let mut rows = projections.query([])?;
        while let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let native_id: String = row.get(1)?;
            let host: String = row.get(2)?;
            writeln!(
                out,
                "orchestrator-projection {source}/{native_id} host={host} path={:?} revision={} discriminator={} adapter={} title={:?} project=({:?},{:?},{:?}) provider=({:?},{:?},{:?}) modes=({:?},{:?}) git=({:?},{:?}) times=({:?},{:?}) state=({},{},{}) pr={:?} checkpoints={}",
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, Option<String>>(17)?,
                row.get::<_, Option<i64>>(18)?,
                row.get::<_, Option<i64>>(19)?,
                row.get::<_, bool>(20)?,
                row.get::<_, bool>(21)?,
                row.get::<_, bool>(22)?,
                row.get::<_, Option<String>>(23)?,
                row.get::<_, i64>(24)?
            )?;
            dump_orchestrator_projection_children(&self.conn, out, &source, &native_id, &host)?;
        }
        let mut threads = self.conn.prepare(
            "SELECT source,native_id,winner_host,logical_revision_ms,discriminator,
                    adapter_version,title,project_id,project_title,workspace_root,provider_name,
                    provider_instance_id,model,runtime_mode,interaction_mode,branch,worktree,
                    created_at_ms,updated_at_ms,archived,pinned,settled,linked_pull_request,
                    turn_count,checkpoint_count FROM orchestrator_threads ORDER BY source,native_id",
        )?;
        let mut rows = threads.query([])?;
        while let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let native_id: String = row.get(1)?;
            writeln!(
                out,
                "orchestrator {source}/{native_id} host={} revision={} discriminator={} adapter={} title={:?} project=({:?},{:?},{:?}) provider=({:?},{:?},{:?}) modes=({:?},{:?}) git=({:?},{:?}) times=({:?},{:?}) state=({},{},{}) pr={:?} turns={} checkpoints={}",
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, Option<i64>>(17)?,
                row.get::<_, Option<i64>>(18)?,
                row.get::<_, bool>(19)?,
                row.get::<_, bool>(20)?,
                row.get::<_, bool>(21)?,
                row.get::<_, Option<String>>(22)?,
                row.get::<_, i64>(23)?,
                row.get::<_, i64>(24)?
            )?;
            dump_orchestrator_children(&self.conn, out, &source, &native_id)?;
        }
        let mut links = self.conn.prepare(
            "SELECT orchestrator_source,orchestrator_native_id,provider_source,provider_native_id
             FROM orchestrator_links ORDER BY orchestrator_source,orchestrator_native_id",
        )?;
        let mut rows = links.query([])?;
        while let Some(row) = rows.next()? {
            writeln!(
                out,
                "orchestrator-link {}/{} -> {}/{}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?
            )?;
        }
        Ok(())
    }
}

fn materialize_orchestrator_winner(
    tx: &rusqlite::Transaction<'_>,
    source: &str,
    native_id: &str,
) -> Result<()> {
    let winner: String = tx.query_row(
        "SELECT host FROM orchestrator_thread_projections
         WHERE source=?1 AND native_id=?2
         ORDER BY CASE WHEN source='t3' AND adapter_version>=?3 THEN 1 ELSE 0 END DESC,
                  logical_revision_ms DESC, discriminator DESC, adapter_version DESC, host ASC
         LIMIT 1",
        params![source, native_id, crate::adapters::t3::V2_ADAPTER_VERSION],
        |row| row.get(0),
    )?;
    tx.execute(
        "DELETE FROM orchestrator_threads WHERE source=?1 AND native_id=?2",
        params![source, native_id],
    )?;
    tx.execute(
        "INSERT INTO orchestrator_threads
           (source,native_id,winner_host,native_path,logical_revision_ms,discriminator,
            adapter_version,title,project_id,project_title,workspace_root,provider_name,
            provider_instance_id,model,runtime_mode,interaction_mode,branch,worktree,
            created_at_ms,updated_at_ms,archived,pinned,settled,linked_pull_request,
            turn_count,checkpoint_count)
         SELECT source,native_id,host,native_path,logical_revision_ms,discriminator,
            adapter_version,title,project_id,project_title,workspace_root,provider_name,
            provider_instance_id,model,runtime_mode,interaction_mode,branch,worktree,
            created_at_ms,updated_at_ms,archived,pinned,settled,linked_pull_request,
            (SELECT COUNT(*) FROM orchestrator_projection_turns t
             WHERE t.source=p.source AND t.native_id=p.native_id AND t.host=p.host),
            checkpoint_count
         FROM orchestrator_thread_projections p
         WHERE source=?1 AND native_id=?2 AND host=?3",
        params![source, native_id, winner],
    )?;
    for (canonical, projection) in [
        (
            "orchestrator_link_facts",
            "orchestrator_projection_link_facts",
        ),
        ("orchestrator_messages", "orchestrator_projection_messages"),
        ("orchestrator_turns", "orchestrator_projection_turns"),
        (
            "orchestrator_activities",
            "orchestrator_projection_activities",
        ),
    ] {
        tx.execute(
            &format!("DELETE FROM {canonical} WHERE source=?1 AND native_id=?2"),
            params![source, native_id],
        )?;
        let columns = match canonical {
            "orchestrator_link_facts" => {
                "source,native_id,idx,status,provider_source,provider_native_id,reason"
            }
            "orchestrator_messages" => "source,native_id,idx,role,text,timestamp_ms",
            "orchestrator_turns" => {
                "source,native_id,idx,turn_native_id,state,checkpoint_id,payload_json"
            }
            _ => "source,native_id,idx,activity_native_id,kind,payload_json",
        };
        tx.execute(&format!("INSERT INTO {canonical} ({columns}) SELECT {columns} FROM {projection} WHERE source=?1 AND native_id=?2 AND host=?3 ORDER BY idx"),
            params![source,native_id,winner])?;
    }
    Ok(())
}

fn dump_orchestrator_children(
    conn: &Connection,
    out: &mut String,
    source: &str,
    native_id: &str,
) -> Result<()> {
    use std::fmt::Write as _;
    for (table, label, fields) in [
        (
            "orchestrator_link_facts",
            "orchestrator-link-fact",
            "idx,status,provider_source,provider_native_id,reason",
        ),
        (
            "orchestrator_messages",
            "orchestrator-message",
            "idx,role,text,CAST(timestamp_ms AS TEXT),NULL",
        ),
        (
            "orchestrator_turns",
            "orchestrator-turn",
            "idx,turn_native_id,state,checkpoint_id,payload_json",
        ),
        (
            "orchestrator_activities",
            "orchestrator-activity",
            "idx,activity_native_id,kind,payload_json,NULL",
        ),
    ] {
        let mut stmt = conn.prepare(&format!(
            "SELECT {fields} FROM {table} WHERE source=?1 AND native_id=?2 ORDER BY idx"
        ))?;
        let mut rows = stmt.query(params![source, native_id])?;
        while let Some(row) = rows.next()? {
            writeln!(
                out,
                "  {label} {} {:?} {:?} {:?} {:?}",
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?
            )?;
        }
    }
    Ok(())
}

fn dump_orchestrator_projection_children(
    conn: &Connection,
    out: &mut String,
    source: &str,
    native_id: &str,
    host: &str,
) -> Result<()> {
    use std::fmt::Write as _;
    for (table, label, fields) in [
        (
            "orchestrator_projection_link_facts",
            "orchestrator-projection-link-fact",
            "idx,status,provider_source,provider_native_id,reason",
        ),
        (
            "orchestrator_projection_messages",
            "orchestrator-projection-message",
            "idx,role,text,CAST(timestamp_ms AS TEXT),NULL",
        ),
        (
            "orchestrator_projection_turns",
            "orchestrator-projection-turn",
            "idx,turn_native_id,state,checkpoint_id,payload_json",
        ),
        (
            "orchestrator_projection_activities",
            "orchestrator-projection-activity",
            "idx,activity_native_id,kind,payload_json,NULL",
        ),
    ] {
        let mut stmt = conn.prepare(&format!(
            "SELECT {fields} FROM {table} WHERE source=?1 AND native_id=?2 AND host=?3 ORDER BY idx"
        ))?;
        let mut rows = stmt.query(params![source, native_id, host])?;
        while let Some(row) = rows.next()? {
            writeln!(
                out,
                "  {label} {} {:?} {:?} {:?} {:?}",
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?
            )?;
        }
    }
    Ok(())
}
