//! One additive Capture writer shared by Store collection and one-time import.
use super::ingest::{Evidence, atomic_json, read_evidence};
use crate::adapters::{Capture, CaptureMeta, CaptureOutcome};
use anyhow::Result;
use std::{fs::File, io::Write, path::Path};

pub(super) fn merge(
    captures: &Path,
    mut meta: CaptureMeta,
    incoming: Vec<Evidence>,
) -> Result<Capture> {
    let base = captures.join(&meta.host).join("chatgpt");
    std::fs::create_dir_all(&base)?;
    let paths =
        crate::adapters::capture_paths(captures, &meta.host, &meta.source, &meta.native_id)?;
    let (body_path, meta_path) = (paths.body, paths.meta);
    let existing = if meta_path.exists() {
        Some(Capture {
            meta: serde_json::from_reader(File::open(&meta_path)?)?,
            body_path: body_path.clone(),
            meta_path: meta_path.clone(),
            written: false,
            outcome: CaptureOutcome::Unchanged,
        })
    } else {
        None
    };
    let mut records = if let Some(old) = &existing {
        read_evidence(old)?
    } else {
        Vec::new()
    };
    records.extend(incoming);
    records.sort_by(|a, b| {
        (&a.format, &a.origin, a.captured_at, &a.sha256).cmp(&(
            &b.format,
            &b.origin,
            b.captured_at,
            &b.sha256,
        ))
    });
    records.dedup_by(|a, b| {
        a.format == b.format
            && a.origin == b.origin
            && a.captured_at == b.captured_at
            && a.sha256 == b.sha256
    });
    let records = super::snapshots::compact(records)?;
    super::Graph::from_evidence(&meta.native_id, &records)?;
    let unchanged = existing.as_ref().is_some_and(|old| {
        old.meta.source_mtime_ms == meta.source_mtime_ms
            && old.meta.source_size == meta.source_size
            && old.meta.native_meta == meta.native_meta
    }) && existing
        .as_ref()
        .map(read_evidence)
        .transpose()?
        .as_ref()
        .is_some_and(|old| old == &records);
    let outcome = if unchanged {
        CaptureOutcome::Unchanged
    } else if existing.is_some() {
        CaptureOutcome::Extended
    } else {
        CaptureOutcome::New
    };
    if !unchanged {
        crate::adapters::write_retry_sentinel(&meta_path)?;
        let mut stage = tempfile::NamedTempFile::new_in(&base)?;
        {
            let mut writer = zstd::stream::write::Encoder::new(stage.as_file_mut(), 3)?;
            for record in &records {
                serde_json::to_writer(&mut writer, record)?;
                writer.write_all(b"\n")?;
            }
            writer.finish()?;
        }
        stage.as_file().sync_all()?;
        stage.persist(&body_path)?;
        meta.needs_retry = true;
        atomic_json(&meta_path, &meta)?;
    }
    Ok(Capture {
        body_path,
        meta_path,
        meta: if unchanged {
            existing.unwrap().meta
        } else {
            meta
        },
        written: !unchanged,
        outcome,
    })
}
