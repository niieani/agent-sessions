//! Generated media is durable evidence; rendered copies are disposable Views.
use crate::archive::Archive;
pub(super) use crate::media::hash_file;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::fs::File;
use std::path::{Component, Path};
pub(super) fn manifests(root: &Path) -> Result<Vec<(std::path::PathBuf, Value)>> {
    let dir = root.join("assets");
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".meta.json") {
            let value: Value = serde_json::from_reader(File::open(&path)?)?;
            if value.get("format").and_then(Value::as_str) == Some("chatgpt-generated-image-v1") {
                result.push((path, value));
            }
        }
    }
    Ok(result)
}
pub(super) fn asset_path(root: &Path, meta: &Value) -> Result<std::path::PathBuf> {
    let relative = Path::new(
        meta.get("path")
            .and_then(Value::as_str)
            .context("asset path missing")?,
    );
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "unsafe asset path"
    );
    Ok(root.join(relative))
}
pub(super) fn ingest_assets(archive_root: &Path, input: &Path) -> Result<()> {
    let store = crate::media::MediaStore::new(archive_root);
    for (_, meta) in manifests(input)? {
        let source = asset_path(input, &meta)?;
        let (hash, bytes) = hash_file(&source)?;
        ensure!(
            meta["sha256"].as_str() == Some(&hash) && meta["bytes"].as_u64() == Some(bytes),
            "generated asset hash/length mismatch"
        );
        let mime = meta["mime"]
            .as_str()
            .context("generated asset MIME missing")?;
        let id = store.put(File::open(&source)?, mime)?;
        let asset = meta["assetId"]
            .as_str()
            .context("generated assetId missing")?;
        store.record(
            "chatgpt",
            asset,
            id,
            if confirmed(&meta) { 2 } else { 1 },
            meta.clone(),
        )?;
    }
    Ok(())
}
pub(super) fn saved_asset_ids(archive: &Archive) -> Result<std::collections::BTreeSet<String>> {
    let mut saved = std::collections::BTreeSet::new();
    let store = crate::media::MediaStore::new(archive.root());
    for capture in super::ingest::captures(archive)? {
        for id in super::ingest::read_graph(&capture)?
            .generated_assets()
            .keys()
        {
            if let Some(record) = store.reference("chatgpt", id)?
                && record.quality >= 2
                && store.object(&record.media).is_file()
            {
                saved.insert(id.clone());
            }
        }
    }
    Ok(saved)
}

fn confirmed(meta: &Value) -> bool {
    matches!(
        meta["quality"].as_str(),
        Some("original" | "export-original")
    )
}
