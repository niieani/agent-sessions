-- Capture allowlist DDL from T3 Store migrations through 67 (2026-10-21).
-- Auth, runtime effects, caches and other excluded tables are intentionally absent.
CREATE TABLE projection_projects (
      project_id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      workspace_root TEXT NOT NULL,
      scripts_json TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      deleted_at TEXT
    , default_model_selection_json TEXT, default_thread_env_mode TEXT, favicon_path TEXT, auto_pull INTEGER NOT NULL DEFAULT 0, project_icon_json TEXT);
CREATE INDEX idx_projection_projects_updated_at
    ON projection_projects(updated_at)
  ;
CREATE INDEX idx_projection_projects_workspace_root_deleted_at
    ON projection_projects(workspace_root, deleted_at)
  ;
CREATE TABLE orchestration_v2_projection_threads (
      thread_id TEXT PRIMARY KEY,
      project_id TEXT NOT NULL,
      title TEXT NOT NULL,
      default_provider TEXT NOT NULL,
      runtime_mode TEXT NOT NULL,
      interaction_mode TEXT NOT NULL,
      active_provider_thread_id TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      archived_at TEXT,
      deleted_at TEXT,
      payload_json TEXT NOT NULL
    , provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_threads_project_updated_idx ON orchestration_v2_projection_threads(project_id, updated_at);
CREATE TABLE orchestration_v2_projection_runs (
      run_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      ordinal INTEGER NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT,
      status TEXT NOT NULL,
      requested_at TEXT NOT NULL,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    , provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_runs_provider_thread_idx ON orchestration_v2_projection_runs(provider_thread_id);
CREATE INDEX orchestration_v2_projection_runs_recovery_idx
    ON orchestration_v2_projection_runs(status, thread_id)
    WHERE status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
  ;
CREATE UNIQUE INDEX orchestration_v2_projection_runs_thread_ordinal_idx ON orchestration_v2_projection_runs(thread_id, ordinal);
CREATE INDEX orchestration_v2_projection_runs_thread_status_idx ON orchestration_v2_projection_runs(thread_id, status);
CREATE TABLE orchestration_v2_projection_run_attempts (
      attempt_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT NOT NULL,
      attempt_ordinal INTEGER NOT NULL,
      root_node_id TEXT NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT NOT NULL,
      provider_turn_id TEXT,
      status TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , provider_instance_id TEXT);
CREATE UNIQUE INDEX orchestration_v2_projection_run_attempts_run_ordinal_idx ON orchestration_v2_projection_run_attempts(run_id, attempt_ordinal);
CREATE INDEX orchestration_v2_projection_run_attempts_thread_idx ON orchestration_v2_projection_run_attempts(thread_id, run_id);
CREATE TABLE orchestration_v2_projection_nodes (
      node_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      parent_node_id TEXT,
      root_node_id TEXT NOT NULL,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      provider_thread_id TEXT,
      provider_turn_id TEXT,
      runtime_request_id TEXT,
      checkpoint_scope_id TEXT,
      started_at TEXT,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_nodes_live_idx
    ON orchestration_v2_projection_nodes(thread_id)
    WHERE status IN ('pending', 'starting', 'running', 'waiting')
  ;
CREATE INDEX orchestration_v2_projection_nodes_parent_idx ON orchestration_v2_projection_nodes(parent_node_id);
CREATE INDEX orchestration_v2_projection_nodes_provider_turn_idx ON orchestration_v2_projection_nodes(provider_turn_id);
CREATE INDEX orchestration_v2_projection_nodes_thread_run_idx ON orchestration_v2_projection_nodes(thread_id, run_id);
CREATE TABLE orchestration_v2_projection_provider_sessions (
      provider_session_id TEXT PRIMARY KEY,
      thread_id TEXT,
      provider TEXT NOT NULL,
      status TEXT NOT NULL,
      model TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , driver TEXT, provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_provider_sessions_instance_status_idx ON orchestration_v2_projection_provider_sessions(provider_instance_id, status);
CREATE INDEX orchestration_v2_projection_provider_sessions_provider_status_idx ON orchestration_v2_projection_provider_sessions(provider, status);
CREATE INDEX orchestration_v2_projection_provider_sessions_thread_idx ON orchestration_v2_projection_provider_sessions(thread_id);
CREATE TABLE orchestration_v2_projection_provider_threads (
      provider_thread_id TEXT PRIMARY KEY,
      thread_id TEXT,
      owner_node_id TEXT,
      provider TEXT NOT NULL,
      provider_session_id TEXT,
      status TEXT NOT NULL,
      first_run_ordinal INTEGER,
      last_run_ordinal INTEGER,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , driver TEXT, provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_provider_threads_instance_status_idx ON orchestration_v2_projection_provider_threads(provider_instance_id, status);
CREATE INDEX orchestration_v2_projection_provider_threads_owner_idx ON orchestration_v2_projection_provider_threads(owner_node_id);
CREATE INDEX orchestration_v2_projection_provider_threads_session_idx ON orchestration_v2_projection_provider_threads(provider_session_id);
CREATE INDEX orchestration_v2_projection_provider_threads_thread_idx ON orchestration_v2_projection_provider_threads(thread_id);
CREATE TABLE orchestration_v2_projection_provider_turns (
      provider_turn_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      provider_thread_id TEXT NOT NULL,
      node_id TEXT NOT NULL,
      run_attempt_id TEXT,
      ordinal INTEGER NOT NULL,
      status TEXT NOT NULL,
      started_at TEXT,
      completed_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_provider_turns_thread_idx ON orchestration_v2_projection_provider_turns(thread_id);
CREATE UNIQUE INDEX orchestration_v2_projection_provider_turns_thread_ordinal_idx ON orchestration_v2_projection_provider_turns(provider_thread_id, ordinal);
CREATE INDEX orchestration_v2_provider_turns_codex_native_id_idx
    ON orchestration_v2_projection_provider_turns (
      json_extract(payload_json, '$.nativeTurnRef.nativeId')
    )
    WHERE json_extract(payload_json, '$.nativeTurnRef.driver') = 'codex'
  ;
CREATE TABLE orchestration_v2_projection_runtime_requests (
      runtime_request_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      node_id TEXT NOT NULL,
      provider_turn_id TEXT,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      created_at TEXT NOT NULL,
      resolved_at TEXT,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_requests_recovery_idx
    ON orchestration_v2_projection_runtime_requests(thread_id)
    WHERE status = 'pending'
  ;
CREATE INDEX orchestration_v2_projection_runtime_requests_provider_turn_idx ON orchestration_v2_projection_runtime_requests(provider_turn_id);
CREATE INDEX orchestration_v2_projection_runtime_requests_thread_status_idx ON orchestration_v2_projection_runtime_requests(thread_id, status);
CREATE TABLE orchestration_v2_projection_messages (
      message_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      role TEXT NOT NULL,
      streaming INTEGER NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_messages_latest_user_idx
    ON orchestration_v2_projection_messages(thread_id, updated_at DESC, message_id DESC)
    WHERE role = 'user'
  ;
CREATE INDEX orchestration_v2_projection_messages_node_idx ON orchestration_v2_projection_messages(node_id);
CREATE INDEX orchestration_v2_projection_messages_run_idx ON orchestration_v2_projection_messages(run_id);
CREATE INDEX orchestration_v2_projection_messages_thread_created_idx ON orchestration_v2_projection_messages(thread_id, created_at, message_id);
CREATE TABLE orchestration_v2_projection_plans (
      plan_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      kind TEXT NOT NULL,
      status TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_plans_run_idx ON orchestration_v2_projection_plans(run_id);
CREATE INDEX orchestration_v2_projection_plans_thread_idx ON orchestration_v2_projection_plans(thread_id);
CREATE TABLE orchestration_v2_projection_turn_items (
      turn_item_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      provider_thread_id TEXT,
      provider_turn_id TEXT,
      parent_item_id TEXT,
      ordinal INTEGER NOT NULL,
      type TEXT NOT NULL,
      status TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_turn_items_failed_error_idx
    ON orchestration_v2_projection_turn_items(
      thread_id, run_id, node_id, updated_at DESC, ordinal DESC, turn_item_id DESC
    )
    WHERE type = 'error' AND status = 'failed'
  ;
CREATE INDEX orchestration_v2_projection_turn_items_node_ordinal_idx ON orchestration_v2_projection_turn_items(node_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_provider_turn_idx ON orchestration_v2_projection_turn_items(provider_turn_id);
CREATE INDEX orchestration_v2_projection_turn_items_recovery_idx
    ON orchestration_v2_projection_turn_items(thread_id)
    WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
      AND status IN ('pending', 'running', 'waiting')
  ;
CREATE INDEX orchestration_v2_projection_turn_items_run_ordinal_idx ON orchestration_v2_projection_turn_items(run_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_shell_pending_idx
    ON orchestration_v2_projection_turn_items(thread_id, run_id)
    WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
      AND status NOT IN ('completed', 'interrupted', 'failed', 'cancelled')
  ;
CREATE INDEX orchestration_v2_projection_turn_items_thread_ordinal_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id);
CREATE INDEX orchestration_v2_projection_turn_items_thread_run_idx
    ON orchestration_v2_projection_turn_items(thread_id, run_id)
  ;
CREATE INDEX orchestration_v2_projection_turn_items_user_message_idx
    ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id)
    WHERE type = 'user_message'
  ;
CREATE TABLE orchestration_v2_projection_checkpoint_scopes (
      scope_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      parent_scope_id TEXT,
      provider_thread_id TEXT,
      kind TEXT NOT NULL,
      ordinal_within_parent INTEGER NOT NULL,
      advances_app_run_count INTEGER NOT NULL,
      created_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_checkpoint_scopes_parent_idx ON orchestration_v2_projection_checkpoint_scopes(parent_scope_id);
CREATE INDEX orchestration_v2_projection_checkpoint_scopes_thread_idx ON orchestration_v2_projection_checkpoint_scopes(thread_id);
CREATE TABLE orchestration_v2_projection_checkpoints (
      checkpoint_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      scope_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT NOT NULL,
      parent_checkpoint_id TEXT,
      ordinal_within_scope INTEGER NOT NULL,
      app_run_ordinal INTEGER,
      status TEXT NOT NULL,
      captured_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_checkpoints_parent_idx ON orchestration_v2_projection_checkpoints(parent_checkpoint_id);
CREATE UNIQUE INDEX orchestration_v2_projection_checkpoints_scope_ordinal_idx ON orchestration_v2_projection_checkpoints(scope_id, ordinal_within_scope);
CREATE INDEX orchestration_v2_projection_checkpoints_thread_idx ON orchestration_v2_projection_checkpoints(thread_id);
CREATE TABLE orchestration_v2_projection_context_handoffs (
      context_handoff_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      target_run_id TEXT NOT NULL,
      to_provider_thread_id TEXT NOT NULL,
      strategy TEXT NOT NULL,
      status TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    );
CREATE INDEX orchestration_v2_projection_context_handoffs_target_run_idx ON orchestration_v2_projection_context_handoffs(target_run_id);
CREATE INDEX orchestration_v2_projection_context_handoffs_thread_idx ON orchestration_v2_projection_context_handoffs(thread_id);
CREATE TABLE orchestration_v2_projection_context_transfers (
      context_transfer_id TEXT PRIMARY KEY,
      source_thread_id TEXT NOT NULL,
      target_thread_id TEXT NOT NULL,
      target_run_id TEXT,
      type TEXT NOT NULL,
      status TEXT NOT NULL,
      source_provider TEXT,
      target_provider TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , source_provider_instance_id TEXT, target_provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_context_transfers_source_thread_idx ON orchestration_v2_projection_context_transfers(source_thread_id);
CREATE INDEX orchestration_v2_projection_context_transfers_target_run_idx ON orchestration_v2_projection_context_transfers(target_run_id);
CREATE INDEX orchestration_v2_projection_context_transfers_target_thread_idx ON orchestration_v2_projection_context_transfers(target_thread_id, status);
CREATE TABLE orchestration_v2_projection_subagents (
      subagent_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      parent_node_id TEXT NOT NULL,
      provider TEXT NOT NULL,
      provider_thread_id TEXT,
      child_thread_id TEXT,
      origin TEXT NOT NULL,
      status TEXT NOT NULL,
      started_at TEXT,
      completed_at TEXT,
      updated_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , driver TEXT, provider_instance_id TEXT);
CREATE INDEX orchestration_v2_projection_subagents_child_thread_idx ON orchestration_v2_projection_subagents(child_thread_id);
CREATE INDEX orchestration_v2_projection_subagents_parent_node_idx ON orchestration_v2_projection_subagents(parent_node_id);
CREATE INDEX orchestration_v2_projection_subagents_provider_thread_idx ON orchestration_v2_projection_subagents(provider_thread_id);
CREATE INDEX orchestration_v2_projection_subagents_thread_idx ON orchestration_v2_projection_subagents(thread_id, started_at, subagent_id);
CREATE TABLE orchestration_v2_projection_provider_session_bindings (
      provider_session_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      PRIMARY KEY (provider_session_id, thread_id)
    );
CREATE INDEX orchestration_v2_projection_provider_session_bindings_thread_idx
    ON orchestration_v2_projection_provider_session_bindings(thread_id)
  ;
CREATE TABLE orchestration_v2_events (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id TEXT NOT NULL UNIQUE,
      command_id TEXT,
      thread_id TEXT NOT NULL,
      run_id TEXT,
      node_id TEXT,
      provider TEXT,
      raw_event_id TEXT,
      event_type TEXT NOT NULL,
      occurred_at TEXT NOT NULL,
      payload_json TEXT NOT NULL
    , driver TEXT, provider_instance_id TEXT);
CREATE INDEX orchestration_v2_events_command_idx ON orchestration_v2_events(command_id, sequence);
CREATE INDEX orchestration_v2_events_instance_sequence_idx ON orchestration_v2_events(provider_instance_id, sequence);
CREATE INDEX orchestration_v2_events_node_sequence_idx ON orchestration_v2_events(node_id, sequence);
CREATE INDEX orchestration_v2_events_raw_event_idx ON orchestration_v2_events(raw_event_id);
CREATE INDEX orchestration_v2_events_run_sequence_idx ON orchestration_v2_events(run_id, sequence);
CREATE INDEX orchestration_v2_events_thread_sequence_idx ON orchestration_v2_events(thread_id, sequence);
CREATE INDEX orchestration_v2_events_thread_type_sequence_idx ON orchestration_v2_events(thread_id, event_type, sequence);
CREATE TABLE orchestration_events (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id TEXT NOT NULL UNIQUE,
      aggregate_kind TEXT NOT NULL,
      stream_id TEXT NOT NULL,
      stream_version INTEGER NOT NULL,
      event_type TEXT NOT NULL,
      occurred_at TEXT NOT NULL,
      command_id TEXT,
      causation_event_id TEXT,
      correlation_id TEXT,
      actor_kind TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      metadata_json TEXT NOT NULL
    , application_event_version INTEGER NOT NULL DEFAULT 1);
CREATE INDEX idx_orch_events_command_id
    ON orchestration_events(command_id)
  ;
CREATE INDEX idx_orch_events_correlation_id
    ON orchestration_events(correlation_id)
  ;
CREATE INDEX idx_orch_events_stream_sequence
    ON orchestration_events(aggregate_kind, stream_id, sequence)
  ;
CREATE UNIQUE INDEX idx_orch_events_stream_version
    ON orchestration_events(aggregate_kind, stream_id, stream_version)
  ;
CREATE INDEX idx_orchestration_events_agent_stream_sequence
    ON orchestration_events(stream_id, sequence)
    WHERE application_event_version = 2 AND aggregate_kind = 'thread'
  ;
CREATE INDEX idx_orchestration_events_application_high_water
    ON orchestration_events(sequence)
    WHERE aggregate_kind = 'project'
      OR (application_event_version = 2 AND aggregate_kind = 'thread')
  ;
CREATE INDEX idx_orchestration_events_application_sequence
    ON orchestration_events(application_event_version, sequence)
  ;
CREATE INDEX orchestration_events_v2_created_threads_idx
    ON orchestration_events(stream_id)
    WHERE application_event_version = 2
      AND aggregate_kind = 'thread'
      AND event_type = 'thread.created'
  ;
CREATE TABLE orchestration_v2_legacy_imports (
      thread_id TEXT PRIMARY KEY,
      source_updated_at TEXT NOT NULL,
      shell_imported_at TEXT NOT NULL,
      transcript_imported_at TEXT,
      imported_message_count INTEGER NOT NULL DEFAULT 0,
      last_error TEXT
    );
CREATE INDEX orchestration_v2_legacy_imports_pending_transcript_idx
    ON orchestration_v2_legacy_imports(transcript_imported_at, shell_imported_at, thread_id)
  ;
