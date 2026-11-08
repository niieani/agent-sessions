//! Durable Session Leads; files are authoritative, SQLite is reconstructible.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
mod sync;
mod wire;
pub use sync::sync;
pub type SessionLeads = BTreeMap<(String, String), SessionLead>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLead {
    pub cursor: String,
    pub source: String,
    pub session_id: String,
    pub led_by: LedBy,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub host: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub origin: Option<Origin>,
    pub recorded_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedBy {
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub kind: String,
    #[serde(flatten)]
    pub fields: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FeedConfig {
    pub name: String,
    pub url: String,
    pub token_file: Option<PathBuf>,
}
impl SessionLead {
    pub fn validate(&self) -> Result<()> {
        self.validate_fields()
            .with_context(|| format!("Session Lead cursor {:?}", self.cursor))
    }
    /// Producer-defined requester identity; only malformed identities are errors.
    pub fn requested_by(&self) -> Result<Option<(&str, &str)>> {
        let Some(value) = self
            .origin
            .as_ref()
            .and_then(|origin| origin.fields.get("requestedBy"))
        else {
            return Ok(None);
        };
        let (source, id) = value.split_once(':').with_context(|| format!(
            "Session Lead cursor {:?}: invalid origin.requestedBy (expected <source>:<native-session-id>)", self.cursor
        ))?;
        anyhow::ensure!(
            !source.trim().is_empty() && !id.trim().is_empty() && !id.chars().any(char::is_control),
            "Session Lead cursor {:?}: invalid origin.requestedBy (expected nonempty source and native session id)",
            self.cursor
        );
        Ok(Some((source, id)))
    }
    fn validate_fields(&self) -> Result<()> {
        anyhow::ensure!(
            ["codex", "claude"].contains(&self.source.as_str()),
            "unsupported source {:?}",
            self.source
        );
        anyhow::ensure!(
            !self.session_id.trim().is_empty() && !self.session_id.chars().any(char::is_control),
            "invalid lead sessionId"
        );
        anyhow::ensure!(!self.cursor.is_empty(), "empty lead cursor");
        anyhow::ensure!(
            self.led_by.kind == "agent",
            "unsupported ledBy.kind {:?}",
            self.led_by.kind
        );
        if let Some(name) = &self.led_by.name {
            anyhow::ensure!(!name.trim().is_empty(), "empty ledBy.name");
        }
        if let Some(host) = &self.host {
            anyhow::ensure!(!host.trim().is_empty(), "empty host");
        }
        if let Some(origin) = &self.origin {
            anyhow::ensure!(!origin.kind.trim().is_empty(), "empty origin.kind");
            for (field, value) in &origin.fields {
                anyhow::ensure!(!value.trim().is_empty(), "empty origin.{field}");
            }
        }
        self.requested_by()?;
        time::OffsetDateTime::parse(
            &self.recorded_at,
            &time::format_description::well_known::Rfc3339,
        )
        .context("invalid lead recordedAt (expected RFC 3339)")?;
        Ok(())
    }
}
pub fn lead_path(root: &Path, lead: &SessionLead) -> Result<PathBuf> {
    lead.validate()?;
    Ok(root.join("leads").join(&lead.source).join(format!(
        "{}.json",
        crate::path_key::identity_component(&lead.session_id)
    )))
}
pub(crate) fn directory(path: &Path, create: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            anyhow::ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "lead directory is not a real directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if create {
                fs::create_dir(path)?;
                fs::File::open(path.parent().context("directory has no parent")?)?.sync_all()?;
            }
            Ok(create)
        }
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            anyhow::ensure!(
                meta.file_type().is_file(),
                "lead input is not a regular file: {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn publish<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    regular_file(path)?;
    let parent = path.parent().context("lead file has no parent")?;
    let mut pending = tempfile::Builder::new()
        .prefix(".lead-")
        .tempfile_in(parent)?;
    serde_json::to_writer_pretty(&mut pending, record)?;
    pending.write_all(b"\n")?;
    pending.as_file().sync_all()?;
    pending
        .persist(path)
        .with_context(|| format!("publishing {}", path.display()))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn store(root: &Path, lead: &SessionLead) -> Result<()> {
    let path = lead_path(root, lead)?;
    directory(&root.join("leads"), true)?;
    directory(path.parent().unwrap(), true)?;
    publish(&path, lead)
}
pub fn load(root: &Path) -> Result<SessionLeads> {
    let base = root.join("leads");
    let mut records = SessionLeads::new();
    if !directory(&base, false)? {
        return Ok(records);
    }
    for path in lead_files(root)? {
        let lead: SessionLead = serde_json::from_reader(fs::File::open(&path)?)
            .with_context(|| format!("reading Session Lead {}", path.display()))?;
        anyhow::ensure!(
            path == lead_path(root, &lead)?,
            "lead identity does not match path {}",
            path.display()
        );
        anyhow::ensure!(
            records
                .insert((lead.source.clone(), lead.session_id.clone()), lead)
                .is_none(),
            "duplicate Session Lead"
        );
    }
    Ok(records)
}

// Missing optional fields are None; explicitly supplied null is malformed v1.
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Discover canonical inputs without decoding them; report clutter without following links.
/// Also used by indexed inspection so operators see unexpected archive entries.
pub fn lead_files(root: &Path) -> Result<Vec<PathBuf>> {
    let base = root.join("leads");
    if !directory(&base, false)? {
        return Ok(Vec::new());
    }
    let filename = regex::Regex::new(
        r"^(?:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}|id-(?:[0-9a-f]{2})+|id-sha256-[0-9a-f]{64})\.json$",
    )?;
    let mut files = Vec::new();
    for source in fs::read_dir(base)? {
        let source = source?;
        if source.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if !["codex", "claude"].iter().any(|s| source.file_name() == *s)
            || !source.file_type()?.is_dir()
        {
            tracing::warn!(path = %source.path().display(), "ignoring unexpected Session Lead entry");
            continue;
        }
        for entry in fs::read_dir(source.path())? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if !entry.file_type()?.is_file() || !filename.is_match(&name) {
                tracing::warn!(path = %entry.path().display(), "ignoring unexpected Session Lead entry");
                continue;
            }
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}
