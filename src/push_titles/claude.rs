use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::PushTitlePlan;
use crate::archive::Archive;

pub(super) enum PendingRecovery {
    Retry,
    Completed,
    Foreign(String),
}

pub(super) fn validate_path(root: &Path, path: &Path, native_id: &str) -> Result<PathBuf> {
    if !path.is_file() {
        return Ok(path.to_path_buf());
    }
    let root = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalizing Claude Store root {}", root.display()))?;
    let path = std::fs::canonicalize(path)
        .with_context(|| format!("canonicalizing Claude Session {}", path.display()))?;
    anyhow::ensure!(
        path.starts_with(&root),
        "Claude Session path is outside its Store"
    );
    anyhow::ensure!(
        path.file_name().and_then(|name| name.to_str()) == Some(&format!("{native_id}.jsonl")),
        "Claude Session path does not match its native id"
    );
    Ok(path)
}

pub(super) fn current_title(path: &Path) -> Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening Claude Session {}", path.display()));
        }
    };
    read_title(BufReader::new(file), path)
}

fn read_title(mut reader: impl BufRead, path: &Path) -> Result<Option<String>> {
    let mut title = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if !line
            .windows(b"custom-title".len())
            .any(|window| window == b"custom-title")
        {
            continue;
        }
        let record = serde_json::from_slice::<serde_json::Value>(&line).with_context(|| {
            format!("parsing possible Claude custom-title in {}", path.display())
        })?;
        if record.get("type").and_then(|value| value.as_str()) == Some("custom-title") {
            let value = record
                .get("customTitle")
                .and_then(|value| value.as_str())
                .context("Claude custom-title record has no string customTitle")?;
            title = Some(value.to_string());
        }
    }
    Ok(title)
}

pub(super) fn preflight(plan: &PushTitlePlan) -> Result<()> {
    let (_, _, temp) = sidecar_paths(plan)?;
    if prepared_sidecar_exists(&temp)? {
        validate_prepared_sidecar(&temp, &plan.title)?;
        anyhow::ensure!(
            plan.pending.is_some(),
            "Claude custom-title sidecar temp exists without a recovery journal"
        );
    }
    Ok(())
}

pub(super) fn apply(archive: &Archive, plan: &PushTitlePlan) -> Result<()> {
    let prepared = prepare_sidecar(plan, &plan.title)?;
    if plan.current.as_deref() == Some(plan.title.as_str()) {
        publish_sidecar(&prepared)?;
        return reconcile_published_sidecar(plan, &plan.title);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&plan.native_path)
        .with_context(|| format!("opening Claude Session {}", plan.native_path.display()))?;
    file.lock()
        .with_context(|| format!("locking Claude Session {}", plan.native_path.display()))?;
    let current = read_title(BufReader::new(&mut file), &plan.native_path)?;
    let allowed_previous = plan
        .pending
        .as_ref()
        .filter(|pending| plan.current.as_deref() == Some(pending.title.as_str()))
        .map(|pending| pending.title.as_str())
        .or(plan.last_pushed.as_deref());
    anyhow::ensure!(
        current == plan.current && (current.is_none() || current.as_deref() == allowed_previous),
        "Claude title changed after planning for {}",
        plan.native_id
    );
    let checked_len = file.seek(SeekFrom::End(0))?;
    anyhow::ensure!(
        file.metadata()?.len() == checked_len,
        "Claude Session changed while planning title write-back"
    );
    super::write_pending_with_claude_boundary(archive, plan, checked_len)?;
    let mut needs_separator = false;
    if checked_len > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            needs_separator = true;
            file.write_all(b"\n")?;
        }
    }
    let record = title_record(&plan.title, &plan.native_id)?;
    file.write_all(&record)?;
    file.sync_data()?;
    let expected_growth = u64::from(needs_separator) + record.len() as u64;
    if file.metadata()?.len() != checked_len + expected_growth {
        resolve_concurrent_append(&mut file, checked_len, &prepared, plan)?;
    }
    publish_sidecar(&prepared)?;
    reconcile_published_sidecar(plan, &plan.title)
}

pub(super) fn finish_pending(plan: &PushTitlePlan, title: &str) -> Result<()> {
    let prepared = prepare_sidecar(plan, title)?;
    publish_sidecar(&prepared)?;
    reconcile_published_sidecar(plan, title)
}

pub(super) fn recover_pending(
    plan: &PushTitlePlan,
    title: &str,
    previous: Option<&str>,
    checked_len: Option<u64>,
) -> Result<PendingRecovery> {
    let Some(checked_len) = checked_len else {
        abandon_pending(plan)?;
        return if plan.current.as_deref() == Some(title) {
            Ok(PendingRecovery::Foreign(title.to_string()))
        } else {
            Ok(PendingRecovery::Retry)
        };
    };
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&plan.native_path)
        .with_context(|| format!("opening Claude Session {}", plan.native_path.display()))?;
    file.lock()
        .with_context(|| format!("locking Claude Session {}", plan.native_path.display()))?;
    anyhow::ensure!(
        file.metadata()?.len() >= checked_len,
        "Claude Session shrank after interrupted title write-back"
    );
    let titles = appended_custom_titles(&mut file, checked_len)?;
    let Some(resolution) = classify_appended_titles(&titles, previous, title) else {
        abandon_pending(plan)?;
        return Ok(PendingRecovery::Retry);
    };
    match resolution {
        AppendResolution::Desired => {
            finish_pending(plan, title)?;
            Ok(PendingRecovery::Completed)
        }
        AppendResolution::ForeignCurrent(foreign) => {
            publish_title_sidecar(plan, foreign)?;
            Ok(PendingRecovery::Foreign(foreign.to_string()))
        }
        AppendResolution::Restore(foreign) => {
            file.write_all(&title_record(foreign, &plan.native_id)?)?;
            file.sync_data()?;
            publish_title_sidecar(plan, foreign)?;
            Ok(PendingRecovery::Foreign(foreign.to_string()))
        }
    }
}

pub(super) fn abandon_pending(plan: &PushTitlePlan) -> Result<()> {
    let (_, _, temp) = sidecar_paths(plan)?;
    match std::fs::remove_file(temp) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn resolve_concurrent_append(
    file: &mut File,
    checked_len: u64,
    prepared: &PreparedSidecar,
    plan: &PushTitlePlan,
) -> Result<()> {
    let titles = appended_custom_titles(file, checked_len)?;
    match classify_appended_titles(&titles, plan.current.as_deref(), &plan.title)
        .context("Claude Session changed concurrently with no complete title record")?
    {
        AppendResolution::Desired => Ok(()),
        AppendResolution::ForeignCurrent(foreign) => {
            rewrite_prepared(prepared, foreign)?;
            publish_sidecar(prepared)?;
            anyhow::bail!("Claude title changed concurrently for {}", plan.native_id);
        }
        AppendResolution::Restore(foreign) => {
            file.write_all(&title_record(foreign, &plan.native_id)?)?;
            file.sync_data()?;
            rewrite_prepared(prepared, foreign)?;
            publish_sidecar(prepared)?;
            anyhow::bail!(
                "Claude title changed concurrently for {}; restored the foreign title",
                plan.native_id
            );
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AppendResolution<'a> {
    Desired,
    ForeignCurrent(&'a str),
    Restore(&'a str),
}

fn classify_appended_titles<'a>(
    titles: &'a [String],
    previous: Option<&str>,
    desired: &str,
) -> Option<AppendResolution<'a>> {
    let last = titles.last()?.as_str();
    if last != desired {
        return Some(AppendResolution::ForeignCurrent(last));
    }
    let preceding = titles
        .len()
        .checked_sub(2)
        .and_then(|index| titles.get(index))
        .map(String::as_str);
    match preceding {
        Some(title) if Some(title) != previous && title != desired => {
            Some(AppendResolution::Restore(title))
        }
        _ => Some(AppendResolution::Desired),
    }
}

fn appended_custom_titles(file: &mut File, checked_len: u64) -> Result<Vec<String>> {
    file.seek(SeekFrom::Start(checked_len))?;
    let mut titles = Vec::new();
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if !line
            .windows(b"custom-title".len())
            .any(|window| window == b"custom-title")
        {
            continue;
        }
        let record: serde_json::Value = serde_json::from_slice(&line)
            .context("parsing concurrently appended Claude custom-title")?;
        if record.get("type").and_then(|value| value.as_str()) == Some("custom-title") {
            if titles.len() == 2 {
                titles.remove(0);
            }
            titles.push(
                record
                    .get("customTitle")
                    .and_then(|value| value.as_str())
                    .context("concurrent Claude custom-title has no string value")?
                    .to_string(),
            );
        }
    }
    Ok(titles)
}

fn title_record(title: &str, native_id: &str) -> Result<Vec<u8>> {
    let mut record = serde_json::to_vec(&ClaudeTitleRecord {
        record_type: "custom-title",
        custom_title: title,
        session_id: native_id,
    })?;
    record.push(b'\n');
    Ok(record)
}

fn sidecar_paths(plan: &PushTitlePlan) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let project = plan
        .native_path
        .parent()
        .context("Claude Session has no project directory")?;
    let dir = project.join(&plan.native_id);
    match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Claude Session sidecar directory is not a real directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let path = dir.join("custom-title.json");
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!("Claude custom-title sidecar is a symlink");
    }
    Ok((
        dir,
        path,
        project
            .join(&plan.native_id)
            .join("custom-title.json.agent-sessions.tmp"),
    ))
}

fn prepare_sidecar(plan: &PushTitlePlan, title: &str) -> Result<PreparedSidecar> {
    let (dir, path, temp) = sidecar_paths(plan)?;
    if !dir.exists() {
        std::fs::create_dir(&dir)?;
    }
    if prepared_sidecar_exists(&temp)? {
        validate_prepared_sidecar(&temp, title)?;
        return Ok(PreparedSidecar { temp, path });
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("creating Claude title sidecar temp {}", temp.display()))?;
    serde_json::to_writer(
        &mut file,
        &ClaudeTitleSidecar {
            custom_title: title,
        },
    )?;
    file.sync_all()?;
    drop(file);
    Ok(PreparedSidecar { temp, path })
}

struct PreparedSidecar {
    temp: PathBuf,
    path: PathBuf,
}

fn publish_sidecar(prepared: &PreparedSidecar) -> Result<()> {
    anyhow::ensure!(
        prepared_sidecar_exists(&prepared.temp)?,
        "Claude custom-title sidecar temp disappeared"
    );
    std::fs::rename(&prepared.temp, &prepared.path).with_context(|| {
        format!(
            "publishing Claude title sidecar {}",
            prepared.path.display()
        )
    })?;
    Ok(())
}

fn publish_title_sidecar(plan: &PushTitlePlan, title: &str) -> Result<()> {
    let (dir, path, temp) = sidecar_paths(plan)?;
    if !dir.exists() {
        std::fs::create_dir(&dir)?;
    }
    let prepared = if prepared_sidecar_exists(&temp)? {
        PreparedSidecar { temp, path }
    } else {
        prepare_sidecar(plan, title)?
    };
    rewrite_prepared(&prepared, title)?;
    publish_sidecar(&prepared)
}

fn reconcile_published_sidecar(plan: &PushTitlePlan, title: &str) -> Result<()> {
    let current = current_title(&plan.native_path)?;
    if current.as_deref() == Some(title) {
        return Ok(());
    }
    let (_, path, _) = sidecar_paths(plan)?;
    if let Some(foreign) = current.as_deref() {
        let prepared = prepare_sidecar(plan, foreign)?;
        publish_sidecar(&prepared)?;
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!(
        "Claude title changed concurrently for {}; reconciled its sidecar",
        plan.native_id
    )
}

fn rewrite_prepared(prepared: &PreparedSidecar, title: &str) -> Result<()> {
    anyhow::ensure!(
        prepared_sidecar_exists(&prepared.temp)?,
        "Claude custom-title sidecar temp disappeared"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&prepared.temp)?;
    serde_json::to_writer(
        &mut file,
        &ClaudeTitleSidecar {
            custom_title: title,
        },
    )?;
    file.sync_all()?;
    Ok(())
}

fn prepared_sidecar_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Claude custom-title sidecar temp is not a regular file"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn validate_prepared_sidecar(path: &Path, title: &str) -> Result<()> {
    let sidecar: ClaudeTitleSidecarOwned = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        sidecar.custom_title == title,
        "Claude title sidecar temp does not match the recovery journal"
    );
    Ok(())
}

#[derive(Serialize)]
struct ClaudeTitleRecord<'a> {
    #[serde(rename = "type")]
    record_type: &'static str,
    #[serde(rename = "customTitle")]
    custom_title: &'a str,
    #[serde(rename = "sessionId")]
    session_id: &'a str,
}

#[derive(Serialize)]
struct ClaudeTitleSidecar<'a> {
    #[serde(rename = "customTitle")]
    custom_title: &'a str,
}

#[derive(Deserialize)]
struct ClaudeTitleSidecarOwned {
    #[serde(rename = "customTitle")]
    custom_title: String,
}

#[cfg(test)]
mod tests {
    use super::{AppendResolution, classify_appended_titles};

    fn titles(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn appended_titles_preserve_the_latest_concurrent_foreign_choice() {
        assert_eq!(
            classify_appended_titles(&titles(&["human", "ours"]), None, "ours"),
            Some(AppendResolution::Restore("human"))
        );
        assert_eq!(
            classify_appended_titles(&titles(&["ours", "human"]), None, "ours"),
            Some(AppendResolution::ForeignCurrent("human"))
        );
        assert_eq!(
            classify_appended_titles(&titles(&["old", "ours"]), Some("old"), "ours"),
            Some(AppendResolution::Desired)
        );
        assert_eq!(
            classify_appended_titles(&titles(&["unrelated", "old", "ours"]), Some("old"), "ours"),
            Some(AppendResolution::Desired)
        );
        assert_eq!(classify_appended_titles(&[], None, "ours"), None);
    }
}
