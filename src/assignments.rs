//! Durable, explicit Session-to-Project relationships. Files are authoritative.
use crate::archive::Archive;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub type ProjectAssignments = BTreeMap<(String, String), ProjectAssignment>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectAssignment {
    pub schema_version: u32,
    pub source: String,
    pub native_id: String,
    pub project: String,
    pub assigned_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn assignment_path(root: &Path, source: &str, native_id: &str) -> Result<PathBuf> {
    validate_identity(source, native_id)?;
    Ok(root.join("assignments").join(source).join(format!(
        "{}.json",
        crate::path_key::identity_component(native_id)
    )))
}

fn validate_identity(source: &str, native_id: &str) -> Result<()> {
    anyhow::ensure!(
        crate::adapters::adapter_for(source).is_some(),
        "unknown assignment Source {source:?}"
    );
    anyhow::ensure!(
        !native_id.trim().is_empty() && !native_id.chars().any(char::is_control),
        "invalid assignment native identity"
    );
    Ok(())
}
fn validate_project(project: &str) -> Result<()> {
    anyhow::ensure!(
        !project.trim().is_empty()
            && project == project.trim()
            && !project.chars().any(char::is_control)
            && !project.contains(['[', ']']),
        "project must be a nonempty plain project reference without surrounding whitespace or wikilink syntax"
    );
    Ok(())
}

/// Reject links at each archive-owned component; never traverse to external files.
fn directory(path: &Path, create: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_dir(),
                "assignment directory is not a real directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if create {
                fs::create_dir(path)?;
                fs::File::open(
                    path.parent()
                        .context("assignment directory has no parent")?,
                )?
                .sync_all()?;
            }
            Ok(create)
        }
        Err(error) => Err(error.into()),
    }
}
fn regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "assignment is not a regular file: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn load(root: &Path) -> Result<ProjectAssignments> {
    let base = root.join("assignments");
    let mut assignments = BTreeMap::new();
    if !directory(&base, false)? {
        return Ok(assignments);
    }
    for source in fs::read_dir(&base)? {
        let source = source?;
        directory(&source.path(), false)?;
        let source_name = source
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("assignment Source must be UTF-8"))?;
        anyhow::ensure!(
            crate::adapters::adapter_for(&source_name).is_some(),
            "unknown assignment Source {source_name:?}"
        );
        for entry in fs::read_dir(source.path())? {
            let path = entry?.path();
            regular_file(&path)?;
            // Unpublished complete/partial files from interrupted atomic publication
            // have no authority and are never indexed.
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".assignment-"))
            {
                continue;
            }
            let record: ProjectAssignment = serde_json::from_reader(fs::File::open(&path)?)
                .with_context(|| format!("reading Project Assignment {}", path.display()))?;
            anyhow::ensure!(
                record.schema_version == 1,
                "unsupported Project Assignment schema in {}",
                path.display()
            );
            validate_identity(&record.source, &record.native_id)?;
            validate_project(&record.project)?;
            anyhow::ensure!(
                record.assigned_at_ms >= 0,
                "invalid assignment timestamp in {}",
                path.display()
            );
            anyhow::ensure!(
                path == assignment_path(root, &record.source, &record.native_id)?,
                "Project Assignment identity does not match path {}",
                path.display()
            );
            let key = (record.source.clone(), record.native_id.clone());
            anyhow::ensure!(
                assignments.insert(key, record).is_none(),
                "duplicate Project Assignment identity"
            );
        }
    }
    Ok(assignments)
}

pub fn assign(
    archive: &Archive,
    source: &str,
    native_id: &str,
    project: &str,
    reason: Option<&str>,
) -> Result<()> {
    let path = assignment_path(archive.root(), source, native_id)?;
    validate_project(project)?;
    let _lock = archive.acquire_writer_lock()?;
    let mut db = archive.db()?;
    anyhow::ensure!(
        db.has_session_identity(source, native_id)?,
        "unknown Session {source}:{native_id}"
    );
    // Validate all current durable input before replacing any part of it.
    load(archive.root())?;
    directory(&archive.root().join("assignments"), true)?;
    directory(path.parent().expect("assignment parent"), true)?;
    regular_file(&path)?;
    let record = ProjectAssignment {
        schema_version: 1,
        source: source.into(),
        native_id: native_id.into(),
        project: project.into(),
        assigned_at_ms: crate::adapters::now_ms(),
        reason: reason.map(str::to_owned),
    };
    let mut pending = tempfile::Builder::new()
        .prefix(".assignment-")
        .tempfile_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut pending, &record)?;
    pending.write_all(b"\n")?;
    pending.as_file().sync_all()?;
    pending
        .persist(&path)
        .with_context(|| format!("publishing Project Assignment {}", path.display()))?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    refresh_index(archive, &mut db).context(
        "Project Assignment is durable; index refresh failed; rerun command or rebuild to recover",
    )
}

pub fn unassign(archive: &Archive, source: &str, native_id: &str) -> Result<()> {
    let path = assignment_path(archive.root(), source, native_id)?;
    let _lock = archive.acquire_writer_lock()?;
    let mut db = archive.db()?;
    anyhow::ensure!(
        db.has_session_identity(source, native_id)?,
        "unknown Session {source}:{native_id}"
    );
    load(archive.root())?;
    if regular_file(&path)? {
        fs::remove_file(&path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()?;
    }
    refresh_index(archive, &mut db).context("Project Assignment removal is durable; index refresh failed; rerun command or rebuild to recover")
}

pub fn refresh_index(archive: &Archive, db: &mut crate::archive::Db) -> Result<()> {
    db.replace_project_assignments(&load(archive.root())?)
}
