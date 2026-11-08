//! Supersede policy: what an incoming native payload may do to an existing
//! Capture. One rule set for both ingest paths — local capture
//! ([`super::write_file_capture`], the Codex rollout group) and Bundle ingest.
//!
//! Append-oriented file Sources are judged by the Dialog their Adapter parses
//! out of both bodies, never by bytes: vendors rewrite session files into
//! leaner formats (Codex, 2026-09) as well as prune and rewind them, so size
//! says nothing about which body holds more of the conversation
//! ([ADR](../../docs/adr/2026-09-28-supersede-by-dialog-coverage.md)).
//! Logical SQLite Sources keep their revision-first order.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{Capture, CaptureMeta, adapter_for, cursor, t3, tmp_sibling};
use crate::model::{Conversation, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureOrdering {
    AppendBodyFirst,
    LogicalRevisionFirst,
}

/// Source-family semantics shared by local Capture settlement, Bundle ingest,
/// and projection election. Keep SQLite-backed Sources on one revision-first
/// path rather than growing Adapter-local policy forks.
pub(crate) fn capture_ordering(source: &str) -> CaptureOrdering {
    match source {
        cursor::SOURCE | t3::SOURCE => CaptureOrdering::LogicalRevisionFirst,
        _ => CaptureOrdering::AppendBodyFirst,
    }
}

/// The cheap first check, from the sidecar's recorded revision alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionCheck {
    /// Same revision + discriminator (file: mtime + size) as the sidecar
    /// records: provably the same payload; reuse the existing Capture.
    Unchanged,
    /// A logical SQLite revision older than the Capture's: refuse.
    Stale,
    /// The payload differs. Logical Sources supersede; append-oriented
    /// Sources settle by Dialog coverage ([`settle_changed_body`]).
    Changed,
}

pub fn revision_check(
    existing: &CaptureMeta,
    incoming_revision: i64,
    incoming_discriminator: u64,
) -> RevisionCheck {
    if existing.source_mtime_ms == incoming_revision
        && existing.source_size == incoming_discriminator
    {
        RevisionCheck::Unchanged
    } else if capture_ordering(&existing.source) == CaptureOrdering::LogicalRevisionFirst
        && incoming_revision < existing.source_mtime_ms
    {
        RevisionCheck::Stale
    } else {
        RevisionCheck::Changed
    }
}

/// T3 v1 and v2 timestamps are incomparable: imports omit old runtime/event
/// clocks. Adapter revision 3 is the fixed format boundary, not a general
/// license for a newer Adapter to accept a native rollback.
pub(crate) fn storage_generation(source: &str, adapter_version: u32) -> u32 {
    u32::from(source == t3::SOURCE && adapter_version >= t3::V2_ADAPTER_VERSION)
}

pub(crate) fn revision_check_for_adapter(
    existing: &CaptureMeta,
    revision: i64,
    discriminator: u64,
    adapter_version: u32,
) -> RevisionCheck {
    let old = storage_generation(&existing.source, existing.adapter_version);
    let new = storage_generation(&existing.source, adapter_version);
    match new.cmp(&old) {
        Ordering::Greater => RevisionCheck::Changed,
        Ordering::Less => RevisionCheck::Stale,
        Ordering::Equal => revision_check(existing, revision, discriminator),
    }
}

/// How a changed body settles against the existing Capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodySettlement {
    /// The incoming body holds everything the Capture held: replace it.
    Supersede,
    /// The Dialogs diverged: keep the old body as a retained Prior Revision
    /// and make the incoming body current.
    Revise,
    /// The incoming body lost Dialog from the end: keep the Capture.
    Refuse,
}

/// Settle a [`RevisionCheck::Changed`] body. `incoming` is a staged Capture
/// (its body not yet committed). Either body failing to parse is an error —
/// never a silent size-based fallback.
pub fn settle_changed_body(existing: &Capture, incoming: &Capture) -> Result<BodySettlement> {
    if capture_ordering(&existing.meta.source) == CaptureOrdering::LogicalRevisionFirst {
        return Ok(
            if storage_generation(&incoming.meta.source, incoming.meta.adapter_version)
                > storage_generation(&existing.meta.source, existing.meta.adapter_version)
            {
                BodySettlement::Revise
            } else {
                BodySettlement::Supersede
            },
        );
    }
    let adapter = adapter_for(&existing.meta.source).with_context(|| {
        format!(
            "no Adapter to compare {} Captures of {}",
            existing.meta.source, existing.meta.native_id
        )
    })?;
    let old = adapter
        .body_dialog(existing)
        .with_context(|| format!("parsing Capture {}", existing.body_path.display()))?;
    let new = adapter.body_dialog(incoming).with_context(|| {
        format!(
            "parsing incoming payload for {} {}",
            incoming.meta.source, incoming.meta.native_id
        )
    })?;
    Ok(
        match DialogCoverage::of(&old).compare(&DialogCoverage::of(&new)) {
            Coverage::Covers => BodySettlement::Supersede,
            Coverage::Diverged => BodySettlement::Revise,
            Coverage::Truncated => BodySettlement::Refuse,
        },
    )
}

/// One Dialog Message's identity for coverage: role, timestamp, and text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MessageKey {
    role: Role,
    timestamp_ms: Option<i64>,
    text: u64,
}

/// The ordered Dialog one body holds plus the last moment it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DialogCoverage {
    messages: Vec<MessageKey>,
    last_activity_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    Covers,
    Truncated,
    Diverged,
}

impl DialogCoverage {
    pub(crate) fn of(conversation: &Conversation) -> Self {
        let messages: Vec<MessageKey> = conversation
            .messages
            .iter()
            .map(|message| {
                let mut hasher = DefaultHasher::new();
                message.text.hash(&mut hasher);
                MessageKey {
                    role: message.role,
                    timestamp_ms: message.timestamp_ms,
                    text: hasher.finish(),
                }
            })
            .collect();
        let last_activity_ms = messages
            .iter()
            .filter_map(|message| message.timestamp_ms)
            .chain(conversation.updated_at_ms)
            .max();
        Self {
            messages,
            last_activity_ms,
        }
    }

    /// Does `incoming` hold everything `self` (the existing Capture) held?
    pub(crate) fn compare(&self, incoming: &Self) -> Coverage {
        let shared = self
            .messages
            .iter()
            .zip(&incoming.messages)
            .take_while(|(old, new)| old == new)
            .count();
        let existing_is_prefix = shared == self.messages.len();
        let incoming_is_prefix = shared == incoming.messages.len();
        let span = incoming.last_activity_ms.cmp(&self.last_activity_ms);
        match (existing_is_prefix, incoming_is_prefix) {
            (true, _) if span != Ordering::Less => Coverage::Covers,
            // Same Dialog but the incoming span ends earlier: trailing
            // activity was cut.
            (true, true) => Coverage::Truncated,
            (false, true) => Coverage::Truncated,
            _ => Coverage::Diverged,
        }
    }
}

/// Keep `existing` as a Prior Revision before a diverged body replaces it:
/// `<native-id>.revisions/<captured-at-ms>.<ext>.zst` plus
/// `<captured-at-ms>.revision.json` (its Provenance, verbatim). The body is
/// hard-linked, so the current Capture stays intact until the caller renames
/// the new body over it. Revisions are preserved evidence only — never
/// Enumerated, Extracted, or rebuilt as Captures.
pub(crate) fn retain_prior_revision(existing: &Capture) -> Result<PathBuf> {
    let meta_name = file_name(&existing.meta_path)?;
    let stem = meta_name
        .strip_suffix(".meta.json")
        .with_context(|| format!("not a sidecar: {}", existing.meta_path.display()))?;
    let body_name = file_name(&existing.body_path)?;
    let ext = body_name
        .strip_prefix(&format!("{stem}."))
        .with_context(|| format!("body does not match sidecar: {body_name}"))?;
    let dir = existing
        .meta_path
        .with_file_name(format!("{stem}.revisions"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating revisions {}", dir.display()))?;
    // Compare decompressed evidence: compression settings and observation
    // timestamps are not body identity. Stream old revisions with bounded memory.
    let fingerprint = body_fingerprint(&existing.body_path)?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "zst")
            && body_fingerprint(&path)? == fingerprint
        {
            return Ok(path);
        }
    }
    let revision = existing.meta.captured_at_ms;
    let body = dir.join(format!("{revision}.{ext}"));
    let meta = dir.join(format!("{revision}.revision.json"));
    if body.exists() || meta.exists() {
        bail!("Prior Revision {} already exists", body.display());
    }
    std::fs::hard_link(&existing.body_path, &body)
        .with_context(|| format!("retaining Prior Revision {}", body.display()))?;
    let tmp = tmp_sibling(&meta);
    std::fs::write(&tmp, serde_json::to_string_pretty(&existing.meta)?)?;
    std::fs::rename(&tmp, &meta)
        .with_context(|| format!("committing Prior Revision {}", meta.display()))?;
    Ok(body)
}

pub(crate) fn body_fingerprint(path: &Path) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut reader = zstd::stream::read::Decoder::new(std::fs::File::open(path)?)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().to_vec())
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("unusable Capture path {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(messages: &[(Role, i64, &str)], last: i64) -> DialogCoverage {
        let mut coverage = DialogCoverage {
            messages: Vec::new(),
            last_activity_ms: Some(last),
        };
        for (role, ts, text) in messages {
            let mut hasher = DefaultHasher::new();
            text.to_string().hash(&mut hasher);
            coverage.messages.push(MessageKey {
                role: *role,
                timestamp_ms: Some(*ts),
                text: hasher.finish(),
            });
        }
        coverage
    }

    const BASE: [(Role, i64, &str); 2] = [(Role::User, 1, "hi"), (Role::Assistant, 2, "hello")];

    #[test]
    fn identical_dialog_and_span_covers() {
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&BASE, 5)),
            Coverage::Covers
        );
    }

    #[test]
    fn appended_dialog_or_later_activity_covers() {
        let grown = [BASE[0], BASE[1], (Role::User, 6, "more")];
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&grown, 6)),
            Coverage::Covers
        );
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&BASE, 9)),
            Coverage::Covers
        );
    }

    #[test]
    fn lost_tail_messages_or_trailing_activity_is_truncated() {
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&BASE[..1], 5)),
            Coverage::Truncated
        );
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&BASE, 4)),
            Coverage::Truncated
        );
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&[], 5)),
            Coverage::Truncated
        );
    }

    #[test]
    fn misaligned_dialog_diverges() {
        let reworded = [BASE[0], (Role::Assistant, 2, "different")];
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&reworded, 5)),
            Coverage::Diverged
        );
        let other_start = [(Role::User, 0, "earlier"), BASE[0], BASE[1]];
        assert_eq!(
            dialog(&BASE, 5).compare(&dialog(&other_start, 5)),
            Coverage::Diverged
        );
        let more_but_shorter = [BASE[0], BASE[1], (Role::User, 3, "x")];
        assert_eq!(
            dialog(&BASE, 9).compare(&dialog(&more_but_shorter, 4)),
            Coverage::Diverged,
            "more Dialog yet an earlier end does not line up"
        );
    }

    #[test]
    fn nothing_is_covered_by_anything_newer() {
        assert_eq!(dialog(&[], 5).compare(&dialog(&BASE, 5)), Coverage::Covers);
    }
}
