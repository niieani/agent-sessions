//! Destination and path-safety rules for Bundle ingest: every local path is
//! derived from trusted Provenance, never from tar entry names.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::adapters::CaptureMeta;

/// Derive a Capture's local destinations from trusted Provenance, never tar
/// paths. Cursor native keys can contain path syntax, so their deterministic
/// Capture stem is used instead of the raw identity.
pub(super) fn bundled_capture_paths(
    base: &Path,
    meta: &CaptureMeta,
    body_name: &str,
) -> Result<(PathBuf, PathBuf)> {
    let adapter = crate::adapters::adapter_for(&meta.source)
        .ok_or_else(|| anyhow::anyhow!("unknown Capture Source"))?;
    anyhow::ensure!(
        adapter.capture_native_id(&meta.native_id) == meta.native_id,
        "unsafe path component: a virtual Session identity cannot name a Capture"
    );
    let root = base
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("Capture namespace missing"))?;
    let paths = crate::adapters::capture_paths(root, &meta.host, &meta.source, &meta.native_id)?;
    if paths.body.file_name().and_then(|s| s.to_str()) != Some(body_name) {
        bail!(
            "malformed bundle: body {body_name} does not match {} sidecar native id {:?}",
            meta.source,
            meta.native_id
        );
    }
    Ok((paths.body, paths.meta))
}

pub(super) fn ensure_safe_component(s: &str) -> Result<()> {
    if s.is_empty() || s.contains(['/', '\\', '\0']) || matches!(s, "." | "..") {
        bail!("unsafe path component in bundle: {s:?}");
    }
    Ok(())
}
