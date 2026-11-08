//! Imported v2 shells retain identity/recovery evidence from durable v1 Captures.
use super::*;

pub(super) fn latest_legacy(capture: &Capture) -> Result<Option<OrchestratorProjection>> {
    let stem = capture
        .meta_path
        .file_name()
        .and_then(|v| v.to_str())
        .and_then(|v| v.strip_suffix(".meta.json"))
        .context("invalid T3 Capture sidecar name")?;
    let dir = capture
        .meta_path
        .with_file_name(format!("{stem}.revisions"));
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut latest: Option<(CaptureMeta, PathBuf)> = None;
    for entry in entries {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        let Some(revision) = name.strip_suffix(".revision.json") else {
            continue;
        };
        let meta: CaptureMeta = serde_json::from_reader(std::fs::File::open(&path)?)?;
        ensure!(
            meta.source == SOURCE
                && meta.host == capture.meta.host
                && meta.native_id == capture.meta.native_id,
            "T3 Prior Revision identity mismatch: {}",
            path.display()
        );
        if meta.adapter_version < V2_ADAPTER_VERSION
            && latest
                .as_ref()
                .is_none_or(|(old, _)| meta.captured_at_ms > old.captured_at_ms)
        {
            latest = Some((meta, dir.join(format!("{revision}.ndjson.zst"))));
        }
    }
    latest
        .map(|(meta, body_path)| {
            extract_projection(&Capture {
                meta,
                body_path,
                meta_path: capture.meta_path.clone(),
                written: false,
                outcome: CaptureOutcome::Unchanged,
            })
        })
        .transpose()
}
