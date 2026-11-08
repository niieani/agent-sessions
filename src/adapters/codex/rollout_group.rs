//! Aggregate Captures for Codex rollover groups: one logical Session whose
//! rollout is split across several physical files, concatenated in order
//! into a single Capture body.

use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::super::{
    Capture, CaptureMeta, CaptureOutcome, CapturedSourceFile, EnumerateGate, NativeFileRef,
    NativeLocation, SessionRef, commit_staged, kept_capture, mtime_ms, now_ms, tmp_sibling,
    write_retry_sentinel,
};
use super::{BODY_CAPTURE_ADAPTER_VERSION, SOURCE};

pub(super) fn rollout_group_is_captured(
    gate: &EnumerateGate,
    native_id: &str,
    files: &[NativeFileRef],
    aggregate_mtime: i64,
    native_meta_revision: Option<(i64, u64)>,
    native_meta: Option<&Value>,
) -> Result<bool> {
    let Some(bookmark) = gate.since() else {
        return Ok(false);
    };
    if aggregate_mtime > bookmark {
        return Ok(false);
    }
    if gate.trusts_bookmark() {
        return Ok(native_meta_revision.is_none_or(|revision| revision.0 <= bookmark));
    }
    let Some(captured) = gate.captured_file(native_id) else {
        return Ok(false);
    };
    // A catalog revision is global and changes for unrelated threads. Local
    // Archives can compare the captured per-Session projection directly,
    // preserving cheap no-op pulls. Stateless exports have no sidecars and
    // are conservatively invalidated by the global revision above.
    if captured.native_meta.as_ref() != native_meta {
        return Ok(false);
    }
    for (index, file) in files.iter().enumerate() {
        let birth =
            super::super::native_birth_time_ms(&file.path, &std::fs::metadata(&file.path)?)?;
        let recorded = if captured.source_files.is_empty() {
            captured.native_birth_time_ms
        } else {
            captured
                .source_files
                .get(index)
                .and_then(|f| f.birth_time_ms)
        };
        if birth != recorded {
            return Ok(false);
        }
    }
    if captured.source_files.is_empty() {
        return Ok(files.len() == 1
            && files[0].path == captured.native_path
            && files[0].mtime_ms == captured.mtime_ms
            && files[0].size == captured.size);
    }
    Ok(captured.source_files.len() == files.len()
        && captured
            .source_files
            .iter()
            .zip(files)
            .all(|(left, right)| {
                Path::new(&left.path) == right.path
                    && left.mtime_ms == right.mtime_ms
                    && left.size == right.size
            }))
}

/// The same physical segments, wherever they live now: equal filenames,
/// mtimes, and sizes in order. Codex moves a whole rollout group between
/// `sessions/` and `archived_sessions/` without touching its bytes.
fn same_segments(recorded: &[CapturedSourceFile], current: &[CapturedSourceFile]) -> bool {
    recorded.len() == current.len()
        && recorded.iter().zip(current).all(|(left, right)| {
            Path::new(&left.path).file_name() == Path::new(&right.path).file_name()
                && left.mtime_ms == right.mtime_ms
                && left.size == right.size
        })
}

pub(super) fn capture_rollout_group(
    session: &SessionRef,
    captures_dir: &Path,
    host: &str,
) -> Result<Capture> {
    super::super::file_capture::retry_busy(|| {
        capture_rollout_group_once(session, captures_dir, host)
    })
}

fn capture_rollout_group_once(
    session: &SessionRef,
    captures_dir: &Path,
    host: &str,
) -> Result<Capture> {
    let base = captures_dir.join(host).join(SOURCE);
    std::fs::create_dir_all(&base)?;
    let paths = crate::adapters::capture_paths(captures_dir, host, SOURCE, &session.native_id)?;
    let (body_path, meta_path) = (paths.body, paths.meta);
    let existing = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CaptureMeta>(&raw).ok())
        .filter(|_| body_path.is_file());

    let mut inputs = Vec::new();
    for source in session.native_files() {
        let input = std::fs::File::open(&source.path)
            .with_context(|| format!("opening Codex rollout segment {}", source.path.display()))?;
        let metadata = input.metadata()?;
        let birth_time_ms = super::super::native_file_birth_time_ms(&input, &metadata)?;
        inputs.push((
            input,
            CapturedSourceFile {
                path: source.path.to_string_lossy().into_owned(),
                mtime_ms: mtime_ms(&metadata),
                size: metadata.len(),
                birth_time_ms,
            },
            metadata.modified()?,
        ));
    }
    let source_files: Vec<CapturedSourceFile> =
        inputs.iter().map(|(_, source, _)| source.clone()).collect();
    let source_mtime_ms = source_files
        .iter()
        .map(|source| source.mtime_ms)
        .max()
        .unwrap_or(0);
    let source_size = source_files.iter().map(|source| source.size).sum();
    let native_birth_time_ms = source_files
        .iter()
        .map(|f| f.birth_time_ms)
        .collect::<Option<Vec<_>>>()
        .and_then(|times| times.into_iter().min());

    if let Some(existing) = &existing
        && same_segments(&existing.source_files, &source_files)
    {
        return kept_capture(
            existing,
            session,
            existing.adapter_version,
            &NativeLocation {
                native_path: source_files[0].path.clone(),
                native_birth_time_ms,
                source_files,
            },
            body_path,
            meta_path,
            CaptureOutcome::Unchanged,
        );
    }

    write_retry_sentinel(&meta_path)?;
    let tmp_body = tmp_sibling(&body_path);
    let _staged = super::super::file_capture::StagedBody(tmp_body.clone());
    let output = std::fs::File::create(&tmp_body)
        .with_context(|| format!("creating aggregate Capture {}", tmp_body.display()))?;
    let writer = BufWriter::new(output);
    let mut encoder = zstd::stream::write::Encoder::new(writer, 0)?;
    for (index, (input, source, _)) in inputs.iter_mut().enumerate() {
        std::io::copy(&mut BufReader::new(&*input), &mut encoder)?;
        if index + 1 < source_files.len() && source.size > 0 {
            input.seek(std::io::SeekFrom::End(-1))?;
            let mut last = [0_u8; 1];
            input.read_exact(&mut last)?;
            if last[0] != b'\n' {
                encoder.write_all(b"\n")?;
            }
        }
    }
    let mut writer = encoder.finish()?;
    writer.flush()?;
    drop(writer);
    super::super::file_capture::after_stream();
    for (input, before, modified) in &inputs {
        let after = input.metadata()?;
        if after.modified()? != *modified || after.len() != before.size {
            return Err(super::super::file_capture::BusyCapture(before.path.clone()).into());
        }
    }

    let meta = CaptureMeta {
        repository_resolution: None,
        schema_version: 1,
        source: SOURCE.to_string(),
        native_id: session.native_id.clone(),
        host: host.to_string(),
        native_path: source_files[0].path.clone(),
        project_dir: None,
        kind: session.kind,
        source_mtime_ms,
        source_size,
        source_files,
        native_meta_mtime_ms: session.native_meta_mtime_ms,
        native_meta_size: session.native_meta_size,
        captured_at_ms: now_ms(),
        adapter_version: BODY_CAPTURE_ADAPTER_VERSION,
        superseded_native_ids: existing
            .as_ref()
            .map(|meta| meta.superseded_native_ids.clone())
            .unwrap_or_default(),
        needs_retry: false,
        native_meta: session.native_meta.clone(),
        native_birth_time_ms,
        goal_attachments: Vec::new(),
    };
    match commit_staged(existing.as_ref(), &tmp_body, &body_path, &meta_path, meta)? {
        Some(capture) => Ok(capture),
        None => {
            tracing::warn!(
                native_id = session.native_id,
                "Codex rollout group lost Dialog its Capture holds; keeping the Capture"
            );
            let existing = existing.expect("only an existing Capture refuses");
            let mut location = NativeLocation::recorded(&existing);
            location.native_birth_time_ms = native_birth_time_ms;
            kept_capture(
                &existing,
                session,
                existing.adapter_version,
                &location,
                body_path,
                meta_path,
                CaptureOutcome::Refused,
            )
        }
    }
}
