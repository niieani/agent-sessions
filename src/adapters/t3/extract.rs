use super::*;

pub fn extract_projection(capture: &Capture) -> Result<OrchestratorProjection> {
    ensure!(capture.meta.source == SOURCE, "not a T3 Capture");
    let file = std::fs::File::open(&capture.body_path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    let mut project = None;
    let mut repository_urls = std::collections::BTreeSet::new();
    let mut thread = None;
    let mut runtime = None;
    let mut imported = false;
    let mut v2_links = Vec::new();
    let mut messages = Vec::new();
    let mut turns = Vec::new();
    let mut activities = Vec::new();
    let mut checkpoint_count = 0_u32;
    let mut v2_checkpoint_count = None;
    for line in BufReader::new(decoder).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        if value["record"] != "row" {
            continue;
        }
        let table = value["table"].as_str().context("T3 row lacks table")?;
        if table == "orchestration_v2_projection_threads" {
            v2_checkpoint_count = Some(0_u32);
        }
        if table == "orchestration_v2_projection_checkpoints" {
            v2_checkpoint_count = Some(v2_checkpoint_count.unwrap_or(0).saturating_add(1));
        }
        if table == "orchestration_v2_legacy_imports" {
            imported = true;
        }
        let mut columns = captured_column_map(&value)?;
        if matches!(table, "projection_projects" | "orchestration_events") {
            collect_repository_urls(&columns, &mut repository_urls);
        }
        let table = decode_v2_row(table, &mut columns, &mut v2_links)?;
        match table {
            "projection_projects" => project = Some(columns),
            "projection_threads" => thread = Some(columns),
            "provider_session_runtime" => runtime = Some(columns),
            "projection_thread_messages" => messages.push(OrchestratorMessage {
                role: text(&columns, "role").unwrap_or_default(),
                text: text(&columns, "text").unwrap_or_default(),
                timestamp_ms: timestamp(&columns, "created_at"),
            }),
            "projection_turns" => turns.push(OrchestratorTurn {
                native_id: text(&columns, "turn_id")
                    .or_else(|| scalar_string(&columns, "row_id"))
                    .unwrap_or_default(),
                state: text(&columns, "state"),
                checkpoint_id: text(&columns, "checkpoint_ref"),
                payload_json: Some(serde_json::to_string(&columns)?),
            }),
            "projection_thread_activities" => activities.push(OrchestratorActivity {
                native_id: text(&columns, "activity_id").unwrap_or_default(),
                kind: text(&columns, "kind"),
                payload_json: text(&columns, "payload_json"),
            }),
            "checkpoint_diff_blobs" => checkpoint_count = checkpoint_count.saturating_add(1),
            _ => {}
        }
    }
    let project = project.context("T3 Capture lacks Project projection")?;
    let thread = thread.context("T3 Capture lacks Thread projection")?;
    let mut link_facts = if v2_links.is_empty() {
        runtime.as_ref().map(link_fact).into_iter().collect()
    } else {
        v2_links
    };
    if imported
        && link_facts.is_empty()
        && let Some(prior) = legacy::latest_legacy(capture)?
    {
        link_facts = prior.link_facts;
        for message in prior.messages {
            if !messages.iter().any(|m| {
                m.role == message.role
                    && m.text == message.text
                    && m.timestamp_ms == message.timestamp_ms
            }) {
                messages.push(message);
            }
        }
        messages.sort_by_key(|m| m.timestamp_ms);
    }
    Ok(OrchestratorProjection {
        repository_url: (repository_urls.len() == 1)
            .then(|| repository_urls.into_iter().next().unwrap()),
        source: SOURCE.into(),
        native_id: capture.meta.native_id.clone(),
        host: capture.meta.host.clone(),
        native_path: capture.meta.native_path.clone(),
        logical_revision_ms: capture.meta.source_mtime_ms,
        discriminator: capture.meta.source_size,
        adapter_version: capture.meta.adapter_version,
        title: text(&thread, "title"),
        project_id: text(&project, "project_id"),
        project_title: text(&project, "title"),
        workspace_root: text(&project, "workspace_root"),
        provider_name: runtime
            .as_ref()
            .and_then(|row| text(row, "provider_name"))
            .or_else(|| text(&thread, "default_provider")),
        provider_instance_id: runtime
            .as_ref()
            .and_then(|row| text(row, "provider_instance_id"))
            .or_else(|| text(&thread, "provider_instance_id")),
        model: text(&thread, "model_selection_json").or_else(|| text(&thread, "model")),
        runtime_mode: text(&thread, "runtime_mode")
            .or_else(|| runtime.as_ref().and_then(|row| text(row, "runtime_mode"))),
        interaction_mode: text(&thread, "interaction_mode"),
        branch: text(&thread, "branch"),
        worktree: text(&thread, "worktree_path"),
        created_at_ms: timestamp(&thread, "created_at"),
        updated_at_ms: timestamp(&thread, "updated_at"),
        archived: present(&thread, "archived_at"),
        pinned: present(&thread, "pinned_at"),
        settled: present(&thread, "settled_at")
            || text(&thread, "settled_override").is_some_and(|value| value == "settled"),
        linked_pull_request: text(&thread, "linked_pull_request_json"),
        checkpoint_count: v2_checkpoint_count.unwrap_or(checkpoint_count),
        link_facts,
        messages,
        turns,
        activities,
    })
}

fn captured_column_map(row: &Value) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for column in row["columns"].as_array().context("T3 row lacks columns")? {
        let name = column["name"].as_str().context("T3 column lacks name")?;
        out.insert(name.into(), column["value"].clone());
    }
    Ok(out)
}

fn text(row: &BTreeMap<String, Value>, name: &str) -> Option<String> {
    row.get(name)?.as_str().map(str::to_owned)
}

fn scalar_string(row: &BTreeMap<String, Value>, name: &str) -> Option<String> {
    let value = row.get(name)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}

fn timestamp(row: &BTreeMap<String, Value>, name: &str) -> Option<i64> {
    let value = row.get(name)?;
    value.as_i64().or_else(|| {
        value
            .as_str()
            .and_then(|value| value.parse().ok().or_else(|| parse_iso_ms(value).ok()))
    })
}

fn present(row: &BTreeMap<String, Value>, name: &str) -> bool {
    row.get(name).is_some_and(|value| !value.is_null())
}

pub(super) fn link_fact(runtime: &BTreeMap<String, Value>) -> OrchestratorLinkFact {
    let provider = text(runtime, "provider_name");
    let raw = text(runtime, "resume_cursor_json");
    let malformed = |reason: &str| OrchestratorLinkFact {
        status: OrchestratorLinkStatus::Malformed,
        provider_source: None,
        provider_native_id: None,
        reason: Some(reason.into()),
    };
    let unsupported = |reason: String| OrchestratorLinkFact {
        status: OrchestratorLinkStatus::Unsupported,
        provider_source: None,
        provider_native_id: None,
        reason: Some(reason),
    };
    let Some(provider) = provider else {
        return malformed("missing provider_name");
    };
    let Some(raw) = raw else {
        return malformed("missing resume_cursor_json");
    };
    let Ok(cursor) = serde_json::from_str::<Value>(&raw) else {
        return malformed("malformed resume_cursor_json");
    };
    let Some(cursor) = cursor.as_object() else {
        return malformed("provider resume cursor must be an object");
    };
    let candidate = match provider.as_str() {
        "claudeAgent"
            if cursor.len() == 4
                && ["threadId", "resume", "resumeSessionAt", "turnCount"]
                    .iter()
                    .all(|key| cursor.contains_key(*key))
                && cursor
                    .get("threadId")
                    .and_then(Value::as_str)
                    .is_some_and(is_uuid)
                && cursor
                    .get("resumeSessionAt")
                    .and_then(Value::as_str)
                    .is_some_and(is_uuid)
                && cursor.get("turnCount").and_then(Value::as_u64).is_some() =>
        {
            cursor
                .get("resume")
                .and_then(Value::as_str)
                .filter(|value| is_uuid(value))
                .map(|value| ("claude", value))
        }
        "codex" if cursor.len() == 1 => cursor
            .get("threadId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| ("codex", value)),
        "claudeAgent" | "codex" => None,
        "cursor" | "grok" | "opencode" => {
            return unsupported(format!("unsupported T3 provider {provider}"));
        }
        _ => return unsupported(format!("unknown T3 provider {provider}")),
    };
    let Some((source, native_id)) = candidate else {
        return malformed("unsupported or empty provider resume cursor shape");
    };
    OrchestratorLinkFact {
        status: OrchestratorLinkStatus::Candidate,
        provider_source: Some(source.into()),
        provider_native_id: Some(native_id.into()),
        reason: None,
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

// Old Capture tables remain decodable: archives must rebuild without live Stores.
fn decode_v2_row<'a>(
    table: &'a str,
    columns: &mut BTreeMap<String, Value>,
    links: &mut Vec<OrchestratorLinkFact>,
) -> Result<&'a str> {
    if !table.starts_with("orchestration_v2_projection_") {
        return Ok(table);
    }
    if table == "orchestration_v2_projection_provider_session_bindings" {
        return Ok(table);
    }
    let payload: Value = serde_json::from_str(
        &text(columns, "payload_json").context("T3 v2 Capture lacks payload_json")?,
    )
    .with_context(|| format!("incompatible T3 v2 {table} payload_json"))?;
    ensure!(
        payload.is_object(),
        "incompatible T3 v2 {table}: payload must be an object"
    );
    match table {
        "orchestration_v2_projection_threads" => {
            for (native, canonical) in [
                ("runtimeMode", "runtime_mode"),
                ("interactionMode", "interaction_mode"),
                ("branch", "branch"),
                ("worktreePath", "worktree_path"),
                ("pinnedAt", "pinned_at"),
                ("settledAt", "settled_at"),
                ("settledOverride", "settled_override"),
                ("providerInstanceId", "provider_instance_id"),
            ] {
                if let Some(value) = payload.get(native) {
                    columns.insert(canonical.into(), value.clone());
                }
            }
            for (native, canonical) in [
                ("modelSelection", "model_selection_json"),
                ("linkedPullRequest", "linked_pull_request_json"),
            ] {
                if let Some(value) = payload.get(native).filter(|v| !v.is_null()) {
                    columns.insert(
                        canonical.into(),
                        Value::String(serde_json::to_string(value)?),
                    );
                }
            }
            Ok("projection_threads")
        }
        "orchestration_v2_projection_provider_threads" => {
            // Native children are not the enclosing app Thread's Session.
            if !present(columns, "owner_node_id") {
                links.push(v2_link(&payload));
            }
            Ok(table)
        }
        "orchestration_v2_projection_messages" => {
            let message = payload["text"]
                .as_str()
                .context("incompatible T3 v2 message: missing text")?;
            columns.insert("text".into(), Value::String(message.into()));
            Ok("projection_thread_messages")
        }
        "orchestration_v2_projection_runs" => {
            columns.insert(
                "turn_id".into(),
                columns
                    .get("run_id")
                    .cloned()
                    .context("T3 v2 run lacks run_id")?,
            );
            columns.insert(
                "state".into(),
                columns
                    .get("status")
                    .cloned()
                    .context("T3 v2 run lacks status")?,
            );
            if let Some(id) = payload.get("checkpointId") {
                columns.insert("checkpoint_ref".into(), id.clone());
            }
            Ok("projection_turns")
        }
        "orchestration_v2_projection_turn_items" | "orchestration_v2_projection_subagents" => {
            let key = if table.ends_with("subagents") {
                "subagent_id"
            } else {
                "turn_item_id"
            };
            columns.insert(
                "activity_id".into(),
                columns
                    .get(key)
                    .cloned()
                    .context("T3 v2 activity lacks id")?,
            );
            columns.insert(
                "kind".into(),
                payload
                    .get("type")
                    .cloned()
                    .unwrap_or_else(|| Value::String("subagent".into())),
            );
            Ok("projection_thread_activities")
        }
        _ => Ok(table),
    }
}

fn v2_link(payload: &Value) -> OrchestratorLinkFact {
    let fail = |status, reason: &str| OrchestratorLinkFact {
        status,
        provider_source: None,
        provider_native_id: None,
        reason: Some(reason.into()),
    };
    let native = &payload["nativeThreadRef"];
    let Some(driver) = native["driver"].as_str() else {
        return fail(
            OrchestratorLinkStatus::Malformed,
            "missing nativeThreadRef.driver",
        );
    };
    let source = match driver {
        "codex" => "codex",
        "claudeAgent" => "claude",
        _ => {
            return fail(
                OrchestratorLinkStatus::Unsupported,
                "unsupported nativeThreadRef.driver",
            );
        }
    };
    let Some(id) = native["nativeId"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
    else {
        return fail(
            OrchestratorLinkStatus::Malformed,
            "missing nativeThreadRef.nativeId",
        );
    };
    if native["strength"] != "strong" || !is_uuid(id) {
        return fail(
            OrchestratorLinkStatus::Malformed,
            "unproven nativeThreadRef identity",
        );
    }
    OrchestratorLinkFact {
        status: OrchestratorLinkStatus::Candidate,
        provider_source: Some(source.into()),
        provider_native_id: Some(id.into()),
        reason: None,
    }
}

fn identity_key(value: &Value) -> Option<String> {
    value["origin"]["canonicalKey"]
        .as_str()
        .and_then(crate::repository_key::repository_key)
        .or_else(|| {
            value["canonicalKey"]
                .as_str()
                .and_then(crate::repository_key::repository_key)
        })
}

fn collect_repository_urls(
    columns: &BTreeMap<String, Value>,
    urls: &mut std::collections::BTreeSet<String>,
) {
    for (name, value) in columns {
        let decoded = value
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| value.clone());
        let identity = match name.as_str() {
            "repository_identity_json" | "repository_identity" => &decoded,
            "payload_json" => &decoded["repositoryIdentity"],
            _ => continue,
        };
        if let Some(key) = identity_key(identity) {
            urls.insert(key);
        }
    }
}

pub(crate) fn repository_fallbacks(
    captures: &[Capture],
    resolver: &dyn crate::repository_resolution::Resolver,
) -> Result<BTreeMap<(String, String), String>> {
    let mut answers: BTreeMap<(String, String), std::collections::BTreeSet<String>> =
        BTreeMap::new();
    for capture in captures.iter().filter(|c| c.meta.source == SOURCE) {
        resolver.extracted();
        let projection = match extract_projection(capture) {
            Ok(projection) => projection,
            Err(error) => {
                tracing::warn!(%error, "invalid T3 repository evidence");
                continue;
            }
        };
        if let Some(url) = projection.repository_url {
            let links = projection
                .link_facts
                .iter()
                .filter(|f| f.status == OrchestratorLinkStatus::Candidate)
                .filter_map(|f| Some((f.provider_source.clone()?, f.provider_native_id.clone()?)))
                .collect::<std::collections::BTreeSet<_>>();
            if links.len() == 1 {
                answers
                    .entry(links.into_iter().next().unwrap())
                    .or_default()
                    .insert(url);
            }
        }
    }
    Ok(answers
        .into_iter()
        .filter_map(|(key, urls)| {
            (urls.len() == 1).then(|| (key, urls.into_iter().next().unwrap()))
        })
        .collect())
}
