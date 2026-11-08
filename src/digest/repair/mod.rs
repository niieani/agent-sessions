//! Explicit replacement of a complete Session's Digests, with resumable publication.
use super::*;
use sha2::{Digest as _, Sha256};
use std::path::Path;

mod publication;
use publication::resume_publications;
pub(super) use publication::{Publication, publish};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMode {
    Loose,
    ProjectPrefixes,
}
#[derive(Debug, Clone)]
pub struct RepairOptions {
    pub mode: RepairMode,
    pub dry_run: bool,
    pub force: bool,
    pub now_ms: i64,
}
#[derive(Debug, Default)]
pub struct RepairStats {
    pub selected: usize,
    pub repaired: usize,
    pub skipped: usize,
    pub failed: usize,
    pub requests: usize,
    pub input_tokens: usize,
    pub errors: Vec<String>,
}

pub fn repair(archive: &Archive, options: &RepairOptions) -> Result<RepairStats> {
    if options.dry_run || options.mode == RepairMode::ProjectPrefixes {
        return repair_with_client(archive, &support::UnusedClient, options);
    }
    let config = load_config(archive)?;
    let key = std::env::var(&config.api_key_env).with_context(|| {
        format!(
            "missing Digest API key environment variable {}",
            config.api_key_env
        )
    })?;
    repair_with_client(archive, &HttpModelClient::new(&config, key)?, options)
}

pub fn repair_with_client(
    archive: &Archive,
    client: &dyn ModelClient,
    options: &RepairOptions,
) -> Result<RepairStats> {
    let _lock = if options.dry_run {
        None
    } else {
        Some(archive.acquire_writer_lock()?)
    };
    // Complete publication before any reader sees a mixed Topic generation.
    resume_publications(archive, options.dry_run)?;
    if options.mode == RepairMode::ProjectPrefixes {
        return super::prefix_cleanup::run(archive, options);
    }
    let fleet = FleetConfig::load(archive.root())?.unwrap_or_default();
    let config = fleet
        .digest
        .as_ref()
        .context("Digest repair requires [digest] configuration")?;
    let workday = WorkdayConfig::new(&fleet.views.workday)?;
    let progress = if options.dry_run {
        Progress::inert()
    } else {
        Progress::begin(
            archive.root(),
            "repair-digests",
            config.progress_interval()?,
        )?
    };
    let result = repair_planned(
        archive, client, config, &fleet, &workday, options, &progress,
    );
    progress.finish(&result)?;
    result
}

fn repair_planned(
    archive: &Archive,
    client: &dyn ModelClient,
    config: &DigestConfig,
    fleet: &FleetConfig,
    workday: &WorkdayConfig,
    options: &RepairOptions,
    progress: &Progress,
) -> Result<RepairStats> {
    let counter = tokenizer::LocalTokenizer::load(config)?;
    let mut stats = RepairStats::default();
    let db = Db::open_read_only(&archive.db_path())?;
    let digest_options = DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: options.dry_run,
        now_ms: options.now_ms,
    };
    let Selection {
        plans, backfills, ..
    } = select_plans(archive, &db, fleet, workday, &digest_options, true)?;
    if !options.dry_run {
        backfill::persist(archive.root(), &backfills)?;
    }
    let projects = crate::render::resolved_projects(archive)?;
    let plans = plans
        .into_iter()
        .filter(|p| projects.get(&p.session.id).is_some_and(Option::is_none))
        .collect::<VecDeque<_>>();
    // Repair regenerates every settled Session Day and the Session title.
    progress.plan(plans.iter().map(|plan| PlannedSession {
        source: &plan.session.source,
        session: &plan.session.native_id,
        days: plan.days.len(),
        title: true,
    }));
    let worker = Worker {
        archive,
        client,
        config,
        workday,
        options: &digest_options,
        progress,
        counter: &counter,
    };
    let workers = config.concurrency.min(plans.len().max(1));
    let queue = Mutex::new(plans);
    let results = Mutex::new(Vec::new());
    progress.while_running(|| {
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let Some(plan) = queue.lock().expect("repair queue poisoned").pop_front()
                        else {
                            break;
                        };
                        let (source, native_id) =
                            (plan.session.source.clone(), plan.session.native_id.clone());
                        let result = process_plan(&worker, options, plan);
                        let ok = result.as_ref().is_ok_and(|stats| stats.failed == 0);
                        progress.session_finished(&source, &native_id, ok);
                        results
                            .lock()
                            .expect("repair results poisoned")
                            .push(result);
                    }
                });
            }
        })
    });
    for result in results.into_inner().expect("repair results poisoned") {
        let mut result = result?;
        stats.selected += result.selected;
        stats.repaired += result.repaired;
        stats.skipped += result.skipped;
        stats.failed += result.failed;
        stats.requests += result.requests;
        stats.input_tokens += result.input_tokens;
        stats.errors.append(&mut result.errors);
    }
    stats.errors.sort();
    if !options.dry_run {
        refresh_cache(archive)?;
    }
    Ok(stats)
}

fn process_plan(
    worker: &Worker<'_>,
    options: &RepairOptions,
    mut plan: SessionPlan,
) -> Result<RepairStats> {
    let (archive, config) = (worker.archive, worker.config);
    let mut stats = RepairStats::default();
    stats.selected += 1;
    let dir = files::digest_dir(
        archive.root(),
        &plan.session.source,
        &plan.session.native_id,
    )?;
    let fingerprint = fingerprint(&plan, config)?;
    if !options.force && completed(&dir, &fingerprint)? {
        stats.skipped += 1;
        return Ok(stats);
    }
    tracing::info!(
        source = plan.session.source,
        session = plan.session.native_id,
        days = plan.days.len(),
        "repairing Session Digests"
    );
    let pushed = plan
        .existing_session
        .as_ref()
        .map(|s| s.pushed.clone())
        .unwrap_or_default();
    plan.existing_session = None;
    plan.refresh_session = true;
    for day in &mut plan.days {
        day.existing = None;
        day.stale = true;
    }
    let result = (|| -> Result<()> {
        ensure_complete_days(&dir, &plan)?;
        if options.dry_run {
            let mut generated = DigestStats::default();
            process_session_inner(worker, &plan, &mut generated)?;
            stats.input_tokens += generated.input_tokens;
            return Ok(());
        }
        let stage = tempfile::Builder::new()
            .prefix("asessions-digest-repair-")
            .tempdir()?;
        let staged = Archive::open(stage.path())?;
        let mut generated = DigestStats::default();
        let staged_worker = Worker {
            archive: &staged,
            ..*worker
        };
        let result = process_session_inner(&staged_worker, &plan, &mut generated);
        stats.requests += generated.requests;
        stats.input_tokens += generated.input_tokens;
        preserve_rejections(archive, &staged, &plan)?;
        result?;
        anyhow::ensure!(generated.failed == 0, "Session title regeneration failed");
        let staged_dir =
            files::digest_dir(stage.path(), &plan.session.source, &plan.session.native_id)?;
        let mut session = files::read_session(&staged_dir.join("session.json"))?
            .context("repair did not generate Session Digest")?;
        session.pushed = pushed;
        let days = plan
            .days
            .iter()
            .map(|d| {
                let path = files::path_digest_dir(Path::new(""), d.path_id.as_deref())
                    .join(format!("{}.json", d.workday));
                Ok((
                    path.clone(),
                    files::read_day(&staged_dir.join(&path))?
                        .context("repair did not generate Day Digest")?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        publish(
            archive,
            &dir,
            &Publication {
                schema_version: 2,
                fingerprint,
                session,
                days,
            },
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            if !options.dry_run {
                stats.repaired += 1;
                tracing::info!(
                    source = plan.session.source,
                    session = plan.session.native_id,
                    requests = stats.requests,
                    "Session Digest repair completed"
                );
            }
        }
        Err(error) => {
            stats.failed += 1;
            let error = format!(
                "{}/{}: {error:#}",
                plan.session.source, plan.session.native_id
            );
            tracing::warn!(%error,"Digest repair failed; old Digests preserved unless publication remains pending");
            if !options.dry_run {
                files::write_json(
                    archive.root(),
                    &dir.join("repair.failed"),
                    &serde_json::json!({"error":error,"attempted_at_ms":options.now_ms}),
                )?;
            }
            stats.errors.push(error);
        }
    }
    Ok(stats)
}

fn fingerprint(plan: &SessionPlan, config: &DigestConfig) -> Result<String> {
    let bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"prompt_version":PROMPT_VERSION,"project":plan.project,"model":config.model,"day_prompt":config.prompts.day,"session_prompt":config.prompts.session_title,"max_input_tokens":config.max_input_tokens,"extra_body":config.extra_body,"days":plan.days.iter().map(|d|serde_json::json!({"workday":d.workday,"path_id":d.path_id,"shared_context":d.shared_context,"messages":d.messages})).collect::<Vec<_>>()}),
    )?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn completed(dir: &Path, fingerprint: &str) -> Result<bool> {
    match std::fs::read(dir.join("repair.completed")) {
        Ok(bytes) => Ok(serde_json::from_slice::<String>(&bytes)? == fingerprint),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn ensure_complete_days(dir: &Path, plan: &SessionPlan) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let days = plan
        .days
        .iter()
        .map(|d| d.workday.as_str())
        .collect::<BTreeSet<_>>();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json")
            && path.file_name().and_then(|s| s.to_str()) != Some("session.json")
        {
            anyhow::ensure!(
                days.contains(
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                ),
                "cannot repair a partial Session: {} has no settled Dialog in the repair selection",
                path.display()
            );
        }
    }
    Ok(())
}
fn preserve_rejections(archive: &Archive, stage: &Archive, plan: &SessionPlan) -> Result<()> {
    use std::io::BufRead as _;
    let dir = files::digest_dir(stage.root(), &plan.session.source, &plan.session.native_id)?;
    let target = files::digest_dir(
        archive.root(),
        &plan.session.source,
        &plan.session.native_id,
    )?;
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .ends_with(".rejected.jsonl")
        {
            continue;
        }
        for line in std::io::BufReader::new(std::fs::File::open(entry.path())?).lines() {
            let line = line?;
            let record: serde_json::Value = serde_json::from_str(&line)?;
            files::append_rejected(
                archive.root(),
                &target.join(entry.file_name()),
                &files::RejectedDigest {
                    attempted_at_ms: record["attemptedAtMs"]
                        .as_i64()
                        .context("invalid rejection timestamp")?,
                    kind: record["kind"].as_str().context("invalid rejection kind")?,
                    attempt: record["attempt"]
                        .as_u64()
                        .context("invalid rejection attempt")?
                        as usize,
                    error: record["error"]
                        .as_str()
                        .context("invalid rejection error")?,
                    raw: record["raw"].as_str().context("invalid rejected output")?,
                },
            )?;
        }
    }
    Ok(())
}
