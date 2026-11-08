use super::Evidence;
use crate::model::{Conversation, Message, MessageKind, Provenance, Role, TranscriptFacts};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<String>,
    pub message: Option<Value>,
}
#[derive(Default, Debug, Clone)]
pub struct Graph {
    pub id: String,
    pub title: Option<String>,
    pub current: Option<String>,
    pub updated: Option<f64>,
    pub(super) native_revision: Option<super::revision::Revision>,
    pub created: Option<f64>,
    pub project: Option<String>,
    pub nodes: BTreeMap<String, Node>,
    pub issues: BTreeSet<String>,
    pub versions: BTreeSet<String>,
    pub fetched_versions: BTreeSet<String>,
    pub export_covered_versions: BTreeSet<String>,
    pub exported_media: BTreeMap<String, Value>,
    pub has_native_history: bool,
    node_observations: BTreeMap<String, (i64, u8)>,
}
fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}
impl Graph {
    pub fn from_evidence(id: &str, records: &[Evidence]) -> Result<Self> {
        let mut graph = Self {
            id: id.into(),
            ..Self::default()
        };
        for evidence in records {
            ensure!(
                matches!(
                    evidence.format.as_str(),
                    "export"
                        | "browser"
                        | "export-media-index"
                        | "catalog"
                        | "snapshot-metadata"
                        | "media-index"
                ),
                "unsupported ChatGPT evidence format {:?}",
                evidence.format
            );
        }
        let ordered = super::snapshots::ordered(records)?;
        // Snapshot metadata and selected pages precede versions, so version roots
        // can resolve the selected sibling's parent independently of capture order.
        for (rank, evidence) in ordered.iter().enumerate() {
            if evidence.format == "snapshot-metadata" {
                graph.metadata(&serde_json::from_str(&evidence.payload)?)?;
            } else if evidence.format == "export" {
                graph.add_export(&serde_json::from_str(&evidence.payload)?, rank as i64)?;
            } else if evidence.format == "browser" && !evidence.origin.contains("/versions?") {
                graph.add_browser(
                    &serde_json::from_str(&evidence.payload)?,
                    &evidence.origin,
                    rank as i64,
                )?;
            }
        }
        for evidence in &ordered {
            if evidence.format == "media-index" {
                let meta: Value = serde_json::from_str(&evidence.payload)?;
                let asset = meta["assetId"].as_str().context("assetId missing")?;
                graph.exported_media.insert(asset.into(), meta.clone());
            }
            if evidence.format == "export-media-index" {
                let row: Value = serde_json::from_str(&evidence.payload)?;
                let asset = row["file_id"]
                    .as_str()
                    .context("export media file_id absent")?;
                ensure!(
                    row["origination_thread_id"].as_str() == Some(id),
                    "export media conversation identity mismatch"
                );
                graph.exported_media.insert(asset.into(),serde_json::json!({"assetId":asset,"conversationId":id,"messageId":row["origination_message_id"],"mime":row["mime_type"],"expectedBytes":row["file_size_bytes"],"state":row["state"]}));
            }
        }
        for (rank, evidence) in ordered.iter().enumerate() {
            if evidence.format == "browser" && evidence.origin.contains("/versions?") {
                let payload: Value = serde_json::from_str(&evidence.payload)?;
                let target = query(&evidence.origin, "message_id")
                    .context("versions URL lacks message_id")?;
                graph.fetched_versions.insert(target.clone());
                let parent = graph
                    .nodes
                    .get(&target)
                    .context("version target absent; collect selected history first")?
                    .parent
                    .clone();
                for branch in payload
                    .as_array()
                    .context("versions payload must be array")?
                {
                    graph.add_sequence(
                        branch.as_array().context("version branch must be array")?,
                        parent.clone(),
                        (rank as i64, 1),
                    )?;
                }
            }
        }
        super::versions::cover_from_exports(&mut graph, &ordered)?;
        Ok(graph)
    }
    pub(super) fn add_export(&mut self, payload: &Value, captured_at: i64) -> Result<()> {
        self.has_native_history = true;
        self.metadata(payload)?;
        for (id, value) in payload
            .get("mapping")
            .and_then(Value::as_object)
            .context("export mapping missing")?
        {
            self.observe_node(
                id.clone(),
                Node {
                    parent: str_at(value, "parent"),
                    message: value.get("message").filter(|x| !x.is_null()).cloned(),
                },
                (captured_at, 0),
            );
        }
        self.current = str_at(payload, "current_node");
        Ok(())
    }
    fn metadata(&mut self, p: &Value) -> Result<()> {
        if p.get("title").is_some() {
            self.title = str_at(p, "title");
        }
        if let Some(value) = p.get("update_time") {
            let revision = super::revision::Revision::new(value)?;
            if self
                .native_revision
                .as_ref()
                .is_none_or(|old| &revision > old)
            {
                self.updated = revision.number.as_f64();
                self.native_revision = Some(revision);
            }
        }
        if let Some(t) = p.get("create_time").and_then(Value::as_f64) {
            self.created = Some(t);
        }
        if p.get("gizmo_id").is_some() {
            self.project = str_at(p, "gizmo_id");
        }
        Ok(())
    }
    fn add_browser(&mut self, p: &Value, url: &str, captured_at: i64) -> Result<()> {
        self.metadata(p)?;
        let Some(messages) = p.get("messages").and_then(Value::as_array) else {
            return Ok(());
        };
        self.has_native_history = true;
        let first = messages.first().and_then(|m| str_at(m, "id"));
        let parent = first
            .as_ref()
            .and_then(|id| self.nodes.get(id))
            .and_then(|n| n.parent.clone());
        self.add_sequence(messages, parent, (captured_at, 2))?;
        if let Some(before) = query(url, "before") {
            if let Some(last) = messages.last().and_then(|m| str_at(m, "id")) {
                // A pagination seam proves an otherwise unknown parent only.
                // Export topology and already observed sequences take precedence.
                let node = self.nodes.entry(before).or_insert(Node {
                    parent: None,
                    message: None,
                });
                if node.parent.is_none() {
                    node.parent = Some(last);
                }
            }
        } else if !url.contains("/messages?") {
            self.current = messages.last().and_then(|m| str_at(m, "id"));
        }
        if p.pointer("/page_info/has_previous_page")
            .and_then(Value::as_bool)
            == Some(true)
            && let Some(first) = first
        {
            self.issues.insert(format!("history-before:{first}"));
        }
        Ok(())
    }
    fn observe_node(&mut self, id: String, node: Node, observation: (i64, u8)) {
        // Versions are assembled after selected pages to resolve their roots,
        // but message content must still follow native observation chronology.
        if self
            .node_observations
            .get(&id)
            .is_some_and(|old| *old > observation)
        {
            return;
        }
        self.node_observations.insert(id.clone(), observation);
        self.nodes.insert(id, node);
    }
    fn add_sequence(
        &mut self,
        messages: &[Value],
        mut previous: Option<String>,
        observation: (i64, u8),
    ) -> Result<()> {
        for message in messages {
            let id = str_at(message, "id").context("message id missing")?;
            if message
                .pointer("/metadata/has_versions")
                .and_then(Value::as_bool)
                == Some(true)
            {
                self.versions.insert(id.clone());
            }
            // Browser responses omit internal graph nodes. Their ordered visible
            // sequence is authoritative; metadata.parent_id can name absent nodes.
            if let Some(existing) = self.nodes.get(&id)
                && previous.is_none()
            {
                previous = existing.parent.clone();
            }
            self.observe_node(
                id.clone(),
                Node {
                    parent: previous,
                    message: Some(message.clone()),
                },
                observation,
            );
            previous = Some(id);
        }
        Ok(())
    }
    /// Only the current selected response can still grow. Historical streaming
    /// commentary can retain `in_progress` after a finished response follows it.
    pub fn in_progress_message_ids(&self) -> Vec<&str> {
        self.current
            .as_deref()
            .filter(|id| {
                self.nodes
                    .get(*id)
                    .and_then(|node| node.message.as_ref())
                    .is_some_and(|message| {
                        message.get("status").and_then(Value::as_str) == Some("in_progress")
                    })
            })
            .into_iter()
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.has_native_history,
            "conversation history has not been captured"
        );
        for identity in
            std::iter::once(self.id.as_str()).chain(self.nodes.keys().map(String::as_str))
        {
            ensure!(
                !identity.is_empty()
                    && identity
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
                "unsafe graph identity {identity:?}"
            );
        }
        for (id, node) in &self.nodes {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(id.as_str());
            while let Some(current) = cursor {
                ensure!(seen.insert(current), "cycle at {current}");
                cursor = self
                    .nodes
                    .get(current)
                    .with_context(|| format!("missing ancestor {current} for {id}"))?
                    .parent
                    .as_deref();
            }
            if let Some(parent) = &node.parent {
                ensure!(self.nodes.contains_key(parent), "missing ancestor {parent}");
            }
        }
        for issue in &self.issues {
            let id = issue.strip_prefix("history-before:").unwrap();
            ensure!(
                self.nodes.get(id).is_some_and(|n| n.parent.is_some()),
                "incomplete history before {id}"
            );
        }
        if let Some(current) = &self.current {
            ensure!(
                self.nodes.contains_key(current),
                "selected node {current} missing"
            );
        }
        Ok(())
    }
    pub fn generated_assets(&self) -> BTreeMap<String, Value> {
        let mut assets = self.exported_media.clone();
        for (message_id, node) in &self.nodes {
            let Some(message) = &node.message else {
                continue;
            };
            if message.pointer("/author/role").and_then(Value::as_str) == Some("user") {
                continue;
            }
            for part in message
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if part["content_type"]
                    .as_str()
                    .is_some_and(|kind| kind != "image_asset_pointer")
                    || (part
                        .pointer("/metadata/generation")
                        .is_none_or(Value::is_null)
                        && part.pointer("/metadata/dalle").is_none_or(Value::is_null))
                {
                    continue;
                }
                if let Some(pointer) = part["asset_pointer"].as_str() {
                    let asset = pointer.rsplit('/').next().unwrap_or(pointer);
                    assets.insert(asset.into(),serde_json::json!({"assetId":asset,"conversationId":self.id,"messageId":message_id,"mime":part["mime_type"],"expectedBytes":part["size_bytes"]}));
                }
            }
        }
        assets
    }
    pub fn selected_ids(&self) -> Result<Vec<String>> {
        self.validate()?;
        let mut out = Vec::new();
        let mut cursor = self.current.as_deref();
        while let Some(id) = cursor {
            out.push(id.into());
            cursor = self.nodes[id].parent.as_deref();
        }
        out.reverse();
        Ok(out)
    }
    pub fn conversation(&self, provenance: Provenance) -> Result<Conversation> {
        let mut messages = Vec::new();
        let generated = super::presentation::generated_messages(self)?;
        let mut starting_model = None;
        let mut saw_assistant = false;
        let mut actions = 0;
        for id in self.selected_ids()? {
            if let Some(native) = &self.nodes[&id].message {
                if let Some(mut message) = dialog(native, &self.id)? {
                    if message.role == Role::Assistant && !saw_assistant {
                        saw_assistant = true;
                        starting_model = native
                            .pointer("/metadata/model_slug")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                            .map(str::to_owned);
                    }
                    message.actions_before = actions;
                    actions = 0;
                    messages.push(message);
                } else {
                    actions += 1;
                }
                if let Some(media) = generated.get(&id) {
                    messages.push(media.clone());
                }
            }
        }
        messages.extend(super::presentation::orphaned_media_messages(self)?);
        Ok(Conversation {
            provenance,
            title: self.title.clone(),
            project_path: None,
            native_project: None,
            created_at_ms: self.created.map(|v| (v * 1000.) as i64),
            updated_at_ms: self.updated.map(|v| (v * 1000.) as i64),
            messages,
            transcript_facts: TranscriptFacts {
                starting_model,
                ..TranscriptFacts::default()
            },
            subagent_parent: None,
            inherited_context: None,
            subagent_references: vec![],
            branches: self.branches()?,
            subagent_links: vec![],
            trailing_actions: actions,
        })
    }
}
pub fn query(url: &str, key: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| v.to_owned())
    })
}
pub(super) fn dialog_role(message: &Value) -> Option<Role> {
    let role = match message.pointer("/author/role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return None,
    };
    if matches!(
        message
            .pointer("/content/content_type")
            .and_then(Value::as_str),
        Some("thoughts" | "reasoning_recap" | "model_editable_context")
    ) {
        return None;
    }
    if message
        .pointer("/metadata/is_visually_hidden_from_conversation")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    if role == Role::Assistant {
        if let Some(recipient) = message.get("recipient").and_then(Value::as_str)
            && recipient != "all"
        {
            return None;
        }
        if let Some(channel) = message.get("channel").and_then(Value::as_str)
            && !matches!(channel, "final" | "commentary")
        {
            return None;
        }
        if message.pointer("/metadata/reasoning_status").is_some()
            && message.get("channel").is_none_or(Value::is_null)
            && message.get("end_turn").and_then(Value::as_bool) != Some(true)
        {
            return None;
        }
    }
    Some(role)
}
pub fn dialog(message: &Value, _conversation_id: &str) -> Result<Option<Message>> {
    let Some(role) = dialog_role(message) else {
        return Ok(None);
    };
    let text = super::presentation::native_text(message)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(Message {
        role,
        kind: MessageKind::Dialog,
        text,
        timestamp_ms: message
            .get("create_time")
            .and_then(Value::as_f64)
            .map(|v| (v * 1000.) as i64),
        actions_before: 0,
    }))
}
