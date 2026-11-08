//! Read-only browser companion Store; metadata enumeration, per-Session capture.
use super::ingest::{Evidence, hash};
use crate::adapters::{Capture, CaptureMeta, EnumerateGate, SessionKind, SessionRef, mtime_ms};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

pub(super) fn enumerate(root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut groups: BTreeMap<String, Vec<(PathBuf, Value)>> = BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".meta.json")
        {
            continue;
        }
        let meta: Value = serde_json::from_reader(File::open(&path)?)?;
        ensure!(
            meta["accountId"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "accountId missing: {}",
            path.display()
        );
        let url = meta["url"].as_str().context("url missing")?;
        meta["sha256"].as_str().context("sha256 missing")?;
        crate::timefmt::parse_iso_ms(meta["capturedAt"].as_str().context("capturedAt missing")?)?;
        let id = url
            .split("/backend-api/conversations/")
            .nth(1)
            .map(|tail| tail.split(['?', '/']).next().unwrap().to_string())
            .unwrap_or_else(|| format!("catalog-{}", hash(url.as_bytes())));
        groups.entry(id).or_default().push((path, meta));
    }
    for (path, meta) in super::media::manifests(root)? {
        let id = meta["conversationId"]
            .as_str()
            .context("generated media conversationId missing")?;
        groups.entry(id.into()).or_default().push((path, meta));
    }
    let mut refs = Vec::new();
    for (native_id, mut observations) in groups {
        observations.sort_by(|a, b| a.0.cmp(&b.0));
        let mut size = 0;
        let mut mtime = i64::MIN;
        let account = observations
            .iter()
            .find_map(|(_, m)| m.get("accountId").cloned())
            .context("accountId missing")?;
        let mut descriptors = Vec::new();
        for (path, meta) in observations {
            ensure!(
                meta.get("accountId").is_none_or(|id| id == &account),
                "conflicting ChatGPT accountId for {native_id}"
            );
            let body = if meta["format"] == "chatgpt-generated-image-v1" {
                super::media::asset_path(root, &meta)?
            } else {
                path.with_file_name(
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .replace(".meta.json", ".json"),
                )
            };
            for file in [&path, &body] {
                let stat = std::fs::metadata(file)?;
                size += stat.len();
                mtime = mtime.max(mtime_ms(&stat));
            }
            descriptors.push(json!({"body":body,"meta":meta}));
        }
        if gate.skips(&native_id, mtime, size, None, None) {
            continue;
        }
        refs.push(SessionRef {
            native_id,
            path: root.to_path_buf(),
            additional_files: vec![],
            project_dir: None,
            kind: SessionKind::Main,
            mtime_ms: mtime,
            size,
            native_meta_path: None,
            native_meta_mtime_ms: None,
            native_meta_size: None,
            native_meta: Some(json!({"accountId":account,"observations":descriptors})),
        });
    }
    Ok(refs)
}
pub(super) fn capture(session: &SessionRef, captures: &Path, host: &str) -> Result<Capture> {
    super::media::ingest_assets(
        captures.parent().context("Capture root parent missing")?,
        &session.path,
    )?;
    let native = session
        .native_meta
        .as_ref()
        .context("ChatGPT Store observations missing")?;
    let mut records = Vec::new();
    let mut captured_at = i64::MIN;
    for observation in native["observations"]
        .as_array()
        .context("observations missing")?
    {
        if observation["meta"]["format"] == "chatgpt-generated-image-v1" {
            let mut meta = observation["meta"].clone();
            crate::media::sanitize_metadata(&mut meta);
            let at = crate::timefmt::parse_iso_ms(
                meta["capturedAt"]
                    .as_str()
                    .context("media capturedAt missing")?,
            )?;
            captured_at = captured_at.max(at);
            let payload = serde_json::to_string(&meta)?;
            records.push(Evidence {
                format: "media-index".into(),
                origin: format!(
                    "browser-download:{}",
                    meta["assetId"].as_str().context("assetId missing")?
                ),
                captured_at: at,
                sha256: hash(payload.as_bytes()),
                payload,
            });
            continue;
        }
        let path = Path::new(observation["body"].as_str().context("body path missing")?);
        let before = std::fs::metadata(path)?;
        let payload = std::fs::read_to_string(path)?;
        let _: Box<serde_json::value::RawValue> = serde_json::from_str(&payload)?;
        let meta = &observation["meta"];
        ensure!(
            meta["sha256"].as_str() == Some(&hash(payload.as_bytes())),
            "capture hash mismatch: {}",
            path.display()
        );
        let after = std::fs::metadata(path)?;
        ensure!(
            before.len() == after.len() && mtime_ms(&before) == mtime_ms(&after),
            "native response changed during Capture"
        );
        let at = crate::timefmt::parse_iso_ms(
            meta["capturedAt"].as_str().context("capturedAt missing")?,
        )?;
        captured_at = captured_at.max(at);
        let origin = meta["url"].as_str().context("url missing")?.to_string();
        records.push(Evidence {
            format: if origin.contains("/backend-api/conversations/") {
                "browser"
            } else {
                "catalog"
            }
            .into(),
            origin,
            captured_at: at,
            sha256: hash(payload.as_bytes()),
            payload,
        });
    }
    let meta = CaptureMeta {
        repository_resolution: None,
        schema_version: 1,
        source: "chatgpt".into(),
        native_id: session.native_id.clone(),
        host: host.into(),
        native_path: session.path.to_string_lossy().into_owned(),
        project_dir: None,
        kind: SessionKind::Main,
        source_mtime_ms: session.mtime_ms,
        source_size: session.size,
        native_birth_time_ms: None,
        source_files: vec![],
        native_meta_mtime_ms: None,
        native_meta_size: None,
        captured_at_ms: captured_at,
        adapter_version: 2,
        superseded_native_ids: vec![],
        needs_retry: true,
        native_meta: Some(json!({"accountId":native["accountId"]})),
        goal_attachments: vec![],
    };
    super::capture::merge(captures, meta, records)
}
