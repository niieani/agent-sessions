//! Fingerprint backfill: Digests written before Dialog fingerprints existed
//! stay fresh when their covered range still matches the Session Day.
//!
//! Releases before fingerprints treated a Digest as fresh while its covered
//! range reached the day's last Message. Adopting the current fingerprint for
//! an exactly matching range keeps that freshness without a model call; any
//! later Dialog change then invalidates it through the fingerprint.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::files::{DayDigest, write_json};

/// A fingerprint-less Digest adopted in memory, awaiting persistence.
pub(super) struct Backfill {
    path: PathBuf,
    digest: DayDigest,
}

/// Adopt `fingerprint` into `digest` when it has none and covers exactly
/// `first_idx..=last_idx`. Returns the pending write for an adopted Digest.
pub(super) fn adopt(
    digest: &mut DayDigest,
    path: &Path,
    first_idx: usize,
    last_idx: usize,
    fingerprint: &str,
) -> Option<Backfill> {
    let matches = digest.dialog_fingerprint.is_none()
        && digest.covered.first_idx == first_idx
        && digest.covered.last_idx == last_idx;
    matches.then(|| {
        digest.dialog_fingerprint = Some(fingerprint.to_string());
        Backfill {
            path: path.to_path_buf(),
            digest: digest.clone(),
        }
    })
}

impl Backfill {
    pub(super) fn source(&self) -> &str {
        &self.digest.source
    }
}

/// Write every adopted fingerprint. Callers hold the Archive writer lock.
pub(super) fn persist(archive_root: &Path, backfills: &[Backfill]) -> Result<()> {
    for backfill in backfills {
        write_json(archive_root, &backfill.path, &backfill.digest)?;
    }
    Ok(())
}
