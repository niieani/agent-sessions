//! Name Tables: inline `[views.*-names]` entries merged with the Name Table
//! files listed in `[views] name-tables`.
//!
//! A Name Table file is a generated companion to `config.toml` (for example,
//! emitted from a notes system) holding any of `[source-names]`,
//! `[host-names]`, and `[project-names]`. Every key has exactly one origin:
//! a key defined twice — inline and in a file, in two files, or as two
//! spellings of one repository — is a configuration error, never an override.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::fleet::ViewConfig;
use crate::repository_key::repository_key;

/// How inline entries are named in conflict messages.
const INLINE_ORIGIN: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct NameTableFile {
    #[serde(default)]
    source_names: IndexMap<String, String>,
    #[serde(default)]
    host_names: IndexMap<String, String>,
    #[serde(default)]
    project_names: IndexMap<String, String>,
}

/// The Project Name Table key a raw key selects: an absolute recorded path
/// stays verbatim; a Source-qualified native identity stays distinct; other keys normalize to a repository key, so
/// `https://github.com/owner/repo` and `owner/repo` are the same key.
fn project_key(raw: &str) -> Result<String> {
    if let Some(id) = raw.strip_prefix("chatgpt:") {
        anyhow::ensure!(
            crate::chatgpt::projects::native_project_id(id),
            "project-names: invalid ChatGPT Project identity {raw:?}"
        );
        return Ok(raw.into());
    }
    if let Some((source, id)) = raw.split_once(':')
        && !source.is_empty()
        && source
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        && !id.is_empty()
        && !id.starts_with('/')
        && !id.chars().any(|c| c.is_whitespace() || c == '\\')
    {
        return Ok(raw.into());
    }
    if raw.starts_with('/') {
        return Ok(raw.to_owned());
    }
    repository_key(raw).with_context(|| {
        format!("project-names: {raw:?} is neither a Source-qualified native identity, an absolute recorded path, nor a repository")
    })
}

/// Normalizes inline `[views.project-names]` keys; rejects two keys naming
/// the same Project.
pub(crate) fn normalize_inline(views: &mut ViewConfig) -> Result<()> {
    let mut normalized = IndexMap::with_capacity(views.project_names.len());
    let mut spellings: HashMap<String, &str> = HashMap::new();
    for (raw, name) in &views.project_names {
        let key = project_key(raw).context("views")?;
        if let Some(other) = spellings.insert(key.clone(), raw) {
            bail!("views.project-names: {other:?} and {raw:?} both name {key:?}");
        }
        normalized.insert(key, name.clone());
    }
    views.project_names = normalized;
    Ok(())
}

/// Loads every `[views] name-tables` file (relative paths resolve against
/// the Archive root) and merges its entries into `views`.
pub(crate) fn merge_files(views: &mut ViewConfig, archive_root: &Path) -> Result<()> {
    let mut origins: HashMap<(&'static str, String), String> = HashMap::new();
    for (table, entries) in [
        ("source-names", &views.source_names),
        ("host-names", &views.host_names),
        ("project-names", &views.project_names),
    ] {
        for key in entries.keys() {
            origins.insert((table, key.clone()), INLINE_ORIGIN.to_owned());
        }
    }
    for path in views.name_tables.clone() {
        let path = resolve(archive_root, &path);
        let origin = path.display().to_string();
        let file = read(&path)?;
        let project_names = file
            .project_names
            .into_iter()
            .map(|(raw, name)| Ok((project_key(&raw).with_context(|| origin.clone())?, name)))
            .collect::<Result<Vec<_>>>()?;
        for (table, entries, target) in [
            (
                "source-names",
                file.source_names.into_iter().collect::<Vec<_>>(),
                &mut views.source_names,
            ),
            (
                "host-names",
                file.host_names.into_iter().collect(),
                &mut views.host_names,
            ),
            ("project-names", project_names, &mut views.project_names),
        ] {
            for (key, name) in entries {
                if let Some(previous) = origins.insert((table, key.clone()), origin.clone()) {
                    bail!("{table}: {key:?} from {origin} is already defined in {previous}");
                }
                target.insert(key, name);
            }
        }
    }
    Ok(())
}

fn resolve(archive_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        archive_root.join(path)
    }
}

fn read(path: &Path) -> Result<NameTableFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading Name Table file {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing Name Table file {}", path.display()))
}
