use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptSource {
    #[default]
    Builtin,
    Config,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoveredRange {
    pub first_idx: usize,
    pub last_idx: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DayDigest {
    pub schema_version: u32,
    pub source: String,
    pub native_id: String,
    pub workday: String,
    pub covered: CoveredRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialog_fingerprint: Option<String>,
    pub topic: Option<String>,
    pub title: String,
    pub locked_title: String,
    /// The Session Intent in effect on this Session Day; absent from
    /// Digests written before Intent existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub tldr: Option<String>,
    pub summary: String,
    pub classification: Vec<String>,
    pub truncated: bool,
    pub model: String,
    pub prompt_version: u32,
    #[serde(default)]
    pub prompt_source: PromptSource,
    pub digested_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionDigest {
    pub schema_version: u32,
    pub topic: String,
    pub title: String,
    pub updated_at_ms: i64,
    pub model: String,
    pub prompt_version: u32,
    #[serde(default)]
    pub prompt_source: PromptSource,
    #[serde(default)]
    pub pushed: BTreeMap<String, String>,
}

pub fn digest_dir(root: &Path, source: &str, native_id: &str) -> Result<PathBuf> {
    anyhow::ensure!(safe_component(source), "unsafe Digest Source {source:?}");
    Ok(root
        .join("digests")
        .join(source)
        .join(crate::path_key::identity_component(native_id)))
}

pub fn path_digest_dir(session_dir: &Path, path_id: Option<&str>) -> PathBuf {
    path_id.map_or_else(
        || session_dir.to_path_buf(),
        |id| {
            session_dir
                .join("branches")
                .join(crate::path_key::identity_component(id))
        },
    )
}

pub fn read_day(path: &Path) -> Result<Option<DayDigest>> {
    let digest: Option<DayDigest> = read_optional(path)?;
    if let Some(digest) = &digest {
        anyhow::ensure!(digest.schema_version == 1, "unsupported Digest schema");
        anyhow::ensure!(
            digest.covered.first_idx <= digest.covered.last_idx,
            "invalid Digest covered range in {}",
            path.display()
        );
        if let Some(fingerprint) = &digest.dialog_fingerprint {
            anyhow::ensure!(
                fingerprint.len() == 64
                    && fingerprint
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid Digest Dialog fingerprint in {}",
                path.display()
            );
        }
        let filename_day = path.file_stem().and_then(|value| value.to_str());
        anyhow::ensure!(
            filename_day == Some(digest.workday.as_str()),
            "Digest Workday does not match {}",
            path.display()
        );
        anyhow::ensure!(
            !digest.source.is_empty()
                && !digest.native_id.is_empty()
                && !digest.title.trim().is_empty()
                && !digest.locked_title.trim().is_empty()
                && !digest.summary.trim().is_empty(),
            "incomplete Digest {}",
            path.display()
        );
    }
    Ok(digest)
}

pub fn read_session(path: &Path) -> Result<Option<SessionDigest>> {
    let digest: Option<SessionDigest> = read_optional(path)?;
    if let Some(digest) = &digest {
        anyhow::ensure!(digest.schema_version == 1, "unsupported Digest schema");
        anyhow::ensure!(
            !digest.topic.trim().is_empty() && !digest.title.trim().is_empty(),
            "incomplete Session Digest {}",
            path.display()
        );
    }
    Ok(digest)
}

pub fn ensure_day_identity(
    digest: &DayDigest,
    source: &str,
    native_id: &str,
    workday: &str,
    path: &Path,
) -> Result<()> {
    anyhow::ensure!(
        digest.source == source && digest.native_id == native_id && digest.workday == workday,
        "Digest identity does not match {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn ensure_ready(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        anyhow::ensure!(
            !parent.join("repair.pending").try_exists()?,
            "pending Digest repair in {}; rerun repair-digests to finish publication",
            parent.display()
        );
    }
    Ok(())
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    ensure_ready(path)?;
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing Digest {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading Digest {}", path.display())),
    }
}

pub fn write_json(archive_root: &Path, path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Digest path has no parent")?;
    reject_symlink_ancestors(archive_root, parent)?;
    std::fs::create_dir_all(parent)?;
    reject_symlink_ancestors(archive_root, parent)?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating Digest temporary file in {}", parent.display()))?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("committing Digest {}", path.display()))?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedDigest<'a> {
    pub attempted_at_ms: i64,
    pub kind: &'a str,
    pub attempt: usize,
    pub error: &'a str,
    pub raw: &'a str,
}

pub fn append_rejected(
    archive_root: &Path,
    path: &Path,
    rejected: &RejectedDigest<'_>,
) -> Result<()> {
    let parent = path
        .parent()
        .context("Rejected Digest path has no parent")?;
    reject_symlink_ancestors(archive_root, parent)?;
    std::fs::create_dir_all(parent)?;
    reject_symlink_ancestors(archive_root, parent)?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Rejected Digest path is not a real file: {}",
            path.display()
        );
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening Rejected Digest {}", path.display()))?;
    serde_json::to_writer(&mut file, rejected)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn reject_symlink_ancestors(archive_root: &Path, parent: &Path) -> Result<()> {
    let digest_root = archive_root.join("digests");
    let relative = parent
        .strip_prefix(&digest_root)
        .with_context(|| format!("Digest path escapes {}", digest_root.display()))?;
    match std::fs::symlink_metadata(&digest_root) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Digest root is not a real directory: {}",
            digest_root.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("checking {}", digest_root.display()));
        }
    }
    let mut current = digest_root;
    for component in relative.components() {
        anyhow::ensure!(
            matches!(component, Component::Normal(_)),
            "unsafe Digest path {}",
            parent.display()
        );
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Digest directory is not a real directory: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", current.display()));
            }
        }
    }
    Ok(())
}

fn safe_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    !value.contains('\0')
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
}

pub use super::cache::cache_rows;
