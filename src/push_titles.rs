//! Explicit native title write-back. No other workflow may write a Store.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::adapters::sqlite::{open_read_only, table_columns};
use crate::archive::Archive;
use crate::archive::db::Db;
use crate::digest::{SessionDigest, digest_dir};
use crate::fleet::{FleetConfig, HostConfig};
use crate::pull::StoreRoots;

mod claude;
mod t3;
mod t3_transport;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TitleConfig {
    #[serde(default)]
    pub overwrite_external: bool,
}

const WRITABLE_SOURCES: [&str; 3] = [
    crate::adapters::claude::SOURCE,
    crate::adapters::codex::SOURCE,
    "t3",
];

#[derive(Debug, Clone, Default)]
pub struct PushTitleOptions {
    pub source: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTitleDisposition {
    Push,
    Unchanged,
    Keep,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTitlePlan {
    pub source: String,
    pub native_id: String,
    pub current: Option<String>,
    pub title: String,
    pub disposition: PushTitleDisposition,
    native_path: PathBuf,
    last_pushed: Option<String>,
    digest_path: PathBuf,
    digest: SessionDigest,
    pending_path: PathBuf,
    pending: Option<PendingPush>,
}

#[derive(Debug, Default)]
pub struct PushTitleResult {
    pub plans: Vec<PushTitlePlan>,
    /// Planned pushes in dry-run mode; successfully applied pushes otherwise.
    pub pushed: usize,
    pub unchanged: usize,
    pub kept: usize,
    pub missing: usize,
    pub source_errors: Vec<(String, String)>,
}

pub fn run(archive: &Archive, options: &PushTitleOptions) -> Result<PushTitleResult> {
    validate_source(options.source.as_deref())?;
    let (host, roots) = local_host(archive.root())?;
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let _archive_lock = if options.dry_run {
        None
    } else {
        Some(archive.acquire_writer_lock()?)
    };
    let db = Db::open_read_only(&archive.db_path())?;
    let mut source_errors = Vec::new();
    let plans = build_plan(
        archive,
        &db,
        &host,
        &roots,
        options.source.as_deref(),
        &config.titles,
        &mut source_errors,
    )?;
    let mut result = summarize(plans);
    result.source_errors = source_errors;
    if !options.dry_run {
        apply_plan(archive, &roots, &result.plans)?;
    }
    Ok(result)
}

fn validate_source(source: Option<&str>) -> Result<()> {
    if let Some(source) = source {
        anyhow::ensure!(
            WRITABLE_SOURCES.contains(&source),
            "push-titles supports claude, codex and t3; {source:?} has no writable route"
        );
    }
    Ok(())
}

fn local_host(archive_root: &Path) -> Result<(String, StoreRoots)> {
    let config = FleetConfig::load(archive_root)?.unwrap_or_default();
    let hosts = if config.hosts.is_empty() {
        vec![HostConfig::localhost(
            crate::pull::local_hostname(),
            None,
            None,
            None,
            None,
            None,
            None,
        )]
    } else {
        config.hosts
    };
    let mut local = hosts.into_iter().filter(|host| host.ssh.is_none());
    let host = local
        .next()
        .context("push-titles requires one local [[host]]")?;
    anyhow::ensure!(
        local.next().is_none(),
        "push-titles requires exactly one local [[host]]"
    );
    let roots = host.store_roots();
    Ok((host.name, roots))
}

fn build_plan(
    archive: &Archive,
    db: &Db,
    host: &str,
    roots: &StoreRoots,
    source_filter: Option<&str>,
    titles: &indexmap::IndexMap<String, TitleConfig>,
    source_errors: &mut Vec<(String, String)>,
) -> Result<Vec<PushTitlePlan>> {
    let codex = source_filter.is_none_or(|source| source == crate::adapters::codex::SOURCE)
        && roots.codex.join("state_5.sqlite").is_file();
    let codex = codex
        .then(|| open_codex_read_only(&roots.codex))
        .transpose()?;
    let mut plans = Vec::new();
    for session in db.sessions_with_dialog()? {
        if !WRITABLE_SOURCES.contains(&session.source.as_str())
            || source_filter.is_some_and(|source| source != session.source)
        {
            continue;
        }
        let Some(native_path) = db.session_native_path(session.id, host)? else {
            continue;
        };
        let digest_path =
            digest_dir(archive.root(), &session.source, &session.native_id)?.join("session.json");
        let Some(digest) = crate::digest::read_session(&digest_path)? else {
            continue;
        };
        let last_pushed = digest.pushed.get(&session.source).cloned();
        let pending_path = digest_path.with_file_name("push-title.pending");
        let pending = read_pending(&pending_path)?.filter(|pending| pending.source != "t3");
        if let Some(pending) = &pending {
            anyhow::ensure!(
                pending.source == session.source && pending.native_id == session.native_id,
                "Title Write-back journal identity does not match {}",
                pending_path.display()
            );
        }
        let mut native_path = PathBuf::from(native_path);
        let (current, store_present) = match session.source.as_str() {
            crate::adapters::codex::SOURCE => match &codex {
                Some(connection) => match codex_title(connection, &session.native_id)? {
                    Some(current) => (current, true),
                    None => (None, false),
                },
                None => (None, false),
            },
            crate::adapters::claude::SOURCE => {
                native_path =
                    claude::validate_path(&roots.claude, &native_path, &session.native_id)?;
                (claude::current_title(&native_path)?, native_path.is_file())
            }
            _ => unreachable!("writable Source was checked"),
        };
        let recovering_current = pending
            .as_ref()
            .is_some_and(|pending| current.as_deref() == Some(pending.title.as_str()));
        let disposition = if !store_present {
            PushTitleDisposition::Missing
        } else if current.as_deref() == Some(digest.title.as_str())
            && last_pushed.as_deref() == current.as_deref()
        {
            PushTitleDisposition::Unchanged
        } else if recovering_current
            || (session.source == "codex"
                && titles
                    .get("codex")
                    .is_some_and(|config| config.overwrite_external))
            || current.is_none()
            || current.as_deref() == last_pushed.as_deref()
        {
            PushTitleDisposition::Push
        } else {
            PushTitleDisposition::Keep
        };
        plans.push(PushTitlePlan {
            source: session.source,
            native_id: session.native_id,
            current,
            title: digest.title.clone(),
            disposition,
            native_path,
            last_pushed,
            digest_path,
            digest,
            pending_path,
            pending,
        });
    }
    if source_filter.is_none_or(|source| source == "t3") {
        match t3::build_plan(archive, db, host, roots, titles) {
            Ok(t3_plans) => plans.extend(t3_plans),
            Err(error) if source_filter.is_none() => {
                source_errors.push(("t3".into(), format!("{error:#}")))
            }
            Err(error) => return Err(error),
        }
    }
    plans.sort_by(|left, right| {
        (&left.source, &left.native_id).cmp(&(&right.source, &right.native_id))
    });
    Ok(plans)
}

fn summarize(plans: Vec<PushTitlePlan>) -> PushTitleResult {
    let mut result = PushTitleResult {
        plans,
        ..Default::default()
    };
    for plan in &result.plans {
        match plan.disposition {
            PushTitleDisposition::Push => result.pushed += 1,
            PushTitleDisposition::Unchanged => result.unchanged += 1,
            PushTitleDisposition::Keep => result.kept += 1,
            PushTitleDisposition::Missing => result.missing += 1,
        }
    }
    result
}

fn open_codex_read_only(root: &Path) -> Result<Connection> {
    let path = root.join("state_5.sqlite");
    let connection = open_read_only(&path, "Codex")?;
    validate_codex_catalog(&connection, &path)?;
    Ok(connection)
}

fn open_codex_read_write(root: &Path) -> Result<Connection> {
    let path = root.join("state_5.sqlite");
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "opening Codex Store for title write-back: {}",
            path.display()
        )
    })?;
    connection.busy_timeout(Duration::from_secs(5))?;
    validate_codex_catalog(&connection, &path)?;
    Ok(connection)
}

fn validate_codex_catalog(connection: &Connection, path: &Path) -> Result<()> {
    let columns = table_columns(connection, "threads")?;
    anyhow::ensure!(
        columns.iter().any(|column| column == "id")
            && columns.iter().any(|column| column == "name"),
        "Codex Store {} has no threads(id,name) catalog",
        path.display()
    );
    Ok(())
}

fn codex_title(connection: &Connection, native_id: &str) -> Result<Option<Option<String>>> {
    connection
        .query_row(
            "SELECT name FROM threads WHERE id = ?1",
            [native_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn apply_plan(archive: &Archive, roots: &StoreRoots, plans: &[PushTitlePlan]) -> Result<()> {
    let needs_codex = plans.iter().any(|plan| {
        plan.source == crate::adapters::codex::SOURCE
            && plan.disposition == PushTitleDisposition::Push
    });
    let codex = needs_codex
        .then(|| open_codex_read_write(&roots.codex))
        .transpose()?;
    let mut ordered = plans.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|plan| plan.pending.is_none());
    let mut failures = Vec::new();
    for plan in ordered {
        let outcome = (|| -> Result<()> {
            if recover_or_abandon_pending(archive, plan)? {
                return Ok(());
            }
            if matches!(
                plan.disposition,
                PushTitleDisposition::Keep | PushTitleDisposition::Unchanged
            ) {
                return Ok(());
            }
            if plan.disposition != PushTitleDisposition::Push {
                return Ok(());
            }
            preflight_plan(plan)?;
            write_pending(archive, plan)?;
            match plan.source.as_str() {
                crate::adapters::codex::SOURCE => {
                    if plan.current.as_deref() != Some(plan.title.as_str()) {
                        let changed = codex
                            .as_ref()
                            .expect("Codex connection was opened")
                            .execute(
                                "UPDATE threads SET name = ?1 WHERE id = ?2 AND name IS ?3",
                                params![plan.title, plan.native_id, plan.current],
                            )?;
                        anyhow::ensure!(
                            changed == 1,
                            "Codex title changed after planning for {}",
                            plan.native_id
                        );
                    }
                }
                crate::adapters::claude::SOURCE => claude::apply(archive, plan)?,
                "t3" => t3::apply(roots, plan)?,
                _ => unreachable!("writable Source was checked"),
            }
            record_push(archive, plan)?;
            remove_pending(&plan.pending_path)?;
            Ok(())
        })();
        if let Err(error) = outcome {
            failures.push(format!("{} {}: {error:#}", plan.source, plan.native_id));
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "Title Write-back failed:\n{}",
        failures.join("\n")
    );
    Ok(())
}

fn preflight_plan(plan: &PushTitlePlan) -> Result<()> {
    if plan.source == crate::adapters::claude::SOURCE {
        claude::preflight(plan)?;
    }
    Ok(())
}

fn recover_or_abandon_pending(archive: &Archive, plan: &PushTitlePlan) -> Result<bool> {
    let Some(pending) = &plan.pending else {
        return Ok(false);
    };
    if plan.source == crate::adapters::claude::SOURCE {
        let outcome = claude::recover_pending(
            plan,
            &pending.title,
            pending.previous.as_deref(),
            pending.claude_checked_len,
        )?;
        match outcome {
            claude::PendingRecovery::Retry => {
                remove_pending(&plan.pending_path)?;
                return Ok(false);
            }
            claude::PendingRecovery::Completed => {
                record_push_title(archive, plan, &pending.title)?;
                remove_pending(&plan.pending_path)?;
                return Ok(plan.title == pending.title);
            }
            claude::PendingRecovery::Foreign(title) => {
                remove_pending(&plan.pending_path)?;
                anyhow::bail!(
                    "Claude title changed during interrupted write-back for {}; restored the foreign title {:?}",
                    plan.native_id,
                    title
                );
            }
        }
    }
    if plan.current.as_deref() == Some(pending.title.as_str()) {
        record_push_title(archive, plan, &pending.title)?;
        remove_pending(&plan.pending_path)?;
        return Ok(plan.title == pending.title);
    }
    if plan.source == crate::adapters::claude::SOURCE {
        claude::abandon_pending(plan)?;
    }
    remove_pending(&plan.pending_path)?;
    Ok(false)
}

fn record_push(archive: &Archive, plan: &PushTitlePlan) -> Result<()> {
    record_push_title(archive, plan, &plan.title)
}

fn record_push_title(archive: &Archive, plan: &PushTitlePlan, title: &str) -> Result<()> {
    let mut digest = crate::digest::read_session(&plan.digest_path)?
        .context("Session Digest disappeared during Title Write-back")?;
    let key = if plan.source == "t3" {
        format!("t3:{}", plan.native_id)
    } else {
        plan.source.clone()
    };
    digest.pushed.insert(key, title.to_string());
    crate::digest::write_json(archive.root(), &plan.digest_path, &digest)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingPush {
    schema_version: u32,
    source: String,
    native_id: String,
    previous: Option<String>,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_checked_len: Option<u64>,
}

fn read_pending(path: &Path) -> Result<Option<PendingPush>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let pending: PendingPush = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing Title Write-back journal {}", path.display()))?;
            anyhow::ensure!(
                pending.schema_version == 1,
                "unsupported Title Write-back journal"
            );
            Ok(Some(pending))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_pending(archive: &Archive, plan: &PushTitlePlan) -> Result<()> {
    anyhow::ensure!(
        read_pending(&plan.pending_path)?.is_none(),
        "another interrupted Title Write-back must recover before {}",
        plan.native_id
    );
    let pending = PendingPush {
        schema_version: 1,
        source: plan.source.clone(),
        native_id: plan.native_id.clone(),
        previous: plan.current.clone(),
        title: plan.title.clone(),
        claude_checked_len: None,
    };
    crate::digest::write_json(archive.root(), &plan.pending_path, &pending)
}

fn write_pending_with_claude_boundary(
    archive: &Archive,
    plan: &PushTitlePlan,
    checked_len: u64,
) -> Result<()> {
    let pending = PendingPush {
        schema_version: 1,
        source: plan.source.clone(),
        native_id: plan.native_id.clone(),
        previous: plan.current.clone(),
        title: plan.title.clone(),
        claude_checked_len: Some(checked_len),
    };
    crate::digest::write_json(archive.root(), &plan.pending_path, &pending)
}

fn remove_pending(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
