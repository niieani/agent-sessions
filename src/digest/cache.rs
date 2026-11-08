use super::files::{read_day, read_session};
use crate::archive::db::DigestCacheRow;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
pub fn cache_rows(archive_root: &Path) -> Result<Vec<DigestCacheRow>> {
    let root = archive_root.join("digests");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for source_entry in sorted_dirs(&root)? {
        let source = source_entry
            .file_name()
            .and_then(|value| value.to_str())
            .context("Digest Source directory is not UTF-8")?
            .to_string();
        for session_entry in sorted_dirs(&source_entry)? {
            let component = session_entry
                .file_name()
                .and_then(|value| value.to_str())
                .context("Digest Session directory is not UTF-8")?
                .to_string();
            let mut native_id: Option<String> = None;
            let mut session_row = None;
            let mut digest_dirs = vec![session_entry.clone()];
            let branches = session_entry.join("branches");
            if branches.exists() {
                digest_dirs.extend(sorted_dirs(&branches)?);
            }
            for digest_dir in digest_dirs {
                for entry in std::fs::read_dir(&digest_dir)? {
                    let entry = entry?;
                    if !entry.file_type()?.is_file()
                        || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
                    {
                        continue;
                    }
                    let path = entry.path();
                    let filename = entry.file_name();
                    let filename = filename.to_string_lossy();
                    let relative = path
                        .strip_prefix(archive_root)
                        .expect("Digest is beneath Archive")
                        .to_string_lossy()
                        .into_owned();
                    if filename == "session.json" {
                        let digest = read_session(&path)?.expect("enumerated Session Digest");
                        session_row = Some((digest, relative));
                    } else {
                        let digest = read_day(&path)?.expect("enumerated Day Digest");
                        anyhow::ensure!(
                            digest.source == source
                                && crate::path_key::identity_component(&digest.native_id)
                                    == component,
                            "Digest identity does not match {}",
                            path.display()
                        );
                        if let Some(existing) = &native_id {
                            anyhow::ensure!(
                                existing == &digest.native_id,
                                "mixed Digest identities in {}",
                                session_entry.display()
                            );
                        } else {
                            native_id = Some(digest.native_id.clone());
                        }
                        rows.push(DigestCacheRow {
                            source: source.clone(),
                            native_id: digest.native_id,
                            workday: digest.workday,
                            last_idx: Some(digest.covered.last_idx),
                            title: digest.title,
                            path: relative,
                        });
                    }
                }
            }
            if let Some((digest, path)) = session_row {
                rows.push(DigestCacheRow {
                    source: source.clone(),
                    native_id: native_id
                        .context("Session Digest has no accompanying Session Day Digest")?,
                    workday: String::new(),
                    last_idx: None,
                    title: digest.title,
                    path,
                });
            }
        }
    }
    rows.sort_by(|left, right| {
        (&left.source, &left.native_id, &left.path).cmp(&(
            &right.source,
            &right.native_id,
            &right.path,
        ))
    });
    Ok(rows)
}

fn sorted_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}
