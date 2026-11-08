//! Crash-safe publication of a replacement Digest set: `repair.pending`
//! records the complete set before any file changes, so an interrupted
//! publication is finished by the next repair before new work starts.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::archive::Archive;
use crate::digest::files::{self, DayDigest, SessionDigest};
use crate::workday::WorkdayConfig;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::digest) struct Publication {
    pub(in crate::digest) schema_version: u32,
    pub(in crate::digest) fingerprint: String,
    pub(in crate::digest) session: SessionDigest,
    pub(in crate::digest) days: BTreeMap<std::path::PathBuf, DayDigest>,
}

pub(in crate::digest) fn publish(
    archive: &Archive,
    dir: &Path,
    publication: &Publication,
) -> Result<()> {
    files::write_json(archive.root(), &dir.join("repair.pending"), publication)?;
    std::fs::File::open(dir)?.sync_all()?;
    finish_publication(archive, dir, publication)
}
fn finish_publication(archive: &Archive, dir: &Path, p: &Publication) -> Result<()> {
    anyhow::ensure!(
        p.schema_version == 2
            && !p.days.is_empty()
            && p.session.schema_version == 1
            && !p.session.topic.trim().is_empty()
            && !p.session.title.trim().is_empty(),
        "invalid Digest repair publication"
    );
    for (path, day) in &p.days {
        anyhow::ensure!(
            path.components()
                .all(|p| matches!(p, std::path::Component::Normal(_)))
                && path.file_stem().and_then(|s| s.to_str()) == Some(day.workday.as_str()),
            "invalid repair path"
        );
        anyhow::ensure!(
            day.schema_version == 1
                && day.covered.first_idx <= day.covered.last_idx
                && !day.title.trim().is_empty()
                && !day.locked_title.trim().is_empty()
                && !day.summary.trim().is_empty(),
            "invalid Day in repair publication"
        );
        anyhow::ensure!(
            files::digest_dir(archive.root(), &day.source, &day.native_id)? == dir,
            "repair publication identity mismatch"
        );
        WorkdayConfig::new(&Default::default())?.day_bounds(&day.workday)?;
        anyhow::ensure!(
            day.topic
                .as_ref()
                .is_none_or(|topic| topic == &p.session.topic),
            "repair publication Topic mismatch"
        );
    }
    for (path, day) in &p.days {
        files::write_json(archive.root(), &dir.join(path), day)?;
    }
    files::write_json(archive.root(), &dir.join("session.json"), &p.session)?;
    if let Some(day) = p.days.values().next()
        && crate::digest::title_retry::pending(archive.root(), &day.source, &day.native_id)?
    {
        crate::digest::title_retry::clear(archive.root(), &day.source, &day.native_id)?;
    }
    files::write_json(
        archive.root(),
        &dir.join("repair.completed"),
        &p.fingerprint,
    )?;
    std::fs::File::open(dir)?.sync_all()?;
    std::fs::remove_file(dir.join("repair.pending"))?;
    std::fs::File::open(dir)?.sync_all()?;
    match std::fs::remove_file(dir.join("repair.failed")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub(super) fn resume_publications(archive: &Archive, dry_run: bool) -> Result<()> {
    let root = archive.root().join("digests");
    if !root.exists() {
        return Ok(());
    }
    for source in std::fs::read_dir(root)? {
        let source = source?;
        if !source.file_type()?.is_dir() {
            continue;
        }
        for session in std::fs::read_dir(source.path())? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            let path = session.path().join("repair.pending");
            match std::fs::read(&path) {
                Ok(bytes) => {
                    anyhow::ensure!(
                        !dry_run,
                        "pending Digest repair at {}; rerun repair-digests without --dry-run to finish publication",
                        path.display()
                    );
                    let p: Publication = serde_json::from_slice(&bytes)
                        .with_context(|| format!("reading {}", path.display()))?;
                    finish_publication(archive, &session.path(), &p)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
