//! Settle one bundled native archival unit against the Archive: the same
//! supersede rules as local capture ([`crate::adapters::revision_check`],
//! then Dialog coverage for a changed append-oriented body).

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

use super::metadata::{merge_capture_metadata, t3_attachment_body_is_richer};
use crate::adapters::revision_check_for_adapter;
use crate::adapters::{
    CaptureMeta, CaptureOutcome, RevisionCheck, commit_staged, tmp_sibling, write_capture_meta,
};

/// Settle `body` (the unit's tar entry, consumed either way) and return what
/// happened to the Capture at `body_path`/`meta_path`. A retained body still
/// adopts newer sidecar metadata when `metadata_update` says so.
pub(super) fn settle_unit<R: Read>(
    body: &mut R,
    existing: Option<&CaptureMeta>,
    incoming: &CaptureMeta,
    body_path: &Path,
    meta_path: &Path,
    metadata_update: bool,
) -> Result<CaptureOutcome> {
    if let Some(adapter) = crate::adapters::adapter_for(&incoming.source)
        && let Some(outcome) = adapter.settle_bundle(
            body,
            incoming,
            body_path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .context("Capture hierarchy missing")?,
        )?
    {
        return Ok(outcome);
    }
    let check = existing.map(|existing| {
        match revision_check_for_adapter(
            existing,
            incoming.source_mtime_ms,
            incoming.source_size,
            incoming.adapter_version,
        ) {
            RevisionCheck::Unchanged
                if existing.source_files != incoming.source_files
                    || t3_attachment_body_is_richer(incoming, existing) =>
            {
                RevisionCheck::Changed
            }
            check => check,
        }
    });
    let retained = |outcome: CaptureOutcome| -> Result<CaptureOutcome> {
        if metadata_update {
            let existing = existing.expect("metadata updates need a Capture");
            write_capture_meta(meta_path, &merge_capture_metadata(existing, incoming))?;
        }
        Ok(match outcome {
            CaptureOutcome::Unchanged if metadata_update => CaptureOutcome::Extended,
            outcome => outcome,
        })
    };
    match check {
        Some(RevisionCheck::Unchanged) => {
            drain(body)?;
            retained(CaptureOutcome::Unchanged)
        }
        Some(RevisionCheck::Stale) => {
            tracing::warn!(
                capture = %body_path.display(),
                incoming_revision = incoming.source_mtime_ms,
                "bundled payload is an older logical revision; keeping the Capture"
            );
            drain(body)?;
            retained(CaptureOutcome::Refused)
        }
        None | Some(RevisionCheck::Changed) => {
            // Same commit order as local capture: body first (temp +
            // rename), sidecar second — a sidecar always describes the body
            // next to it.
            let staged = tmp_sibling(body_path);
            let mut out = std::io::BufWriter::new(
                std::fs::File::create(&staged)
                    .with_context(|| format!("creating capture {}", staged.display()))?,
            );
            std::io::copy(body, &mut out)?;
            out.flush()?;
            drop(out);
            match commit_staged(existing, &staged, body_path, meta_path, incoming.clone())? {
                Some(capture) => Ok(capture.outcome),
                None => {
                    tracing::warn!(
                        capture = %body_path.display(),
                        "bundled payload lost Dialog its Capture holds; keeping the Capture"
                    );
                    retained(CaptureOutcome::Refused)
                }
            }
        }
    }
}

fn drain<R: Read>(reader: &mut R) -> Result<()> {
    std::io::copy(reader, &mut std::io::sink())?;
    Ok(())
}
