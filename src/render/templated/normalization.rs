//! Normalize live View entries before writes. Plan every rename first so
//! ambiguous NFC/NFD entries fail before any disposable View mutation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::path_key::{filesystem_key, nfc};

type Rename = (PathBuf, PathBuf);

pub(super) fn plan_renames<'a>(
    root: &Path,
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Result<Vec<Rename>> {
    let live = paths
        .flat_map(|path| path.ancestors())
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| (filesystem_key(&path.to_string_lossy()), path.to_path_buf()))
        .collect::<BTreeMap<_, _>>();
    let mut renames = Vec::new();
    if root.exists() {
        inspect(root, root, &live, &mut BTreeMap::new(), &mut renames)?;
    }
    Ok(renames)
}

fn inspect(
    root: &Path,
    dir: &Path,
    live: &BTreeMap<String, PathBuf>,
    spellings: &mut BTreeMap<String, PathBuf>,
    renames: &mut Vec<Rename>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        // Render unlinks symlinks before applying renames. Do not follow them
        // during preflight, including case-only matches to dangling links.
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).expect("View is beneath root");
        let key = nfc(&relative.to_string_lossy());
        if let Some(planned) = live.get(&filesystem_key(&relative.to_string_lossy()))
            && (key == nfc(&planned.to_string_lossy())
                || super::super::sweep::same_file(&path, &root.join(planned))?)
        {
            if let Some(other) = spellings.insert(key, path.clone()) {
                anyhow::bail!(
                    "View entries {} and {} differ only by Unicode normalization; \
                     resolve the duplicate before rendering",
                    other.display(),
                    path.display()
                );
            }
            let name = entry.file_name();
            let canonical = nfc(&name.to_string_lossy());
            if name != canonical.as_str() {
                renames.push((path.clone(), path.with_file_name(canonical)));
            }
        }
        if kind.is_dir() {
            inspect(root, &path, live, spellings, renames)?;
        }
    }
    Ok(())
}

pub(super) fn apply_renames(mut renames: Vec<Rename>) -> Result<()> {
    // Children first: their source paths still contain the old ancestor names.
    renames.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (from, to) in renames {
        if std::fs::symlink_metadata(&to).is_ok() {
            anyhow::ensure!(
                super::super::sweep::same_file(&from, &to)?,
                "refusing to overwrite {} while normalizing {}",
                to.display(),
                from.display()
            );
        }
        // An intermediate sibling forces APFS to update the stored spelling
        // even when rename(NFD, NFC) would resolve both names to one entry.
        // Moving the inode preserves bytes/mtime and lets git detect a rename.
        let staging = tempfile::Builder::new()
            .prefix(".view-normalization-")
            .tempdir_in(from.parent().context("View entry has no parent")?)?;
        let staged = staging.path().join("entry");
        std::fs::rename(&from, &staged)
            .with_context(|| format!("staging View rename {}", from.display()))?;
        if let Err(error) = std::fs::rename(&staged, &to) {
            if let Err(rollback) = std::fs::rename(&staged, &from) {
                // Do not let TempDir cleanup delete the stranded View.
                let recovery = staging.keep();
                anyhow::bail!(
                    "renaming {} to {} failed: {error}; rollback failed: {rollback}; \
                     recover View from {}",
                    from.display(),
                    to.display(),
                    recovery.display()
                );
            }
            return Err(error)
                .with_context(|| format!("renaming {} to {}", from.display(), to.display()));
        }
        tracing::debug!(from = %from.display(), to = %to.display(), "View normalized to NFC");
    }
    Ok(())
}
