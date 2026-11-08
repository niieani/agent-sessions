mod backfill;
mod cache;
mod client;
mod files;
mod fingerprint;
mod omitted;
mod prefix_cleanup;
pub mod preflight;
mod process;
pub mod progress;
mod prompt;
mod repair;
mod retry;
mod schema;
mod selection;
mod support;
mod title;
mod title_retry;
mod tokenizer;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

use anyhow::{Context, Result};
pub use client::{CompletionKind, HttpModelClient, ModelClient, ModelCorrection, ModelRequest};
pub use files::{
    CoveredRange, DayDigest, PromptSource, SessionDigest, digest_dir, path_digest_dir, read_day,
};
pub(crate) use files::{ensure_ready, read_session, write_json};
pub use omitted::list_omitted;
pub use preflight::{PlanSummary, Preflight, preflight};
use process::{Worker, process_session, process_session_inner};
use progress::{PlannedSession, Progress};
pub use prompt::PROMPT_VERSION;
pub use repair::{RepairMode, RepairOptions, RepairStats, repair, repair_with_client};
pub(crate) use schema::CLASSIFICATIONS;
use schema::{SessionTitleOutput, parse_day_output, validate_day_output};
pub use selection::SessionKey;

use crate::archive::Archive;
use crate::archive::db::{Db, SessionRow};
use crate::fleet::{DigestConfig, FleetConfig};
use crate::model::Message;
use crate::workday::{self, WorkdayConfig};

#[derive(Debug, Clone)]
pub struct DigestOptions {
    pub day: Option<String>,
    pub from: Option<String>,
    pub through: Option<String>,
    /// Restrict candidates to one Session.
    pub session: Option<SessionKey>,
    /// Regenerate the selected Session Day of the selected Session even when
    /// its Digest already covers every Message.
    pub force: bool,
    pub dry_run: bool,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestPreview {
    pub source: String,
    pub native_id: String,
    pub workday: String,
    pub input_tokens: usize,
    pub truncated: bool,
}

#[derive(Debug, Default)]
pub struct DigestStats {
    pub candidates: usize,
    pub requests: usize,
    pub day_written: usize,
    pub session_written: usize,
    pub failed: usize,
    pub input_tokens: usize,
    pub(crate) input_tokens_by_source: BTreeMap<String, usize>,
    /// Fingerprint-less Digests adopted as fresh (see `backfill`); a dry-run
    /// counts them without writing.
    pub fingerprints_backfilled: usize,
    pub previews: Vec<DigestPreview>,
    /// The planned work split into new and redone Digests by Source.
    pub plan: PlanSummary,
}

impl DigestStats {
    fn record_input_tokens(&mut self, source: &str, tokens: usize) {
        self.input_tokens += tokens;
        *self
            .input_tokens_by_source
            .entry(source.to_owned())
            .or_default() += tokens;
    }
}

#[derive(Clone)]
struct DayPlan {
    workday: String,
    path_id: Option<String>,
    shared_context: Vec<Message>,
    dialog_fingerprint: String,
    messages: Vec<Message>,
    first_idx: usize,
    last_idx: usize,
    existing: Option<DayDigest>,
    stale: bool,
}

/// Planned Session work plus what was found on the way: fingerprint
/// backfills, and existing Day Digests of selected Workdays by Source.
struct Selection {
    plans: Vec<SessionPlan>,
    backfills: Vec<backfill::Backfill>,
    existing_days: BTreeMap<String, usize>,
}

struct SessionPlan {
    session: SessionRow,
    days: Vec<DayPlan>,
    existing_session: Option<SessionDigest>,
    refresh_session: bool,
    pinned_classification: Option<Vec<String>>,
    project: Option<String>,
}

pub fn run(archive: &Archive, options: &DigestOptions) -> Result<DigestStats> {
    let config = load_config(archive)?;
    if options.dry_run {
        return run_with_client(archive, &support::UnusedClient, options);
    }
    let api_key = std::env::var(&config.api_key_env).with_context(|| {
        format!(
            "missing Digest API key environment variable {}",
            config.api_key_env
        )
    })?;
    let client = HttpModelClient::new(&config, api_key)?;
    run_with_client(archive, &client, options)
}

pub fn run_with_client(
    archive: &Archive,
    client: &dyn ModelClient,
    options: &DigestOptions,
) -> Result<DigestStats> {
    let _writer_lock = if options.dry_run {
        None
    } else {
        Some(archive.acquire_writer_lock()?)
    };
    let fleet = FleetConfig::load(archive.root())?.unwrap_or_default();
    let config = fleet
        .digest
        .as_ref()
        .context("digest command requires [digest] configuration")?;
    let workday = WorkdayConfig::new(&fleet.views.workday)?;
    if let Some(day) = &options.day {
        workday.day_bounds(day)?;
    }
    anyhow::ensure!(
        !options.force || (options.session.is_some() && options.day.is_some()),
        "forcing a Digest requires one Session and one Workday"
    );
    anyhow::ensure!(
        options.day.is_none() || (options.from.is_none() && options.through.is_none()),
        "Digest Workday selection cannot combine day with from/through"
    );
    if let (Some(from), Some(through)) = (&options.from, &options.through) {
        workday.day_bounds(from)?;
        workday.day_bounds(through)?;
        anyhow::ensure!(from <= through, "Digest Workday range is reversed");
    } else {
        anyhow::ensure!(
            options.from.is_none() && options.through.is_none(),
            "Digest Workday range requires both from and through"
        );
    }
    let progress = if options.dry_run {
        Progress::inert()
    } else {
        Progress::begin(archive.root(), "digest", config.progress_interval()?)?
    };
    let result = digest_planned(
        archive, client, config, &fleet, &workday, options, &progress,
    );
    progress.finish(&result)?;
    result
}

fn digest_planned(
    archive: &Archive,
    client: &dyn ModelClient,
    config: &DigestConfig,
    fleet: &FleetConfig,
    workday: &WorkdayConfig,
    options: &DigestOptions,
    progress: &Progress,
) -> Result<DigestStats> {
    let counter = tokenizer::LocalTokenizer::load(config)?;
    let db = if options.dry_run {
        Db::open_read_only(&archive.db_path())?
    } else {
        archive.db()?
    };
    let Selection {
        plans,
        backfills,
        existing_days,
    } = select_plans(archive, &db, fleet, workday, options, false)?;
    if !options.dry_run {
        backfill::persist(archive.root(), &backfills)?;
    }
    let mut plan_summary = PlanSummary::of(&plans, &backfills, &existing_days);
    progress.plan(plans.iter().map(|plan| {
        let days = plan.days.iter().filter(|day| day.stale).count();
        PlannedSession {
            source: &plan.session.source,
            session: &plan.session.native_id,
            days,
            title: days > 0 || plan.refresh_session,
        }
    }));
    let worker = Worker {
        counter: &counter,
        archive,
        client,
        config,
        workday,
        options,
        progress,
    };
    let worker_count = config.concurrency.min(plans.len().max(1));
    let queue = Mutex::new(VecDeque::from(plans));
    let totals = Mutex::new(DigestStats::default());
    progress.while_running(|| {
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                scope.spawn(|| {
                    let mut local = DigestStats::default();
                    loop {
                        let Some(plan) = queue.lock().expect("Digest queue poisoned").pop_front()
                        else {
                            break;
                        };
                        process_session(&worker, plan, &mut local);
                    }
                    support::merge_stats(
                        &mut totals.lock().expect("Digest totals poisoned"),
                        local,
                    );
                });
            }
        })
    });
    let mut stats = totals.into_inner().expect("Digest totals poisoned");
    stats.fingerprints_backfilled = backfills.len();
    plan_summary.record_tokens(&stats.input_tokens_by_source);
    stats.plan = plan_summary;
    stats.previews.sort_by(|left, right| {
        (&left.source, &left.native_id, &left.workday).cmp(&(
            &right.source,
            &right.native_id,
            &right.workday,
        ))
    });
    if !options.dry_run {
        refresh_cache(archive)?;
    }
    Ok(stats)
}

pub fn refresh_cache(archive: &Archive) -> Result<()> {
    let rows = files::cache_rows(archive.root())?;
    let mut db = archive.db()?;
    db.replace_digest_cache(&rows)
}

fn load_config(archive: &Archive) -> Result<DigestConfig> {
    FleetConfig::load(archive.root())?
        .and_then(|config| config.digest)
        .context("digest command requires [digest] configuration")
}

fn select_plans(
    archive: &Archive,
    db: &Db,
    fleet: &FleetConfig,
    workday: &WorkdayConfig,
    options: &DigestOptions,
    repair: bool,
) -> Result<Selection> {
    let sessions = db.sessions_with_dialog()?;
    let mut child_ids = sessions
        .iter()
        .filter(|session| session.subagent_parent.is_some())
        .map(|session| session.id)
        .collect::<BTreeSet<_>>();
    for parent in &sessions {
        child_ids.extend(
            db.subagent_links(&parent.source, &parent.native_id)?
                .into_iter()
                .map(|row| row.child_session_id),
        );
    }
    let projects = crate::render::resolved_projects(archive)?;
    let current_day = workday.workday_of(options.now_ms);
    let mut plans = Vec::new();
    let mut backfills = Vec::new();
    let mut existing_days = BTreeMap::<String, usize>::new();
    let mut selected_found = false;
    for session in sessions {
        if let Some(wanted) = &options.session {
            if session.source != wanted.source || session.native_id != wanted.native_id {
                continue;
            }
            selected_found = true;
            anyhow::ensure!(
                !child_ids.contains(&session.id),
                "{wanted} is a Subagent Session; its Dialog is digested with its parent"
            );
        } else if child_ids.contains(&session.id) {
            continue;
        }
        let messages = db.messages(session.id)?;
        let policy = crate::render::digest_policy(&session, &messages, &fleet.views)?;
        if policy.omitted || !policy.enabled {
            if let Some(wanted) = &options.session {
                anyhow::bail!(
                    "{wanted} is not digested: it is Omitted or its Kind disables Digests"
                );
            }
            continue;
        }
        let branch_paths = crate::model::branch_paths(&db.branches(session.id)?)?;
        let paths = if branch_paths.is_empty() {
            vec![(
                None,
                Vec::new(),
                messages.clone(),
                session.trailing_actions as u32,
            )]
        } else {
            let mut paths = branch_paths
                .into_iter()
                .filter(|p| !p.shared)
                .collect::<Vec<_>>();
            paths.sort_by_key(|p| p.number);
            paths
                .into_iter()
                .map(|p| {
                    (
                        p.path_id,
                        p.ancestors,
                        p.segment.messages,
                        p.segment.trailing_actions,
                    )
                })
                .collect()
        };
        let dir = files::digest_dir(archive.root(), &session.source, &session.native_id)?;
        let existing_session = files::read_session(&dir.join("session.json"))?;
        let mut days = Vec::new();
        for (path_id, shared_context, own, trailing_actions) in paths {
            let inherited = shared_context.len();
            let mut messages = shared_context.clone();
            messages.extend(own);
            let message_days = workday::message_workdays(workday, &messages);
            let mut by_day: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, day) in message_days.into_iter().enumerate() {
                if let Some(day) = day {
                    by_day.entry(day).or_default().push(index);
                }
            }
            let last_day = by_day.keys().next_back().cloned();
            for (day, indices) in by_day {
                if day >= current_day || !indices.iter().any(|index| *index >= inherited) {
                    continue;
                }
                let day_messages = indices
                    .iter()
                    .map(|index| messages[*index].clone())
                    .collect::<Vec<_>>();
                if last_day.as_deref() == Some(day.as_str())
                    && crate::session_day::is_ongoing_for(
                        &session.source,
                        session.captured_at_ms,
                        &day_messages,
                        trailing_actions,
                    )
                {
                    continue;
                }
                let first_idx = *indices.first().expect("day has a Message");
                let last_idx = *indices.last().expect("day has a Message");
                let day_path =
                    files::path_digest_dir(&dir, path_id.as_deref()).join(format!("{day}.json"));
                let mut existing = files::read_day(&day_path)?;
                if let Some(digest) = &existing {
                    files::ensure_day_identity(
                        digest,
                        &session.source,
                        &session.native_id,
                        &day,
                        &day_path,
                    )?;
                }
                let dialog_fingerprint = fingerprint::dialog(&messages[..=last_idx]);
                backfills.extend(existing.as_mut().and_then(|digest| {
                    backfill::adopt(digest, &day_path, first_idx, last_idx, &dialog_fingerprint)
                }));
                let selected = support::day_selected(options, &day);
                if selected && existing.is_some() {
                    *existing_days.entry(session.source.clone()).or_default() += 1;
                }
                let stale = selected
                    && existing.as_ref().is_none_or(|digest| {
                        digest.covered.first_idx != first_idx
                            || digest.covered.last_idx != last_idx
                            || digest.dialog_fingerprint.as_deref() != Some(&dialog_fingerprint)
                    });
                let preceding = messages
                    .iter()
                    .take(inherited)
                    .enumerate()
                    .filter(|(index, _)| !indices.contains(index))
                    .map(|(_, m)| m.clone())
                    .collect();
                days.push(DayPlan {
                    workday: day,
                    path_id: path_id.clone(),
                    shared_context: preceding,
                    dialog_fingerprint,
                    messages: day_messages,
                    first_idx,
                    last_idx,
                    existing,
                    stale,
                });
            }
        }
        if let (true, Some(wanted), Some(day)) = (options.force, &options.session, &options.day) {
            selection::force_day(wanted, &mut days, day)?;
        }
        let fallback_title = days.iter().find_map(|day| {
            let digest = day.existing.as_ref()?;
            digest.topic.as_ref().map(|_| digest.title.as_str())
        });
        let session_in_scope = days
            .iter()
            .any(|day| support::day_selected(options, &day.workday));
        let refresh_session = session_in_scope
            && (title_retry::pending(archive.root(), &session.source, &session.native_id)?
                || match &existing_session {
                    Some(digest) => {
                        !retry::valid_session_title(&digest.title)
                            && fallback_title != Some(digest.title.as_str())
                    }
                    None => days.iter().any(|day| day.existing.is_some()),
                });
        if (repair && !days.is_empty()) || days.iter().any(|day| day.stale) || refresh_session {
            plans.push(SessionPlan {
                project: projects.get(&session.id).cloned().flatten(),
                session,
                days,
                existing_session,
                refresh_session,
                pinned_classification: policy.classification,
            });
        }
    }
    if let Some(wanted) = &options.session {
        anyhow::ensure!(
            selected_found,
            "no Session {wanted} with Dialog in the Archive"
        );
    }
    Ok(Selection {
        plans,
        backfills,
        existing_days,
    })
}

#[cfg(test)]
mod tokenizer_tests;
