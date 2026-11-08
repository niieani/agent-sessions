//! Durable title work: a failed refresh must not make fresh Day Digests stale
//! or make a previously stored Session title look successfully refreshed.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::files;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingTitle {
    source: String,
    native_id: String,
}

fn path(root: &Path, source: &str, native_id: &str) -> Result<PathBuf> {
    Ok(files::digest_dir(root, source, native_id)?.join("session-title.pending"))
}

pub(super) fn pending(root: &Path, source: &str, native_id: &str) -> Result<bool> {
    let path = path(root, source, native_id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let marker: PendingTitle =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    ensure!(
        marker.source == source && marker.native_id == native_id,
        "Session title retry identity mismatch in {}",
        path.display()
    );
    Ok(true)
}

/// Arm before publishing a Day Digest, so interruption between that publication
/// and the title request cannot leave the title permanently unrefreshed.
pub(super) fn mark(root: &Path, source: &str, native_id: &str) -> Result<()> {
    if !pending(root, source, native_id)? {
        files::write_json(
            root,
            &path(root, source, native_id)?,
            &PendingTitle {
                source: source.into(),
                native_id: native_id.into(),
            },
        )?;
    }
    Ok(())
}

pub(super) fn clear(root: &Path, source: &str, native_id: &str) -> Result<()> {
    let path = path(root, source, native_id)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))
}
