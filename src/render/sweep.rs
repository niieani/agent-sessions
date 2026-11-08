//! Orphan sweep: remove Views no planned unit claims and reconcile kept
//! Views to their planned spelling. Unicode identity is independent of inodes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Remove Views that no planned unit claims. An existing file is kept when
/// it *is* a planned file: sync tools such as Syncthing store names decomposed
/// (NFD) on macOS while Render plans composed paths, and case-insensitive
/// filesystems resolve an older case spelling of a directory to the planned
/// one, so a byte comparison would delete a live View right after writing it.
/// Normalization-equivalent names always claim the same View. Case-only
/// candidates must resolve to the same file; separate case variants on a
/// sensitive filesystem are still swept.
/// A kept file or directory whose spelling differs from the plan by more than
/// Unicode normalization (a Project renamed only by case) is renamed to the
/// planned spelling; normalization-only differences stay as the sync tool
/// stored them (pre-write reconciliation canonicalizes live NFD entries).
/// Manifest validation guarantees planned paths are distinct
/// under this key.
pub(super) fn sweep_orphaned_views(
    root: &Path,
    dir: &Path,
    expected: &BTreeSet<PathBuf>,
) -> Result<usize> {
    let planned = PlannedSpellings {
        files: expected
            .iter()
            .map(|path| (filesystem_path_key(path), path.as_path()))
            .collect(),
        dirs: expected
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| (filesystem_path_key(dir), dir))
            .collect(),
    };
    sweep_orphans(root, dir, &planned)
}

struct PlannedSpellings<'a> {
    files: BTreeMap<String, &'a Path>,
    dirs: BTreeMap<String, &'a Path>,
}

fn filesystem_path_key(path: &Path) -> String {
    crate::path_key::filesystem_key(&path.to_string_lossy())
}

/// The planned spelling `path` resolves to, when the filesystem treats both
/// as one entry.
fn planned_entry<'a>(
    root: &Path,
    path: &Path,
    planned: &BTreeMap<String, &'a Path>,
) -> Result<Option<&'a Path>> {
    let relative = path.strip_prefix(root).expect("View is beneath root");
    match planned.get(&filesystem_path_key(relative)) {
        Some(planned) if *planned == relative => Ok(Some(planned)),
        Some(planned)
            if crate::path_key::nfc(&planned.to_string_lossy())
                == crate::path_key::nfc(&relative.to_string_lossy()) =>
        {
            Ok(Some(planned))
        }
        Some(planned) if same_file(path, &root.join(planned))? => Ok(Some(planned)),
        _ => Ok(None),
    }
}

/// Renames `path` to the planned spelling of its last component when the two
/// differ beyond Unicode normalization; returns the entry's current path.
fn respell(path: PathBuf, planned: &Path) -> Result<PathBuf> {
    use unicode_normalization::UnicodeNormalization;
    let (Some(current), Some(wanted)) = (path.file_name(), planned.file_name()) else {
        return Ok(path);
    };
    let nfc = |name: &std::ffi::OsStr| name.to_string_lossy().nfc().collect::<String>();
    if nfc(current) == nfc(wanted) {
        return Ok(path);
    }
    let target = path.with_file_name(wanted);
    std::fs::rename(&path, &target)
        .with_context(|| format!("renaming {} to {}", path.display(), target.display()))?;
    tracing::debug!(from = %path.display(), to = %target.display(), "View respelled");
    Ok(target)
}

fn sweep_orphans(root: &Path, dir: &Path, planned: &PlannedSpellings) -> Result<usize> {
    let mut removed = 0;
    // Collected first: respelling renames entries of the directory being read.
    let entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            let path = match planned_entry(root, &path, &planned.dirs)? {
                Some(spelling) => respell(path, spelling)?,
                None => path,
            };
            removed += sweep_orphans(root, &path, planned)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else {
            match planned_entry(root, &path, &planned.files)? {
                Some(spelling) => {
                    respell(path, spelling)?;
                }
                None => {
                    std::fs::remove_file(&path)?;
                    tracing::debug!(path = %path.display(), "orphaned View removed");
                    removed += 1;
                }
            }
        }
    }
    Ok(removed)
}

pub(super) fn same_file(left: &Path, right: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = std::fs::metadata(left).with_context(|| format!("inspecting {}", left.display()))?;
    match std::fs::metadata(right) {
        Ok(right) => Ok(left.dev() == right.dev() && left.ino() == right.ino()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", right.display())),
    }
}
