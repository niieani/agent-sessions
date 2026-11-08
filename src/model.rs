//! The narrow canonical types: Conversation, Message, SubagentLink, Provenance.
//!
//! Deliberately narrow and stable for years: the user's Messages, Agent
//! Messages, the assistant's responses, and Subagent links — in that order of
//! importance. Actions (tool calls, parameters, result payloads — tiers 5–7)
//! are not part of the canonical model; they stay recoverable from Captures.
//! The only trace Actions leave here is a count retained for opt-in Transcript
//! markers.

#[path = "model_branches.rs"]
mod branches;
pub use branches::{BranchPath, ConversationBranch, RenderAsset, branch_paths};
#[path = "model_projects.rs"]
mod projects;
pub use projects::NativeProject;
#[path = "model_references.rs"]
mod references;
pub use references::{
    MessagePresentation, ReferenceKind, ReferenceTarget, RenderMaterial, RenderReference,
};

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The role of a Message within a Conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Agent,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Agent => "agent",
            Role::Assistant => "assistant",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Role::User),
            "agent" => Ok(Role::Agent),
            "assistant" => Ok(Role::Assistant),
            other => Err(anyhow::anyhow!("unknown role: {other}")),
        }
    }
}

/// Semantic kind of a canonical Message. Goal variants keep Codex's
/// user-authored persistent objective while separating it from the runtime
/// envelope that carried it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageKind {
    #[default]
    Dialog,
    Goal,
    GoalUnresolved,
    GoalContinuation,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dialog => "dialog",
            Self::Goal => "goal",
            Self::GoalUnresolved => "goal-unresolved",
            Self::GoalContinuation => "goal-continuation",
        }
    }
}

impl std::str::FromStr for MessageKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "dialog" => Ok(Self::Dialog),
            "goal" => Ok(Self::Goal),
            "goal-unresolved" => Ok(Self::GoalUnresolved),
            "goal-continuation" => Ok(Self::GoalContinuation),
            other => Err(anyhow::anyhow!("unknown message kind: {other}")),
        }
    }
}

/// One turn element within a Conversation: a role, text content, a timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub kind: MessageKind,
    pub text: String,
    /// Epoch milliseconds, UTC. `None` only when the native record carried no
    /// timestamp (rare; control records never become Messages).
    pub timestamp_ms: Option<i64>,
    /// How many Actions (tool calls) were omitted between the previous
    /// Message and this one — available to an opt-in View marker, never
    /// Dialog content.
    pub actions_before: u32,
}

/// A bidirectional parent/child link to a Subagent Session.
///
/// Populated from slice 5 onward; part of the model from day one so the
/// Conversation shape is stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentLink {
    pub parent_native_id: String,
    /// The child Session's native id (encodes the parent chain for Claude).
    pub child_native_id: String,
    /// Human description of the delegated task, when the Source records one.
    pub description: Option<String>,
    /// The subagent's type/role (e.g. `Explore`), when recorded.
    pub agent_type: Option<String>,
    /// When the parent spawned the child, epoch milliseconds UTC.
    pub spawned_at_ms: Option<i64>,
    /// The delegated instructions, when recorded by the parent.
    pub instructions: Option<String>,
    pub depth: Option<u32>,
}

/// Child-side native fact: this Conversation was spawned by `parent_native_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentParent {
    pub parent_native_id: String,
    pub description: Option<String>,
    pub agent_type: Option<String>,
    pub instructions: Option<String>,
    pub depth: Option<u32>,
    /// Source-native child key (Claude `agentId`, Codex `agent_path`).
    pub native_key: Option<String>,
    /// Source-native join key (Claude `toolUseId`).
    pub link_key: Option<String>,
}

/// Materialized parent context positively identified in a child Capture and
/// removed before projecting the child's Dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InheritedContext {
    /// Direct parent Capture used as the membership source.
    pub source_native_id: String,
    /// Copied `response_item`s discarded by native id membership.
    pub copied_item_count: u32,
}

/// Parent-side native fact referring to a spawned child. Resolution combines
/// this with a child's [`SubagentParent`] fact; native keys are never treated
/// as Session identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentReference {
    pub child_native_id: Option<String>,
    pub child_native_key: Option<String>,
    pub link_key: Option<String>,
    pub description: Option<String>,
    pub agent_type: Option<String>,
    pub instructions: Option<String>,
    pub spawned_at_ms: Option<i64>,
}

/// The native identity a Session carries forever: its Source, Host, native
/// id, and native location. What makes a record traceable back to where it
/// came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Source name (`claude`, `codex`, `pi`, `cursor`, `vscode`).
    pub source: String,
    /// The Host the Session was recorded on.
    pub host: String,
    /// Native session id, unique within the Source (ADR-0004 identity).
    pub native_id: String,
    /// Absolute path of the native payload inside the Store.
    pub native_path: String,
}

/// Stable, transcript-worthy facts observed while extracting a Session.
/// Source-specific diagnostics stay in the Capture for `agent-sessions info`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptFacts {
    pub recovered_via: Option<RecoveredVia>,
    pub starting_model: Option<String>,
    pub starting_reasoning_effort: Option<String>,
    pub git: Option<GitSnapshot>,
    pub patched_files: Vec<String>,
    pub referenced_files: Vec<String>,
}

/// Exact Orchestrator Thread whose projected Dialog recovers a missing Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredVia {
    pub source: String,
    pub thread_id: String,
    pub host: String,
}

/// A compact Git snapshot suitable for transcript frontmatter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub repository_url: Option<String>,
}

impl GitSnapshot {
    pub fn is_empty(&self) -> bool {
        self.branch.is_none() && self.commit.is_none() && self.repository_url.is_none()
    }
}

impl TranscriptFacts {
    /// Normalize and deterministically de-duplicate file paths without
    /// touching the filesystem. Paths inside the project become relative;
    /// paths outside it remain absolute.
    pub fn normalize_file_paths(&mut self, project_path: Option<&str>) {
        normalize_file_paths(&mut self.patched_files, project_path);
        normalize_file_paths(&mut self.referenced_files, project_path);
    }
}

fn normalize_file_paths(paths: &mut Vec<String>, project_path: Option<&str>) {
    let project = project_path.map(Path::new);
    let mut normalized = BTreeSet::new();
    for path in std::mem::take(paths) {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let rendered = project
            .and_then(|root| Path::new(path).strip_prefix(root).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        normalized.insert(rendered);
    }
    *paths = normalized.into_iter().collect();
}

/// The canonical extracted exchange for a Session (tiers 1–3 only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub provenance: Provenance,
    /// Source-native title. Message text is never promoted to title because
    /// it is already present in the Conversation body.
    pub title: Option<String>,
    /// The project working directory the Session ran in, when known.
    pub project_path: Option<String>,
    /// Verified Source-native Project membership; never a filesystem path.
    #[serde(default)]
    pub native_project: Option<NativeProject>,
    /// First record timestamp, epoch milliseconds UTC.
    pub created_at_ms: Option<i64>,
    /// Last record timestamp, epoch milliseconds UTC.
    pub updated_at_ms: Option<i64>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub branches: Vec<ConversationBranch>,
    /// Compact facts selected for rendered transcript frontmatter.
    #[serde(default)]
    pub transcript_facts: TranscriptFacts,
    pub subagent_parent: Option<SubagentParent>,
    /// Present only when copied parent context was proven and excluded.
    pub inherited_context: Option<InheritedContext>,
    pub subagent_references: Vec<SubagentReference>,
    pub subagent_links: Vec<SubagentLink>,
    /// Actions omitted after the last Message (trailing seam).
    pub trailing_actions: u32,
}

impl Conversation {
    /// Dialog: contains at least one non-empty user, Agent, or assistant Message.
    pub fn has_dialog(&self) -> bool {
        self.messages
            .iter()
            .chain(self.branches.iter().flat_map(|branch| &branch.messages))
            .any(|m| !m.text.trim().is_empty())
    }
}

/// One exact provider identity fact extracted from an Orchestrator Capture.
/// Unsupported and malformed facts remain explicit audit data; only a
/// Candidate may resolve to a provider Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrchestratorLinkStatus {
    Candidate,
    Unsupported,
    Malformed,
}

impl OrchestratorLinkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Unsupported => "unsupported",
            Self::Malformed => "malformed",
        }
    }
}

impl std::str::FromStr for OrchestratorLinkStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "unsupported" => Ok(Self::Unsupported),
            "malformed" => Ok(Self::Malformed),
            other => Err(anyhow::anyhow!("unknown Orchestrator Link status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorLinkFact {
    pub status: OrchestratorLinkStatus,
    pub provider_source: Option<String>,
    pub provider_native_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorMessage {
    pub role: String,
    pub text: String,
    pub timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorTurn {
    pub native_id: String,
    pub state: Option<String>,
    pub checkpoint_id: Option<String>,
    /// Exact native JSON retained for audit/recovery.
    pub payload_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorActivity {
    pub native_id: String,
    pub kind: Option<String>,
    /// Exact native JSON retained for audit/recovery.
    pub payload_json: Option<String>,
}

/// Complete per-Host Orchestrator Thread projection extracted from one
/// Capture. Election always chooses this as a unit; loser fields never mix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorProjection {
    #[serde(default)]
    pub repository_url: Option<String>,
    pub source: String,
    pub native_id: String,
    pub host: String,
    pub native_path: String,
    pub logical_revision_ms: i64,
    pub discriminator: u64,
    pub adapter_version: u32,
    pub title: Option<String>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub workspace_root: Option<String>,
    pub provider_name: Option<String>,
    pub provider_instance_id: Option<String>,
    pub model: Option<String>,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub archived: bool,
    pub pinned: bool,
    pub settled: bool,
    /// Exact native JSON; never interpreted by the Archive projection.
    pub linked_pull_request: Option<String>,
    pub checkpoint_count: u32,
    pub link_facts: Vec<OrchestratorLinkFact>,
    pub messages: Vec<OrchestratorMessage>,
    pub turns: Vec<OrchestratorTurn>,
    pub activities: Vec<OrchestratorActivity>,
}

/// A resolved additive Orchestrator view over one provider Session. Provider
/// and Orchestrator fields remain separately named so callers cannot replace
/// provider-owned identity, title, path, or Conversation data by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Augmentation {
    pub source: String,
    pub thread_id: String,
    pub host: String,
    pub title: Option<String>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub workspace_root: Option<String>,
    pub provider_name: Option<String>,
    pub provider_instance_id: Option<String>,
    pub model: Option<String>,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub archived: bool,
    pub pinned: bool,
    pub settled: bool,
    pub linked_pull_request: Option<String>,
    pub turn_count: u32,
    pub checkpoint_count: u32,
}
