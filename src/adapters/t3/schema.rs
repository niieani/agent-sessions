//! Authoritative live v2 schema. Historical Capture decoding is separate.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct RowQuery<'a> {
    pub table: &'static str,
    pub predicate: &'static str,
    pub id: &'a str,
    pub order: &'static str,
    pub primary_key: &'static [&'static str],
}

// Explicit allowlist: never enumerate arbitrary tables (auth and effects are excluded).
const THREAD_TABLES: &[(&str, &[&str], &str)] = &[
    (
        "orchestration_v2_projection_threads",
        &["thread_id"],
        "thread_id",
    ),
    (
        "orchestration_v2_projection_provider_threads",
        &["provider_thread_id"],
        "provider_thread_id",
    ),
    (
        "orchestration_v2_projection_runs",
        &["run_id"],
        "ordinal,run_id",
    ),
    (
        "orchestration_v2_projection_run_attempts",
        &["attempt_id"],
        "run_id,attempt_ordinal,attempt_id",
    ),
    ("orchestration_v2_projection_nodes", &["node_id"], "node_id"),
    (
        "orchestration_v2_projection_provider_turns",
        &["provider_turn_id"],
        "provider_thread_id,ordinal,provider_turn_id",
    ),
    (
        "orchestration_v2_projection_runtime_requests",
        &["runtime_request_id"],
        "created_at,runtime_request_id",
    ),
    (
        "orchestration_v2_projection_messages",
        &["message_id"],
        "created_at,message_id",
    ),
    ("orchestration_v2_projection_plans", &["plan_id"], "plan_id"),
    (
        "orchestration_v2_projection_turn_items",
        &["turn_item_id"],
        "ordinal,turn_item_id",
    ),
    (
        "orchestration_v2_projection_checkpoint_scopes",
        &["scope_id"],
        "scope_id",
    ),
    (
        "orchestration_v2_projection_checkpoints",
        &["checkpoint_id"],
        "captured_at,checkpoint_id",
    ),
    (
        "orchestration_v2_projection_context_handoffs",
        &["context_handoff_id"],
        "context_handoff_id",
    ),
    (
        "orchestration_v2_projection_subagents",
        &["subagent_id"],
        "subagent_id",
    ),
];

pub(super) fn row_queries<'a>(thread_id: &'a str, project_id: &'a str) -> Vec<RowQuery<'a>> {
    let mut queries = vec![RowQuery {
        table: "projection_projects",
        predicate: "project_id=?1",
        id: project_id,
        order: "project_id",
        primary_key: &["project_id"],
    }];
    for &(table, primary_key, order) in THREAD_TABLES {
        queries.push(RowQuery {
            table,
            predicate: "thread_id=?1",
            id: thread_id,
            order,
            primary_key,
        });
    }
    queries.extend([
        RowQuery { table: "orchestration_v2_legacy_imports", predicate: "thread_id=?1", id: thread_id, order: "thread_id", primary_key: &["thread_id"] },
        RowQuery { table: "orchestration_v2_events", predicate: "thread_id=?1", id: thread_id, order: "sequence", primary_key: &["sequence"] },
        RowQuery { table: "orchestration_events", predicate: "aggregate_kind='project' AND stream_id=?1", id: project_id, order: "sequence", primary_key: &["sequence"] },
        RowQuery { table: "orchestration_v2_projection_provider_session_bindings", predicate: "thread_id=?1", id: thread_id, order: "provider_session_id,thread_id", primary_key: &["provider_session_id","thread_id"] },
        RowQuery { table: "orchestration_v2_projection_provider_sessions", predicate: "thread_id=?1 OR provider_session_id IN (SELECT provider_session_id FROM orchestration_v2_projection_provider_session_bindings WHERE thread_id=?1)", id: thread_id, order: "provider_session_id", primary_key: &["provider_session_id"] },
        RowQuery { table: "orchestration_v2_projection_context_transfers", predicate: "source_thread_id=?1 OR target_thread_id=?1", id: thread_id, order: "context_transfer_id", primary_key: &["context_transfer_id"] },
    ]);
    queries
}

pub(crate) fn validate_schema(conn: &Connection) -> Result<()> {
    for query in row_queries("", "") {
        let columns = table_columns(conn, query.table)?;
        ensure!(
            !columns.is_empty(),
            "incompatible T3 orchestration v2 Store: missing table {}",
            query.table
        );
        let mut required = query.primary_key.to_vec();
        // All columns referenced by predicates, ordering, or projection decoding.
        required.extend(match query.table {
            "orchestration_v2_legacy_imports" => vec![
                "source_updated_at",
                "shell_imported_at",
                "transcript_imported_at",
                "imported_message_count",
                "last_error",
            ],
            "projection_projects" => vec!["title", "workspace_root"],
            "orchestration_events" => vec!["aggregate_kind", "stream_id", "occurred_at"],
            "orchestration_v2_projection_context_transfers" => vec![
                "source_thread_id",
                "target_thread_id",
                "payload_json",
                "updated_at",
            ],
            "orchestration_v2_projection_provider_session_bindings" => vec![],
            _ => vec!["thread_id", "payload_json"],
        });
        required.extend(match query.table {
            "orchestration_v2_projection_threads" => vec![
                "project_id",
                "title",
                "active_provider_thread_id",
                "created_at",
                "updated_at",
                "default_provider",
                "runtime_mode",
                "interaction_mode",
                "archived_at",
                "deleted_at",
            ],
            "orchestration_v2_projection_provider_threads" => {
                vec!["provider", "owner_node_id", "updated_at"]
            }
            "orchestration_v2_projection_messages" => vec!["role", "created_at", "updated_at"],
            "orchestration_v2_projection_runs" => vec!["ordinal", "requested_at", "status"],
            "orchestration_v2_projection_run_attempts" => vec!["run_id", "attempt_ordinal"],
            "orchestration_v2_projection_provider_turns" => vec!["provider_thread_id", "ordinal"],
            "orchestration_v2_projection_runtime_requests" => vec!["created_at"],
            "orchestration_v2_projection_turn_items" => vec!["ordinal"],
            "orchestration_v2_projection_checkpoints" => vec!["captured_at"],
            "orchestration_v2_events" => vec!["occurred_at"],
            _ => vec![],
        });
        for column in required {
            ensure!(
                columns.iter().any(|c| c == column),
                "incompatible T3 orchestration v2 Store: {} missing column {column}",
                query.table
            );
        }
    }
    Ok(())
}
