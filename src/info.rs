//! Capture-backed, source-aware Session inspection for the `info` command.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::adapters::{AdapterExtraction, adapter_for};
use crate::archive::{Archive, extract_capture};
use crate::model::TranscriptFacts;

/// One selected Session plus optional Source-native diagnostics.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub source: String,
    pub native_id: String,
    pub capture_host: String,
    pub available_hosts: Vec<String>,
    pub host_attribution: Option<crate::origins::HostAttribution>,
    pub title: Option<String>,
    pub project_path: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub transcript_facts: Option<TranscriptFacts>,
    /// Adapter-owned data; field names preserve Source semantics.
    pub source_details: Value,
}

/// Look up and inspect a Capture. The Capture layer, rather than the
/// regenerable database, decides what is present and which Host copy wins.
pub fn inspect_session(
    archive: &Archive,
    native_id: &str,
    source: Option<&str>,
    host: Option<&str>,
) -> Result<SessionInfo> {
    let lookup = archive.lookup_capture(native_id, source, host)?;
    let capture = lookup.capture;
    let host_attribution = crate::origins::lookup(
        archive.root(),
        &capture.meta.source,
        &capture.meta.native_id,
    )?;
    let Some(adapter) = adapter_for(&capture.meta.source) else {
        return Ok(SessionInfo {
            source: capture.meta.source,
            native_id: capture.meta.native_id,
            capture_host: capture.meta.host,
            available_hosts: lookup.available_hosts,
            host_attribution,
            title: None,
            project_path: None,
            created_at_ms: None,
            updated_at_ms: None,
            transcript_facts: None,
            source_details: serde_json::json!({ "adapter": "unavailable" }),
        });
    };
    let context = adapter.prepare_extraction(archive.root())?;
    let conversation = match extract_capture(adapter, &capture, &archive.captures_dir(), &context)?
    {
        Some(AdapterExtraction::Conversations(conversations)) => conversations
            .into_iter()
            .find(|c| c.provenance.native_id == native_id)
            .map(Box::new),
        Some(AdapterExtraction::Orchestrator(_)) | None => None,
    };
    let source_details = adapter.inspect(&capture)?;
    Ok(SessionInfo {
        source: capture.meta.source,
        native_id: capture.meta.native_id,
        capture_host: capture.meta.host,
        available_hosts: lookup.available_hosts,
        host_attribution,
        title: conversation
            .as_ref()
            .and_then(|conversation| conversation.title.clone()),
        project_path: conversation
            .as_ref()
            .and_then(|conversation| conversation.project_path.clone()),
        created_at_ms: conversation
            .as_ref()
            .and_then(|conversation| conversation.created_at_ms),
        updated_at_ms: conversation
            .as_ref()
            .and_then(|conversation| conversation.updated_at_ms),
        transcript_facts: conversation.map(|conversation| conversation.transcript_facts),
        source_details,
    })
}

/// Stable, compact default text output. Detailed provider data remains
/// structurally available through `--json` and is shown as JSON here so its
/// native naming is never flattened into a false common abstraction.
pub fn format_human(info: &SessionInfo) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "{} {}", info.source, info.native_id);
    let _ = writeln!(out, "captureHost: {}", info.capture_host);
    if let Some(origin) = &info.host_attribution {
        let _ = writeln!(out, "originHost: {} ({:?})", origin.host, origin.status);
    }
    if info.available_hosts.len() > 1 {
        let _ = writeln!(out, "availableHosts: {}", info.available_hosts.join(", "));
    }
    if let Some(title) = &info.title {
        let _ = writeln!(out, "title: {title}");
    }
    if let Some(project_path) = &info.project_path {
        let _ = writeln!(out, "projectPath: {project_path}");
    }
    if let Some(facts) = &info.transcript_facts {
        if let Some(model) = &facts.starting_model {
            let _ = writeln!(out, "startingModel: {model}");
        }
        if let Some(effort) = &facts.starting_reasoning_effort {
            let _ = writeln!(out, "startingReasoningEffort: {effort}");
        }
        if let Some(git) = &facts.git {
            if let Some(branch) = &git.branch {
                let _ = writeln!(out, "git.branch: {branch}");
            }
            if let Some(commit) = &git.commit {
                let _ = writeln!(out, "git.commit: {commit}");
            }
            if let Some(repository_url) = &git.repository_url {
                let _ = writeln!(out, "git.repositoryUrl: {repository_url}");
            }
        }
        if !facts.patched_files.is_empty() {
            let _ = writeln!(out, "patchedFiles: {}", facts.patched_files.join(", "));
        }
        if !facts.referenced_files.is_empty() {
            let _ = writeln!(
                out,
                "referencedFiles: {}",
                facts.referenced_files.join(", ")
            );
        }
    }
    if !info
        .source_details
        .as_object()
        .is_some_and(|object| object.is_empty())
    {
        out.push_str("sourceDetails:\n");
        let rendered = serde_json::to_string_pretty(&info.source_details)
            .expect("serializing serde_json::Value cannot fail");
        for line in rendered.lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}
