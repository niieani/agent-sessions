//! The Enumerate skip gate and the Capture-tree walkers it shares with
//! rebuild.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{CaptureMeta, CapturedSourceFile, GoalAttachmentStatus, cursor, retry_sentinel, t3};

/// Decides at Enumerate time whether a native file may be skipped.
///
/// The Bookmark alone is not a safe skip condition: files arrive with old
/// mtimes (backup restores, `rsync -a`, tar extracts, vendor renames — a
/// rename preserves mtime) and files can change without their mtime moving
/// past the Bookmark. A file is skippable only when it is at or below the
/// Bookmark **and** the Archive already holds its Capture sidecar recording
/// that exact mtime + size — i.e. it is provably captured, unchanged (a
/// sidecar always describes the body next to it; see [`super::write_file_capture`]'s
/// commit order). Everything else is enumerated and settled by the
/// per-session Capture fast path.
#[derive(Debug, Default)]
pub struct EnumerateGate {
    since: Option<i64>,
    /// native_id → what the existing sidecar recorded about the native file.
    captured: HashMap<String, CapturedFile>,
    /// When true, `since` alone justifies a skip (no sidecar required).
    /// Only for stateless remotes (ADR-0002), which hold no Archive to
    /// check sidecars against — see [`EnumerateGate::bookmark_only`].
    trust_bookmark: bool,
}

/// What a Capture sidecar recorded about its native file — the per-Session
/// change-detection state available at Enumerate time.
#[derive(Debug, Clone)]
pub struct CapturedFile {
    /// Native path inside the Store at capture time.
    pub native_path: PathBuf,
    pub mtime_ms: i64,
    pub size: u64,
    pub native_birth_time_ms: Option<i64>,
    pub source_files: Vec<CapturedSourceFile>,
    pub native_meta_mtime_ms: Option<i64>,
    pub native_meta_size: Option<u64>,
    pub native_meta: Option<serde_json::Value>,
    pub adapter_version: u32,
    pub has_retryable_goal_attachment: bool,
    pub has_retryable_orchestrator_attachment: bool,
}

impl EnumerateGate {
    /// A gate that skips purely on the Bookmark: used by `export` on
    /// stateless remotes (ADR-0002), where there is no Archive and thus no
    /// sidecars to prove a file unchanged. The central side compensates:
    /// ingest is idempotent, and a `pull` without a Bookmark (or a manual
    /// full export) re-covers anything a preserved-mtime edge case hid.
    pub fn bookmark_only(since: Option<i64>) -> Self {
        Self {
            since,
            captured: HashMap::new(),
            trust_bookmark: true,
        }
    }
    /// Load the gate for one (Host, Source) from the Archive's Capture
    /// sidecars. Cheap: one walk of `captures/<host>/<source>/` parsing the
    /// small `.meta.json` files. With `since = None` nothing can ever be
    /// skipped, so the walk is skipped too.
    pub fn load(captures_dir: &Path, host: &str, source: &str, since: Option<i64>) -> Result<Self> {
        let mut gate = Self {
            since,
            captured: HashMap::new(),
            trust_bookmark: false,
        };
        if since.is_none() {
            return Ok(gate);
        }
        let mut sidecars = Vec::new();
        let mut bodies = std::collections::HashSet::new();
        collect_capture_files(
            &captures_dir.join(host).join(source),
            &mut sidecars,
            &mut bodies,
        )?;
        for path in sidecars {
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<CaptureMeta>(&raw) else {
                tracing::warn!(
                    sidecar = %path.display(),
                    "unparsable sidecar; its session will be re-checked"
                );
                continue;
            };
            if meta.needs_retry || retry_sentinel(&path).is_file() {
                continue;
            }
            let Some(stem) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".meta.json"))
            else {
                tracing::warn!(sidecar = %path.display(), "sidecar has no valid filename; its session will be re-checked");
                continue;
            };
            let extension = if matches!(meta.source.as_str(), cursor::SOURCE | t3::SOURCE) {
                "ndjson"
            } else {
                "jsonl"
            };
            let expected_body = path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(format!("{stem}.{extension}.zst"));
            if !bodies.contains(&expected_body) {
                tracing::warn!(sidecar = %path.display(), "Capture sidecar has no body; session will be re-captured");
                continue;
            }
            gate.captured.insert(
                meta.native_id,
                CapturedFile {
                    native_path: PathBuf::from(meta.native_path),
                    mtime_ms: meta.source_mtime_ms,
                    size: meta.source_size,
                    native_birth_time_ms: meta.native_birth_time_ms,
                    source_files: meta.source_files,
                    native_meta_mtime_ms: meta.native_meta_mtime_ms,
                    native_meta_size: meta.native_meta_size,
                    native_meta: meta.native_meta.clone(),
                    adapter_version: meta.adapter_version,
                    has_retryable_goal_attachment: meta.goal_attachments.iter().any(|attachment| {
                        matches!(
                            attachment.status,
                            GoalAttachmentStatus::Missing | GoalAttachmentStatus::Unreadable
                        )
                    }),
                    has_retryable_orchestrator_attachment: meta.native_meta.as_ref().is_some_and(
                        |native_meta| {
                            native_meta["attachments"]
                                .as_array()
                                .is_some_and(|attachments| {
                                    attachments.iter().any(|attachment| {
                                        matches!(
                                            attachment["status"].as_str(),
                                            Some("missing" | "unreadable" | "unstable")
                                        )
                                    })
                                })
                        },
                    ),
                },
            );
        }
        Ok(gate)
    }

    /// True when Enumerate may drop this file without Capturing it: at or
    /// below the Bookmark and already captured with this exact mtime + size.
    pub fn skips(
        &self,
        native_id: &str,
        mtime_ms: i64,
        size: u64,
        native_meta_mtime_ms: Option<i64>,
        native_meta_size: Option<u64>,
    ) -> bool {
        let Some(bookmark) = self.since else {
            return false;
        };
        if mtime_ms.max(native_meta_mtime_ms.unwrap_or(i64::MIN)) > bookmark {
            return false;
        }
        self.trust_bookmark
            || self.captured.get(native_id).is_some_and(|c| {
                c.mtime_ms == mtime_ms
                    && c.size == size
                    && c.native_meta_mtime_ms == native_meta_mtime_ms
                    && c.native_meta_size == native_meta_size
            })
    }

    pub fn birth_time_matches(
        &self,
        native_id: &str,
        path: &Path,
        metadata: &std::fs::Metadata,
    ) -> Result<bool> {
        if self.trust_bookmark {
            return Ok(true);
        }
        let birth = super::native_birth_time_ms(path, metadata)?;
        Ok(self
            .captured
            .get(native_id)
            .is_some_and(|c| c.native_birth_time_ms == birth))
    }

    /// The Bookmark this gate was loaded with, when one exists.
    pub fn since(&self) -> Option<i64> {
        self.since
    }

    pub(crate) fn trusts_bookmark(&self) -> bool {
        self.trust_bookmark
    }

    /// Every already-captured file with what its sidecar recorded. Lets an
    /// Adapter that prunes directories at Enumerate time (Codex) re-stat
    /// captured native paths directly, so growth inside a pruned directory
    /// (append bumps the file's mtime, never its parent directory's) is
    /// still seen.
    pub fn captured_files(&self) -> impl Iterator<Item = (&str, &CapturedFile)> {
        self.captured.iter().map(|(id, c)| (id.as_str(), c))
    }

    pub fn captured_file(&self, native_id: &str) -> Option<&CapturedFile> {
        self.captured.get(native_id)
    }

    pub fn requires_adapter_refresh(&self, native_id: &str, adapter_version: u32) -> bool {
        self.captured.get(native_id).is_some_and(|capture| {
            capture.adapter_version < adapter_version
                || capture.has_retryable_goal_attachment
                || capture.has_retryable_orchestrator_attachment
        })
    }
}

/// Recursively collect `*.meta.json` sidecar paths under `dir`. A missing
/// `dir` is an empty result, not an error.
pub(crate) fn collect_sidecars(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_sidecars(&path, out)?;
        } else if path.to_string_lossy().ends_with(".meta.json") {
            out.push(path);
        }
    }
    Ok(())
}

fn collect_capture_files(
    dir: &Path,
    sidecars: &mut Vec<PathBuf>,
    bodies: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_capture_files(&path, sidecars, bodies)?;
        } else if file_type.is_file() {
            if path.to_string_lossy().ends_with(".meta.json") {
                sidecars.push(path);
            } else if path.extension().is_some_and(|extension| extension == "zst") {
                bodies.insert(path);
            }
        }
    }
    Ok(())
}
