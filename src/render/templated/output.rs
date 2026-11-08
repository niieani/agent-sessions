use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::archive::Archive;
use crate::fleet::{FleetConfig, ViewConfig};
use crate::path_key::filesystem_key;
use crate::pull::StoreRoots;

pub(super) fn output_root(archive: &Archive, config: &ViewConfig) -> Result<PathBuf> {
    let path = match &config.output {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => {
            anyhow::ensure!(
                path.components()
                    .all(|component| matches!(component, Component::Normal(_))),
                "views.output must not contain parent or current-directory components"
            );
            archive.root().join(path)
        }
        None => archive.views_dir(),
    };
    anyhow::ensure!(
        path.components()
            .all(|component| !matches!(component, Component::ParentDir | Component::CurDir)),
        "views.output must not contain parent or current-directory components"
    );
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!(
            "refusing to render through symlinked Views root {}",
            path.display()
        );
    }
    canonical_destination(&path)
}

pub(super) fn preflight_output(
    archive: &Archive,
    config: &FleetConfig,
    output: &Path,
) -> Result<()> {
    let output = canonical_destination(output)?;
    let archive_root = canonical_destination(archive.root())?;
    anyhow::ensure!(
        output != archive_root && !archive_root.starts_with(&output),
        "View output {} contains the Archive root",
        output.display()
    );
    for durable in [
        archive.captures_dir(),
        archive.root().join("digests"),
        archive.root().join("media"),
        archive.root().join("host-attributions"),
        archive.root().join("host-pulls.json"),
        archive.root().join("leads"),
        archive.root().join("lead-feeds"),
        archive.db_path(),
        archive.root().join("archive.db-wal"),
        archive.root().join("archive.db-shm"),
        archive.root().join("config.toml"),
        archive.root().join(".agent-sessions.lock"),
    ] {
        let durable = canonical_destination(&durable)?;
        anyhow::ensure!(
            !paths_overlap(&output, &durable),
            "View output {} overlaps durable Archive data {}",
            output.display(),
            durable.display()
        );
    }
    for template in [
        config.views.template.as_deref(),
        config.views.index_template.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let template = canonical_destination(template)?;
        anyhow::ensure!(
            !paths_overlap(&output, &template),
            "View output {} overlaps user-owned Template {}",
            output.display(),
            template.display()
        );
    }
    let mut stores = vec![StoreRoots::default()];
    stores.extend(
        config
            .hosts
            .iter()
            .filter(|host| host.ssh.is_none())
            .map(|host| {
                let defaults = StoreRoots::default();
                StoreRoots {
                    claude: host.claude_root.clone().unwrap_or(defaults.claude),
                    codex: host.codex_root.clone().unwrap_or(defaults.codex),
                    pi: host.pi_root.clone().unwrap_or(defaults.pi),
                    cursor: host.cursor_root.clone().unwrap_or(defaults.cursor),
                    t3: host.t3_root.clone().unwrap_or(defaults.t3),
                    chatgpt: host.chatgpt_root.clone().unwrap_or(defaults.chatgpt),
                    t3_explicit: host.t3_root.is_some(),
                }
            }),
    );
    for store in stores {
        for root in [
            store.claude,
            store.codex,
            store.pi,
            store.cursor,
            store.t3.join("userdata"),
            store.chatgpt,
        ] {
            let root = canonical_destination(&root)?;
            anyhow::ensure!(
                !paths_overlap(&output, &root),
                "View output {} overlaps native Store {}",
                output.display(),
                root.display()
            );
        }
    }
    match std::fs::symlink_metadata(&output) {
        Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
            "refusing to render through symlinked Views root {}",
            output.display()
        ),
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!("View output {} is not a directory", output.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", output.display())),
    }
}

/// Planned paths must be unique and not each other's ancestors, both as
/// spelled and under the filesystem key: on a case- or normalization-
/// insensitive filesystem two spellings of one path are the same file, so a
/// plan relying on them being distinct would overwrite or sweep its own Views.
pub(super) fn validate_manifest<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Result<()> {
    let paths = paths.collect::<Vec<_>>();
    let mut spellings: BTreeMap<String, &Path> = BTreeMap::new();
    for path in &paths {
        for prefix in path
            .ancestors()
            .filter(|prefix| !prefix.as_os_str().is_empty())
        {
            let key = filesystem_key(&prefix.to_string_lossy());
            let spelling = *spellings.entry(key).or_insert(prefix);
            anyhow::ensure!(
                spelling == prefix,
                "View paths {} and {} differ only by case or Unicode normalization; \
                 they would be one file on a case-insensitive filesystem",
                spelling.display(),
                prefix.display()
            );
        }
    }
    let keys = paths
        .iter()
        .map(|path| PathBuf::from(filesystem_key(&path.to_string_lossy())))
        .collect::<Vec<_>>();
    for left in 0..keys.len() {
        for right in left + 1..keys.len() {
            for (ancestor, descendant) in [(left, right), (right, left)] {
                anyhow::ensure!(
                    !keys[descendant].starts_with(&keys[ancestor]),
                    "View path {} conflicts with descendant {}",
                    paths[ancestor].display(),
                    paths[descendant].display()
                );
            }
        }
    }
    Ok(())
}

pub(super) fn reconcile_topology<'a>(
    output: &Path,
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Result<usize> {
    let mut removed = 0;
    for relative in paths {
        let target = output.join(relative);
        let mut ancestor = output.to_path_buf();
        for component in relative
            .components()
            .take(relative.components().count() - 1)
        {
            ancestor.push(component.as_os_str());
            if ancestor.is_file() {
                std::fs::remove_file(&ancestor)?;
                removed += 1;
            }
        }
        if target.is_dir() {
            removed += count_non_directories(&target)?;
            std::fs::remove_dir_all(&target)?;
        }
    }
    Ok(removed)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn canonical_destination(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .context("destination has no existing ancestor")?;
        tail.push(name.to_os_string());
        existing = existing
            .parent()
            .context("destination has no existing ancestor")?;
    }
    let mut resolved = std::fs::canonicalize(existing)?;
    for component in tail.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn count_non_directories(path: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += count_non_directories(&entry.path())?;
        } else {
            count += 1;
        }
    }
    Ok(count)
}
