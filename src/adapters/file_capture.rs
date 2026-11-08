//! File-backed Capture writing and the staged-body commit shared by every
//! append-oriented ingest path (single native file, Codex rollout group,
//! Bundle ingest).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::supersede::{BodySettlement, RevisionCheck, retain_prior_revision};
use super::{
    Capture, CaptureMeta, CaptureOutcome, CapturedSourceFile, SessionRef, mtime_ms, now_ms,
    revision_check, settle_changed_body, tmp_sibling, write_capture_meta, write_retry_sentinel,
};

/// Only a verified concurrent native write is retryable. IO/parse errors stay failures.
#[derive(Debug, thiserror::Error)]
#[error("native payload changed while being captured: {0}")]
pub struct BusyCapture(pub String);

pub(crate) fn retry_busy<T>(mut capture: impl FnMut() -> Result<T>) -> Result<T> {
    for attempt in 0..3 {
        match capture() {
            Err(error) if error.is::<BusyCapture>() && attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
    unreachable!()
}

/// Remove staged bytes on every exit, including compression and stat failures.
pub(crate) struct StagedBody(pub PathBuf);
impl Drop for StagedBody {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.0.display(), %error, "removing staged Capture failed");
        }
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static AFTER_STREAM: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}
pub(crate) fn after_stream() {
    #[cfg(test)]
    AFTER_STREAM.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// Where a Capture's native bytes currently live in the Store: the sidecar's
/// `nativePath` plus, for multi-file Sessions, each segment's path. Vendors
/// move files without changing them (Codex archiving moves a rollout between
/// `sessions/` and `archived_sessions/`), so a retained Capture can need a new
/// location while its body stays exactly as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeLocation {
    pub native_path: String,
    pub source_files: Vec<CapturedSourceFile>,
    pub native_birth_time_ms: Option<i64>,
}

impl NativeLocation {
    /// The location a sidecar already records — for a Capture kept against
    /// a native payload it does not describe (a refused body).
    pub fn recorded(meta: &CaptureMeta) -> Self {
        Self {
            native_path: meta.native_path.clone(),
            source_files: meta.source_files.clone(),
            native_birth_time_ms: meta.native_birth_time_ms,
        }
    }

    /// A single native file at `session.path`.
    pub fn single(session: &SessionRef, native_birth_time_ms: Option<i64>) -> Self {
        Self {
            native_path: session.path.to_string_lossy().into_owned(),
            source_files: Vec::new(),
            native_birth_time_ms,
        }
    }
}

/// Shared capture writer: streams `session.path` through zstd into the
/// archive layout and writes the Provenance sidecar. Used by file-backed
/// Adapters; never loads the native file into memory.
///
/// If the sidecar already records the native file's exact mtime and size,
/// the file is byte-identical to the Capture and it is reused. Otherwise the
/// new body is staged next to the Capture and settled by Dialog coverage
/// (`commit_staged`): a body covering the Capture's Dialog supersedes it,
/// a diverged one supersedes it while the old body is retained as a Prior
/// Revision, and a truncated one is refused (the Capture stands). Commits
/// are write-temp-then-rename, body before sidecar, so a crash can never
/// leave a torn body behind an intact sidecar.
pub fn write_file_capture(
    source: &str,
    session: &SessionRef,
    captures_dir: &Path,
    host: &str,
    ext: &str,
    adapter_version: u32,
) -> Result<Capture> {
    retry_busy(|| {
        write_file_capture_once(source, session, captures_dir, host, ext, adapter_version)
    })
}

fn write_file_capture_once(
    source: &str,
    session: &SessionRef,
    captures_dir: &Path,
    host: &str,
    ext: &str,
    adapter_version: u32,
) -> Result<Capture> {
    anyhow::ensure!(
        super::adapter_for(source).is_some_and(|a| a.capture_extension() == ext),
        "Capture extension disagrees with Source path policy"
    );
    let paths = super::capture_paths(captures_dir, host, source, &session.native_id)?;
    let (body_path, meta_path) = (paths.body, paths.meta);
    if let Some(parent) = body_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CaptureMeta>(&raw).ok())
        .filter(|_| body_path.is_file());

    // Re-stat the opened file at the IO boundary. Enumeration metadata may
    // already be stale by the time a large Store reaches Capture.
    let input = std::fs::File::open(&session.path)
        .with_context(|| format!("opening native payload {}", session.path.display()))?;
    let before = input.metadata()?;
    let before_mtime = mtime_ms(&before);
    let before_size = before.len();
    let native_birth_time_ms = super::native_file_birth_time_ms(&input, &before)?;

    if let Some(existing) = &existing
        && existing.native_path == session.path.to_string_lossy()
        && revision_check(existing, before_mtime, before_size) == RevisionCheck::Unchanged
    {
        return kept_capture(
            existing,
            session,
            adapter_version,
            &NativeLocation::single(session, native_birth_time_ms),
            body_path,
            meta_path,
            CaptureOutcome::Unchanged,
        );
    }

    let native_meta = read_native_meta(session)?;

    // Establish the pending-derived-work marker before committing a new
    // body/sidecar pair. Existing Provenance remains continuously present.
    write_retry_sentinel(&meta_path)?;

    let tmp_body = tmp_sibling(&body_path);
    let _staged = StagedBody(tmp_body.clone());
    let output = std::fs::File::create(&tmp_body)
        .with_context(|| format!("creating capture {}", tmp_body.display()))?;
    let mut writer = std::io::BufWriter::new(output);
    zstd::stream::copy_encode(std::io::BufReader::new(&input), &mut writer, 0)?;
    writer.flush()?;
    drop(writer);
    after_stream();
    let after = input.metadata()?;
    if after.modified()? != before.modified()? || after.len() != before_size {
        return Err(BusyCapture(session.path.display().to_string()).into());
    }

    // A different native location is not proof of different bytes. Verify
    // before keeping a moved file, so equal mtime/size cannot hide a copy
    // collision while genuine moves retain the Unchanged contract.
    if let Some(existing) = &existing
        && revision_check(existing, before_mtime, before_size) == RevisionCheck::Unchanged
        && super::supersede::body_fingerprint(&tmp_body)?
            == super::supersede::body_fingerprint(&body_path)?
    {
        std::fs::remove_file(&tmp_body)?;
        return kept_capture(
            existing,
            session,
            adapter_version,
            &NativeLocation::single(session, native_birth_time_ms),
            body_path,
            meta_path,
            CaptureOutcome::Unchanged,
        );
    }

    let meta = CaptureMeta {
        repository_resolution: None,
        schema_version: 1,
        source: source.to_string(),
        native_id: session.native_id.clone(),
        host: host.to_string(),
        native_path: session.path.to_string_lossy().into_owned(),
        project_dir: session.project_dir.clone(),
        kind: session.kind,
        source_mtime_ms: before_mtime,
        source_size: before_size,
        source_files: Vec::new(),
        native_meta_mtime_ms: session.native_meta_mtime_ms,
        native_meta_size: session.native_meta_size,
        captured_at_ms: now_ms(),
        adapter_version,
        superseded_native_ids: existing
            .as_ref()
            .map(|meta| meta.superseded_native_ids.clone())
            .unwrap_or_default(),
        needs_retry: false,
        native_meta,
        native_birth_time_ms,
        goal_attachments: Vec::new(),
    };
    match commit_staged(existing.as_ref(), &tmp_body, &body_path, &meta_path, meta)? {
        Some(capture) => Ok(capture),
        None => {
            let existing = existing.expect("only an existing Capture refuses");
            tracing::warn!(
                native = %session.path.display(),
                "native payload lost Dialog its Capture holds; keeping the Capture"
            );
            let mut location = NativeLocation::recorded(&existing);
            location.native_birth_time_ms = native_birth_time_ms;
            kept_capture(
                &existing,
                session,
                adapter_version,
                &location,
                body_path,
                meta_path,
                CaptureOutcome::Refused,
            )
        }
    }
}

/// An existing Capture whose body stays: refresh its sidecar when the native
/// metadata or the native location moved, otherwise reuse it as is.
///
/// Only changed native metadata upgrades `Unchanged` to `Extended`: it can
/// change the Conversation (a thread name). A pure move records the new
/// location and stays `Unchanged` — the Session holds exactly what it held.
pub(crate) fn kept_capture(
    existing: &CaptureMeta,
    session: &SessionRef,
    adapter_version: u32,
    location: &NativeLocation,
    body_path: PathBuf,
    meta_path: PathBuf,
    outcome: CaptureOutcome,
) -> Result<Capture> {
    let refreshed = refresh_sidecar(existing, session, adapter_version, location, &meta_path)?;
    let written = refreshed.is_some();
    let native_meta_moved = refreshed
        .as_ref()
        .is_some_and(|refresh| refresh.native_meta_moved);
    Ok(Capture {
        body_path,
        meta_path,
        meta: refreshed.map_or_else(|| existing.clone(), |refresh| refresh.meta),
        written,
        outcome: match outcome {
            CaptureOutcome::Unchanged if native_meta_moved => CaptureOutcome::Extended,
            outcome => outcome,
        },
    })
}

struct SidecarRefresh {
    meta: CaptureMeta,
    native_meta_moved: bool,
}

/// Atomically refresh a retained Capture's sidecar when the Session's native
/// metadata appeared or changed, or its native files moved, leaving the
/// body's recorded revision alone. `None` when nothing moved.
fn refresh_sidecar(
    existing: &CaptureMeta,
    session: &SessionRef,
    adapter_version: u32,
    location: &NativeLocation,
    meta_path: &Path,
) -> Result<Option<SidecarRefresh>> {
    let native_meta_moved = existing.native_meta_mtime_ms != session.native_meta_mtime_ms
        || existing.native_meta_size != session.native_meta_size
        || (session.native_meta.is_some() && existing.native_meta != session.native_meta);
    let relocated = NativeLocation::recorded(existing) != *location;
    if !native_meta_moved && !relocated {
        return Ok(None);
    }
    let mut updated = existing.clone();
    if native_meta_moved {
        updated.native_meta = read_native_meta(session)?;
        updated.native_meta_mtime_ms = session.native_meta_mtime_ms;
        updated.native_meta_size = session.native_meta_size;
        updated.captured_at_ms = now_ms();
        updated.adapter_version = adapter_version;
    }
    updated.native_path = location.native_path.clone();
    updated.project_dir = session.project_dir.clone();
    updated.source_files = location.source_files.clone();
    updated.native_birth_time_ms = location.native_birth_time_ms;
    write_capture_meta(meta_path, &updated)?;
    Ok(Some(SidecarRefresh {
        meta: updated,
        native_meta_moved,
    }))
}

/// Settle the body staged at `staged` (compressed, not yet committed)
/// against `existing` and commit it per the [`BodySettlement`]. `None` means
/// refused: the staged body lost Dialog and was discarded, the Capture
/// stands. A parse failure of either body removes the staged body and
/// propagates — the Capture is never replaced on a guess.
pub(crate) fn commit_staged(
    existing: Option<&CaptureMeta>,
    staged: &Path,
    body_path: &Path,
    meta_path: &Path,
    mut meta: CaptureMeta,
) -> Result<Option<Capture>> {
    // Keep the original fact revision when no new observation was possible.
    // A timeout must neither move the Session nor settle this new revision.
    if let Some(existing) = existing
        && (meta.repository_resolution.is_none()
            || existing.repository_resolution.as_ref().is_some_and(|f| {
                matches!(f.lookup, crate::repository_resolution::Lookup::Resolved(_))
            }))
    {
        meta.repository_resolution = existing.repository_resolution.clone();
    }
    let settlement = match existing {
        None => None,
        Some(existing) => {
            let current = Capture {
                body_path: body_path.to_path_buf(),
                meta_path: meta_path.to_path_buf(),
                meta: existing.clone(),
                written: false,
                outcome: CaptureOutcome::Unchanged,
            };
            // Goal attachments are resolved after the body lands; compare
            // with the Capture's so both bodies project the same Dialog.
            let mut staged_meta = meta.clone();
            if staged_meta.goal_attachments.is_empty() {
                staged_meta.goal_attachments = existing.goal_attachments.clone();
            }
            let incoming = Capture {
                body_path: staged.to_path_buf(),
                meta_path: meta_path.to_path_buf(),
                meta: staged_meta,
                written: false,
                outcome: CaptureOutcome::Unchanged,
            };
            let settled = settle_changed_body(&current, &incoming).and_then(|settlement| {
                if settlement == BodySettlement::Revise {
                    let retained = retain_prior_revision(&current)?;
                    tracing::warn!(
                        capture = %body_path.display(),
                        revision = %retained.display(),
                        "incoming payload revises its Capture; prior body retained as a revision"
                    );
                }
                Ok(settlement)
            });
            match settled {
                Ok(settlement) => Some(settlement),
                Err(error) => {
                    let _ = std::fs::remove_file(staged);
                    return Err(error);
                }
            }
        }
    };
    if settlement == Some(BodySettlement::Refuse) {
        std::fs::remove_file(staged)
            .with_context(|| format!("discarding refused body {}", staged.display()))?;
        return Ok(None);
    }
    std::fs::rename(staged, body_path)
        .with_context(|| format!("committing capture {}", body_path.display()))?;
    write_capture_meta(meta_path, &meta)?;
    Ok(Some(Capture {
        body_path: body_path.to_path_buf(),
        meta_path: meta_path.to_path_buf(),
        meta,
        written: true,
        outcome: match settlement {
            None => CaptureOutcome::New,
            Some(BodySettlement::Revise) => CaptureOutcome::Revised,
            Some(_) => CaptureOutcome::Extended,
        },
    }))
}

fn read_native_meta(session: &SessionRef) -> Result<Option<serde_json::Value>> {
    if session.native_meta.is_some() {
        return Ok(session.native_meta.clone());
    }
    let Some(path) = &session.native_meta_path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading native meta {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

#[cfg(test)]
mod busy_tests {
    use super::*;
    use crate::adapters::{Adapter, NativeFileRef, SessionKind};
    use std::{cell::Cell, rc::Rc};

    fn session(path: PathBuf) -> SessionRef {
        SessionRef {
            native_id: "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa".into(),
            path,
            additional_files: vec![],
            project_dir: None,
            kind: SessionKind::Main,
            mtime_ms: 0,
            size: 0,
            native_meta_path: None,
            native_meta_mtime_ms: None,
            native_meta_size: None,
            native_meta: None,
        }
    }

    // Mutate at the stat boundary deterministically, without a timing race.
    #[test]
    fn both_paths_retry_preserve_prior_and_clean_staged_bytes() {
        for aggregate in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let native = dir.path().join("native.jsonl");
            std::fs::write(&native, b"{}\n").unwrap();
            let mut session = session(native.clone());
            if aggregate {
                let second = dir.path().join("second.jsonl");
                std::fs::write(&second, b"{}\n").unwrap();
                session.additional_files.push(NativeFileRef {
                    path: second,
                    mtime_ms: 0,
                    size: 3,
                });
            }
            let captures = dir.path().join("captures");
            let capture = || {
                if aggregate {
                    crate::adapters::codex::CodexAdapter.capture(&session, &captures, "fixture")
                } else {
                    write_file_capture("claude", &session, &captures, "fixture", "jsonl", 1)
                }
            };
            let prior = capture().unwrap();
            let body = std::fs::read(&prior.body_path).unwrap();
            let meta = std::fs::read(&prior.meta_path).unwrap();
            std::fs::write(&native, b"{}\n{}\n").unwrap();
            let count = Rc::new(Cell::new(0));
            let observed = count.clone();
            let target = native.clone();
            AFTER_STREAM.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    observed.set(observed.get() + 1);
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&target)
                        .unwrap()
                        .write_all(b"{}\n")
                        .unwrap();
                }))
            });
            let error = capture().unwrap_err();
            AFTER_STREAM.with(|hook| *hook.borrow_mut() = None);
            assert!(error.is::<BusyCapture>(), "{error:#}");
            assert_eq!(count.get(), 3);
            assert_eq!(std::fs::read(&prior.body_path).unwrap(), body);
            assert_eq!(std::fs::read(&prior.meta_path).unwrap(), meta);
            for entry in std::fs::read_dir(prior.body_path.parent().unwrap()).unwrap() {
                assert!(
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".tmp")
                );
            }
            // One concurrent append settles on the immediate re-read.
            let count = Rc::new(Cell::new(0));
            let observed = count.clone();
            let target = native.clone();
            AFTER_STREAM.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if observed.get() == 0 {
                        std::fs::OpenOptions::new()
                            .append(true)
                            .open(&target)
                            .unwrap()
                            .write_all(b"{}\n")
                            .unwrap();
                    }
                    observed.set(observed.get() + 1);
                }))
            });
            capture().unwrap();
            AFTER_STREAM.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(count.get(), 2);
            std::fs::remove_file(&native).unwrap();
            let error = capture().unwrap_err();
            assert!(!error.is::<BusyCapture>());
        }
    }
}
