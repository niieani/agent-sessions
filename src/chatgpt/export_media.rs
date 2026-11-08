//! Reuse generated output bytes already present in an immutable ChatGPT export.
use super::Graph;
use super::ingest::{
    ArraySeed, Evidence, Stats, captures, extract_import, hash, preserve, read_evidence,
};
use crate::archive::Archive;
use anyhow::{Context, Result, ensure};
use serde::de::DeserializeSeed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Default, Serialize, Deserialize)]
pub struct MediaReport {
    pub generated_references: usize,
    pub unassigned_rows: usize,
    pub extracted: usize,
    pub already_saved: usize,
    pub missing: Vec<Value>,
    pub failed: Vec<String>,
}
fn members(zip: &Path) -> Result<BTreeSet<String>> {
    let output = Command::new("/usr/bin/unzip")
        .arg("-Z1")
        .arg(zip)
        .output()?;
    ensure!(output.status.success(), "cannot enumerate export ZIP");
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::to_owned)
        .collect())
}
fn mime(bytes: &[u8]) -> Result<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok(("image/webp", "webp"))
    } else {
        anyhow::bail!("unsupported generated image signature")
    }
}
fn extract(archive: &Archive, zip: &Path, member: &str, reference: &Value) -> Result<bool> {
    let store = crate::media::MediaStore::new(archive.root());
    let asset = reference["assetId"]
        .as_str()
        .context("generated asset identity missing")?;
    if let Some(record) = store.reference("chatgpt", asset)?
        && record.quality >= 2
    {
        return Ok(false);
    }
    let root = archive.root().join("media");
    std::fs::create_dir_all(&root)?;
    let mut child = Command::new("/usr/bin/unzip")
        .arg("-p")
        .arg(zip)
        .arg(member)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    let copied = std::io::copy(&mut child.stdout.take().unwrap(), &mut temporary);
    if copied.is_err() {
        let _ = child.kill();
    }
    let exit = child.wait()?;
    let bytes = copied?;
    ensure!(exit.success(), "cannot read export image {member}");
    if let Some(expected) = reference["expectedBytes"].as_u64() {
        ensure!(bytes == expected, "export image byte length mismatch");
    }
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let mut prefix = [0u8; 12];
    let count = File::open(temporary.path())?.read(&mut prefix)?;
    let (mime, extension) = mime(&prefix[..count])?;
    if let Some(expected) = reference["mime"].as_str() {
        ensure!(mime == expected, "export generated image MIME mismatch");
    }
    let (digest, _) = super::media::hash_file(temporary.path())?;
    let id = store.put(File::open(temporary.path())?, mime)?;
    ensure!(id.sha256 == digest, "export media hash changed");
    let metadata = serde_json::json!({"assetId":asset,"conversationId":reference["conversationId"],"messageId":reference["messageId"],"mime":mime,"bytes":bytes,"sha256":digest,"extension":extension,"quality":"export-original","origin":{"kind":"chatgpt-export","member":member,"zip":zip.to_string_lossy()}});
    store.record("chatgpt", asset, id, 2, metadata)?;
    Ok(true)
}
pub(super) fn ingest(archive: &Archive, zip: &Path, account_id: &str) -> Result<MediaReport> {
    let available = members(zip)?;
    let mut report = MediaReport::default();
    if available.contains("library_files.json") {
        let mut child = Command::new("/usr/bin/unzip")
            .arg("-p")
            .arg(zip)
            .arg("library_files.json")
            .stdout(Stdio::piped())
            .spawn()?;
        let mut de =
            serde_json::Deserializer::from_reader(BufReader::new(child.stdout.take().unwrap()));
        let result = ArraySeed(&mut |raw: Box<RawValue>| {
            let payload = raw.get();
            let row: Value = serde_json::from_str(payload)?;
            if !row["image_gen_generation_id"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
            {
                return Ok(());
            }
            let conversation = row["origination_thread_id"]
                .as_str()
                .filter(|id| !id.is_empty());
            ensure!(
                row["origination_thread_id"].is_null() || row["origination_thread_id"].is_string(),
                "invalid export media conversation identity"
            );
            let asset = row["file_id"]
                .as_str()
                .context("generated export row lacks file_id")?;
            let evidence = Evidence {
                format: if conversation.is_some() {
                    "export-media-index"
                } else {
                    "catalog"
                }
                .into(),
                origin: format!("export:library_files.json:{asset}"),
                captured_at: 0,
                sha256: hash(payload.as_bytes()),
                payload: payload.into(),
            };
            if let Some(conversation) = conversation {
                preserve(archive, conversation, evidence, account_id)?;
            } else {
                // Catalog identity preserves unowned metadata without inventing a Session.
                let identity = format!("catalog-media-{}", evidence.sha256);
                preserve(archive, &identity, evidence, account_id)?;
                report.unassigned_rows += 1;
            }
            Ok(())
        })
        .deserialize(&mut de)
        .context("reading export generated media metadata");
        if result.is_err() {
            let _ = child.kill();
        }
        let exit = child.wait()?;
        result?;
        ensure!(exit.success(), "cannot extract library metadata");
    }
    for capture in captures(archive)? {
        let records = read_evidence(&capture)?
            .into_iter()
            .filter(|r| matches!(r.format.as_str(), "export" | "export-media-index"))
            .collect::<Vec<_>>();
        if records.is_empty() {
            continue;
        }
        let graph = Graph::from_evidence(&capture.meta.native_id, &records)?;
        for reference in graph.generated_assets().values() {
            report.generated_references += 1;
            let asset = reference["assetId"]
                .as_str()
                .context("asset identity absent")?;
            let options = [
                format!("{asset}.dat"),
                format!("{asset}.png"),
                format!("{asset}.webp"),
                format!("{asset}.jpg"),
            ];
            let Some(member) = options.iter().find(|name| available.contains(*name)) else {
                report.missing.push(reference.clone());
                continue;
            };
            match extract(archive, zip, member, reference) {
                Ok(true) => report.extracted += 1,
                Ok(false) => report.already_saved += 1,
                Err(e) => report.failed.push(format!("{asset}: {e:#}")),
            }
        }
    }
    let dir = archive.root().join("media/reports");
    std::fs::create_dir_all(&dir)?;
    super::ingest::atomic_json(&dir.join("export-report.json"), &report)?;
    Ok(report)
}
pub fn ingest_export_media(archive: &Archive, zip: &Path, account_id: &str) -> Result<MediaReport> {
    let lock = archive.acquire_writer_lock()?;
    let mut report = ingest(archive, zip, account_id)?;
    let Stats { failed, .. } = extract_import(archive)?;
    drop(lock);
    report.failed.extend(failed);
    Ok(report)
}
