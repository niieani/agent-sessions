use super::*;
use progress::{ItemKind, Progress};

/// Everything a worker needs to generate one Session's Digests.
pub(super) struct Worker<'a> {
    pub counter: &'a dyn super::tokenizer::TokenCounter,
    pub archive: &'a Archive,
    pub client: &'a dyn ModelClient,
    pub config: &'a DigestConfig,
    pub workday: &'a WorkdayConfig,
    pub options: &'a DigestOptions,
    pub progress: &'a Progress,
}

/// Planned Digest items of a Session: its stale Days plus its title.
pub(super) fn planned_items(plan: &SessionPlan) -> usize {
    let days = plan.days.iter().filter(|day| day.stale).count();
    days + usize::from(days > 0 || plan.refresh_session)
}

/// Digest one Session; returns how many of its items were left unprocessed
/// because the model capacity wait ran out.
pub(super) fn process_session(
    worker: &Worker<'_>,
    plan: SessionPlan,
    stats: &mut DigestStats,
) -> usize {
    stats.candidates += plan.days.iter().filter(|day| day.stale).count();
    let written = stats.day_written + stats.session_written;
    let result = process_session_inner(worker, &plan, stats);
    if let Err(error) = &result
        && super::capacity::is_exhausted(error)
    {
        // Not an item failure: the run ends and the next one picks it up.
        let done = stats.day_written + stats.session_written - written;
        return planned_items(&plan).saturating_sub(done);
    }
    worker.progress.session_finished(
        &plan.session.source,
        &plan.session.native_id,
        result.is_ok(),
    );
    if let Err(error) = result {
        stats.failed += 1;
        tracing::warn!(
            source = plan.session.source,
            session = plan.session.native_id,
            error = format!("{error:#}"),
            "Digest failed; continuing"
        );
    }
    0
}

pub(super) fn process_session_inner(
    worker: &Worker<'_>,
    plan: &SessionPlan,
    stats: &mut DigestStats,
) -> Result<()> {
    let Worker {
        archive,
        config,
        workday,
        options,
        progress,
        ..
    } = *worker;
    let dir = files::digest_dir(
        archive.root(),
        &plan.session.source,
        &plan.session.native_id,
    )?;
    let old_session = plan.existing_session.as_ref();
    let day_topic = plan
        .days
        .iter()
        .filter_map(|day| day.existing.as_ref()?.topic.clone())
        .next();
    if let (Some(session), Some(day)) = (old_session, day_topic.as_ref()) {
        anyhow::ensure!(
            session.topic == *day,
            "Session and Day Digest Topics disagree"
        );
    }
    let mut topic = old_session.map(|digest| digest.topic.clone()).or(day_topic);
    let mut prior = Vec::new();
    let mut changed = false;
    let mut previous_path = None;
    let mut all_prior = Vec::new();
    for day in &plan.days {
        if previous_path != day.path_id {
            prior.clear();
            previous_path = day.path_id.clone();
        }
        if !day.stale {
            if let Some(existing) = &day.existing {
                if topic.is_none() {
                    topic = existing.topic.clone();
                }
                prior.push(existing.clone());
                all_prior.push(existing.clone());
            }
            continue;
        }
        let intent = prior.iter().rev().find_map(|digest| digest.intent.clone());
        let opening = match plan.days.first() {
            Some(first) if intent.is_none() && first.workday != day.workday => {
                prompt::opening_turns(&first.messages)
            }
            _ => Vec::new(),
        };
        let mut complete_path = day.shared_context.clone();
        complete_path.extend(day.messages.clone());
        let prepared = prompt::day_request(
            &plan.session,
            &day.workday,
            &complete_path,
            config,
            prompt::DayRequestContext {
                project: plan.project.as_deref(),
                topic: topic.as_deref(),
                intent: intent.as_deref(),
                opening: &opening,
                previous: &prior,
                workday_config: workday,
                max_input_tokens: config.max_input_tokens,
                counter: worker.counter,
            },
        )?;
        stats.record_input_tokens(&plan.session.source, prepared.input_tokens);
        stats.previews.push(DigestPreview {
            source: plan.session.source.clone(),
            native_id: plan.session.native_id.clone(),
            workday: day.workday.clone(),
            input_tokens: prepared.input_tokens,
            truncated: prepared.truncated,
        });
        if options.dry_run {
            continue;
        }
        anyhow::ensure!(
            prepared.input_tokens <= config.max_input_tokens,
            "Digest context exceeds max-input-tokens after reducing the Session Day body"
        );
        let kind = prepared.request.kind;
        let known_topic = topic.clone();
        progress.started(
            &plan.session.source,
            &plan.session.native_id,
            &day.workday,
            ItemKind::Day,
        );
        let (output, facet) = retry::complete_validated(
            worker,
            stats,
            &plan.session.source,
            &plan.session.native_id,
            &day.workday,
            prepared.request,
            |raw| {
                let output = parse_day_output(raw, kind).context("parsing Session Day output")?;
                validate_day_output(&output)?;
                let day_topic = known_topic
                    .as_deref()
                    .or(output.topic.as_deref())
                    .context("first Session Day output did not establish a Topic")?;
                Ok(match title::check_facet(&output.title, day_topic) {
                    title::FacetCheck::Distinct(facet) => {
                        retry::Checked::Valid((output, Some(facet)))
                    }
                    title::FacetCheck::RepeatsTopic { remainder } => retry::Checked::Correctable {
                        error: anyhow::anyhow!(title::repeat_error(&output.title, day_topic)),
                        fallback: (output, remainder),
                    },
                })
            },
        )?;
        let day_topic = if topic.is_none() {
            topic = output.topic.clone();
            output.topic.clone()
        } else {
            day.existing
                .as_ref()
                .and_then(|digest| digest.topic.clone())
        };
        let full_title = title::compose(
            topic
                .as_deref()
                .context("first Session Day output did not establish a Topic")?,
            facet.as_deref(),
        );
        let full_title = plan.project.as_deref().map_or_else(
            || full_title.clone(),
            |project| prefix_cleanup::strip_project_prefix(&full_title, project),
        );
        let locked_title = day
            .existing
            .as_ref()
            .map(|digest| digest.locked_title.clone())
            .unwrap_or_else(|| full_title.clone());
        let digest = DayDigest {
            schema_version: 1,
            source: plan.session.source.clone(),
            native_id: plan.session.native_id.clone(),
            workday: day.workday.clone(),
            dialog_fingerprint: Some(day.dialog_fingerprint.clone()),
            covered: CoveredRange {
                first_idx: day.first_idx,
                last_idx: day.last_idx,
            },
            topic: day_topic,
            title: full_title,
            locked_title,
            intent: output.intent.or(intent),
            tldr: output.tldr,
            summary: output.summary,
            classification: plan
                .pinned_classification
                .clone()
                .unwrap_or(output.classification),
            truncated: prepared.truncated,
            model: config.model.clone(),
            prompt_version: prompt::PROMPT_VERSION,
            prompt_source: prepared.prompt_source,
            digested_at_ms: options.now_ms,
        };
        title_retry::mark(
            archive.root(),
            &plan.session.source,
            &plan.session.native_id,
        )?;
        files::write_json(
            archive.root(),
            &files::path_digest_dir(&dir, day.path_id.as_deref())
                .join(format!("{}.json", day.workday)),
            &digest,
        )?;
        progress.settled(&plan.session.source, &plan.session.native_id, true);
        stats.day_written += 1;
        changed = true;
        all_prior.push(digest.clone());
        prior.push(digest);
    }
    if options.dry_run {
        if let (Some(topic), false) = (topic.as_deref(), all_prior.is_empty()) {
            let (request, _) =
                prompt::session_title_request(topic, &all_prior, config, plan.project.as_deref());
            let tokens = worker.counter.count(&request)?;
            anyhow::ensure!(
                tokens <= config.max_input_tokens,
                "Digest Session title exceeds max-input-tokens"
            );
            stats.record_input_tokens(&plan.session.source, tokens);
        }
        return Ok(());
    }
    if !changed && !plan.refresh_session {
        return Ok(());
    }
    let topic = topic.context("first Session Day output did not establish a Topic")?;
    let (title_request, prompt_source) =
        prompt::session_title_request(&topic, &all_prior, config, plan.project.as_deref());
    stats.record_input_tokens(&plan.session.source, worker.counter.count(&title_request)?);
    let title_workday = prior
        .last()
        .context("changed Session has no Day Digest")?
        .workday
        .clone();
    progress.started(
        &plan.session.source,
        &plan.session.native_id,
        &title_workday,
        ItemKind::Title,
    );
    title_retry::mark(
        archive.root(),
        &plan.session.source,
        &plan.session.native_id,
    )?;
    let title = retry::complete_validated(
        worker,
        stats,
        &plan.session.source,
        &plan.session.native_id,
        &title_workday,
        title_request,
        |raw| {
            let title: SessionTitleOutput =
                serde_json::from_str(raw).context("parsing Session title output")?;
            anyhow::ensure!(
                retry::valid_session_title(&title.title),
                "Session title must be non-empty and at most 80 characters"
            );
            Ok(retry::Checked::Valid(title.title.trim().to_string()))
        },
    )?;
    progress.settled(&plan.session.source, &plan.session.native_id, true);
    let session_digest = SessionDigest {
        schema_version: 1,
        topic,
        title,
        updated_at_ms: options.now_ms,
        model: config.model.clone(),
        prompt_version: prompt::PROMPT_VERSION,
        prompt_source,
        pushed: old_session
            .map(|digest| digest.pushed.clone())
            .unwrap_or_default(),
    };
    files::write_json(archive.root(), &dir.join("session.json"), &session_digest)?;
    title_retry::clear(
        archive.root(),
        &plan.session.source,
        &plan.session.native_id,
    )?;
    stats.session_written += 1;
    Ok(())
}
