use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::archive::Archive;
use crate::digest::{DayDigest, read_day, write_json};
use crate::fleet::FleetConfig;
use crate::render::{LockedTitleOverrides, TranscriptPlan, plan_transcript_paths};
use crate::workday::WorkdayConfig;

#[derive(Debug, Clone, Default)]
pub struct RetitleOptions {
    pub native_id: Option<String>,
    pub day: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rename {
    pub source: String,
    pub native_id: String,
    pub workday: Option<String>,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct RetitleResult {
    pub relocked: usize,
    pub renames: Vec<Rename>,
}

struct Candidate {
    path: PathBuf,
    digest: DayDigest,
}

pub fn run(archive: &Archive, options: &RetitleOptions) -> Result<RetitleResult> {
    anyhow::ensure!(
        options.day.is_none() || options.native_id.is_some(),
        "retitle --day requires a native id"
    );
    if let Some(day) = &options.day {
        let config = FleetConfig::load(archive.root())?.unwrap_or_default();
        WorkdayConfig::new(&config.views.workday)?.day_bounds(day)?;
    }
    let _writer_lock = if options.dry_run {
        None
    } else {
        Some(archive.acquire_writer_lock()?)
    };
    let candidates = select_candidates(archive.root(), options)?;
    let mut overrides = LockedTitleOverrides::new();
    for candidate in &candidates {
        overrides.insert(
            (
                candidate.digest.source.clone(),
                candidate.digest.native_id.clone(),
                candidate.digest.workday.clone(),
            ),
            candidate.digest.title.clone(),
        );
    }
    let before = plan_transcript_paths(archive, &LockedTitleOverrides::new())?;
    let after = plan_transcript_paths(archive, &overrides)?;
    let renames = changed_paths(&before, &after)?;

    if !options.dry_run {
        for mut candidate in candidates {
            candidate.digest.locked_title = candidate.digest.title.clone();
            write_json(archive.root(), &candidate.path, &candidate.digest)?;
        }
        crate::digest::refresh_cache(archive)?;
    }

    Ok(RetitleResult {
        relocked: overrides.len(),
        renames,
    })
}

fn select_candidates(root: &Path, options: &RetitleOptions) -> Result<Vec<Candidate>> {
    let digest_root = root.join("digests");
    if !digest_root.exists() {
        anyhow::ensure!(options.native_id.is_none(), "no Digest matches native id");
        return Ok(Vec::new());
    }
    let mut selected_digest_seen = false;
    let mut matched_sources = BTreeSet::new();
    let mut candidates = Vec::new();
    for source in sorted_directories(&digest_root)? {
        let source_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .context("Digest Source directory is not UTF-8")?;
        for session in sorted_directories(&source)? {
            let session_component = session
                .file_name()
                .and_then(|value| value.to_str())
                .context("Digest Session directory is not UTF-8")?;
            let mut paths = std::fs::read_dir(&session)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.sort();
            for path in paths {
                if path.file_name().and_then(|value| value.to_str()) == Some("session.json")
                    || path.extension().and_then(|value| value.to_str()) != Some("json")
                {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(&path)?;
                anyhow::ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Digest is not a regular file: {}",
                    path.display()
                );
                let digest = read_day(&path)?.expect("enumerated Digest file");
                anyhow::ensure!(
                    digest.source == source_name
                        && crate::path_key::identity_component(&digest.native_id)
                            == session_component,
                    "Digest identity does not match {}",
                    path.display()
                );
                if options.native_id.as_ref() == Some(&digest.native_id) {
                    matched_sources.insert(digest.source.clone());
                }
                if options
                    .native_id
                    .as_ref()
                    .is_some_and(|wanted| wanted != &digest.native_id)
                    || options
                        .day
                        .as_ref()
                        .is_some_and(|wanted| wanted != &digest.workday)
                {
                    continue;
                }
                selected_digest_seen = true;
                if digest.title != digest.locked_title {
                    candidates.push(Candidate { path, digest });
                }
            }
        }
    }
    anyhow::ensure!(
        matched_sources.len() <= 1,
        "native id {:?} is ambiguous across Sources: {}",
        options.native_id.as_deref().unwrap_or_default(),
        matched_sources.into_iter().collect::<Vec<_>>().join(", ")
    );
    anyhow::ensure!(
        options.native_id.is_none() || selected_digest_seen,
        "no Digest matches native id{}",
        options
            .native_id
            .as_deref()
            .map(|id| format!(" {id:?}"))
            .unwrap_or_default()
    );
    Ok(candidates)
}

fn sorted_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(
            !kind.is_symlink(),
            "Digest directory entry is a symlink: {}",
            entry.path().display()
        );
        if kind.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

fn changed_paths(before: &TranscriptPlan, after: &TranscriptPlan) -> Result<Vec<Rename>> {
    anyhow::ensure!(
        before.output == after.output,
        "View output changed while planning"
    );
    let output = before.output.clone();
    let before_paths = before
        .transcripts
        .iter()
        .map(|unit| {
            (
                (
                    unit.source.clone(),
                    unit.native_id.clone(),
                    unit.workday.clone(),
                ),
                unit.path.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut renames = Vec::new();
    for unit in &after.transcripts {
        let key = (
            unit.source.clone(),
            unit.native_id.clone(),
            unit.workday.clone(),
        );
        let Some(old_path) = before_paths.get(&key) else {
            continue;
        };
        if old_path != &unit.path {
            renames.push(Rename {
                source: unit.source.clone(),
                native_id: unit.native_id.clone(),
                workday: unit.workday.clone(),
                old_path: output.join(old_path),
                new_path: output.join(&unit.path),
            });
        }
    }
    renames.sort_by(|left, right| {
        (&left.source, &left.native_id, &left.workday).cmp(&(
            &right.source,
            &right.native_id,
            &right.workday,
        ))
    });
    Ok(renames)
}
