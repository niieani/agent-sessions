use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{
    LockedTitleOverrides, RenderOptions, RenderStats, RenderedSubagentLink, TranscriptPlan,
    ViewGrouping, relative_href, remove_view_symlinks, render_body_with_links,
    sweep_orphaned_views, write_if_changed,
};
use crate::archive::Archive;
use crate::archive::db::{Db, SessionRow};
use crate::fleet::FleetConfig;
use crate::timefmt;
use crate::workday::WorkdayConfig;

type UnitKey = (i64, Option<String>, Option<String>);

mod branches;
mod collisions;
mod context;
mod layout;
mod normalization;
mod output;
mod plan;
mod planning;
mod units;
use context::{display_project, template_context};
use layout::{Layouts, Templates};
use output::{output_root, preflight_output, reconcile_topology, validate_manifest};
use planning::{assign_paths, json_paths, layout_context, parent_unit_key};
use units::{BuildInputs, Unit, build_units};

pub(super) fn render_all_with_options(
    archive: &Archive,
    options: RenderOptions,
) -> Result<RenderStats> {
    let _writer_lock = archive.acquire_writer_lock()?;
    let db = Db::open_read_only(&archive.db_path())?;
    let sessions = db.sessions_with_dialog()?;
    let messages_by_id = sessions
        .iter()
        .map(|session| Ok((session.id, db.messages(session.id)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let workday = WorkdayConfig::new(&config.views.workday)?;
    let assignments = crate::assignments::load(archive.root())?;
    let leads = db.session_leads()?;
    let grouping = ViewGrouping::new(&sessions, &messages_by_id, &config.views, &db, &assignments)?;
    let layouts = Layouts::load(&config.views)?;
    let templates = Templates::load(&config.views)?;
    let output = output_root(archive, &config.views)?;

    let mut stats = RenderStats {
        format: "asessions-render-v1",
        claims: grouping.claim_stats(),
        ..Default::default()
    };
    let parent_links = parent_links(&sessions, &db)?;
    let mut units = build_units(
        &sessions,
        &messages_by_id,
        &BuildInputs {
            archive,
            config: &config.views,
            workday: &workday,
            grouping: &grouping,
            parent_links: &parent_links,
            overrides: &LockedTitleOverrides::new(),
        },
    )?;
    assign_paths(&mut units, &layouts, &mut stats)?;
    stats.unresolved_repositories = units
        .iter()
        .filter(|unit| {
            unit.relative_path.is_some()
                && unit.group.loose
                && unit
                    .session
                    .repository_resolution
                    .as_ref()
                    .is_some_and(|f| f.unresolved)
        })
        .map(|unit| unit.session.id)
        .collect::<BTreeSet<_>>()
        .len();
    let targets = units
        .iter()
        .filter_map(|unit| unit.relative_path.clone().map(|path| (unit.key(), path)))
        .collect::<BTreeMap<_, _>>();

    let assets = super::assets::Assets::collect(
        archive,
        &sessions,
        &units
            .iter()
            .filter(|unit| unit.relative_path.is_some())
            .map(|unit| unit.session.id)
            .collect(),
    )?;
    let mut writes = BTreeMap::new();
    let mut semantic_inputs = BTreeMap::new();
    let mut index_inputs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut expected = BTreeSet::new();
    let json_paths = layouts
        .json()
        .map(|layout| json_paths(&units, layout))
        .transpose()?
        .unwrap_or_default();
    let mut indexes: BTreeMap<PathBuf, (String, Vec<Value>)> = BTreeMap::new();
    for unit in &units {
        let Some(relative_path) = unit.relative_path.as_ref() else {
            continue;
        };
        let semantic = super::changes::semantic(
            &json!({"source":unit.session.source,"session":unit.session.native_id,"workday":unit.workday,"dialog":super::changes::dialog_input(&unit.all_messages),"digest":super::changes::digest_input(unit.digest.as_ref().filter(|_|unit.branch.as_ref().is_none_or(|b|!b.shared))),"session_digest":super::changes::digest_input(unit.session_digest.as_ref()),"media":assets.media_identity(unit.session.id)}),
        );
        semantic_inputs.insert(relative_path.clone(), (semantic.clone(), "dialog".into()));
        let mut prepared = unit.clone();
        prepared.messages = assets.messages(unit.session.id, relative_path, &unit.messages);
        let footnotes = super::references::Footnotes::prepare(
            assets.generated.get(&unit.session.id),
            relative_path,
            &mut prepared.messages,
        );
        let unit = &prepared;
        let links = rendered_links(unit, &units, &targets, &db)?;
        let augmentations = db.augmentations_for_session(unit.session.id)?;
        let mut body_session = unit.session.clone();
        if !unit.owns_trailing_actions {
            body_session.trailing_actions = 0;
        }
        let body = render_body_with_links(&body_session, &unit.messages, &links, options, &workday);
        let body = branches::navigation(unit, &units, &targets, body)?;
        let mut context = template_context(
            unit,
            &units,
            &config.views,
            &workday,
            &augmentations,
            body,
            &targets,
        );
        if !footnotes.is_empty() {
            context["source_footnotes"] = footnotes.context();
        }
        let lead = leads.get(&(unit.session.source.clone(), unit.session.native_id.clone()));
        context::add_lead_context(&mut context, lead, unit, &units, &targets)?;
        let content = templates.transcript(&context).with_context(|| {
            format!(
                "rendering {}/{}{}",
                unit.session.source,
                unit.session.native_id,
                unit.workday
                    .as_ref()
                    .map(|day| format!("/{day}"))
                    .unwrap_or_default()
            )
        })?;
        let content = footnotes.append(content);
        tracing::debug!(path = %relative_path.display(), "View unit planned");
        anyhow::ensure!(
            writes.insert(relative_path.clone(), content).is_none(),
            "multiple View units planned {}",
            relative_path.display()
        );
        expected.insert(relative_path.clone());
        stats.transcripts += 1;

        if let Some(json_path) = json_paths.get(&unit.key()).cloned() {
            let json = format!("{}\n", serde_json::to_string_pretty(&context)?);
            anyhow::ensure!(
                writes.insert(json_path.clone(), json).is_none(),
                "JSON View conflicts with another View {}",
                json_path.display()
            );
            semantic_inputs.insert(json_path.clone(), (semantic.clone(), "json".into()));
            expected.insert(json_path);
            stats.json_views += 1;
        }

        if !unit.child
            && unit
                .branch
                .as_ref()
                .is_none_or(|branch| branch.parent_id.is_none())
            && let Some(index_layout) = layouts.index(unit)
        {
            let flat = layout_context(unit, None, None);
            let index_path = index_layout.render(&flat)?;
            index_inputs
                .entry(index_path.clone())
                .or_default()
                .push(semantic.clone());
            let index_title = display_project(unit, &config.views)
                .unwrap_or_else(|| unit.group.index_title.clone());
            let href = relative_href(
                index_path.parent().unwrap_or_else(|| Path::new("")),
                relative_path,
            );
            indexes
                .entry(index_path)
                .or_insert_with(|| (index_title, Vec::new()))
                .1
                .push(json!({
                    "created_at_ms": unit.created_at_ms,
                    "date": unit.created_at_ms.map(timefmt::date_only).unwrap_or_default(),
                    "title": unit.locked_title().or(unit.session.title.as_deref()).unwrap_or_default(),
                    "path": href,
                    "filename": relative_path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                }));
        }
    }

    for (path, (title, mut entries)) in indexes {
        entries.sort_by(|left, right| {
            right["created_at_ms"]
                .as_i64()
                .cmp(&left["created_at_ms"].as_i64())
                .then_with(|| left["filename"].as_str().cmp(&right["filename"].as_str()))
        });
        let context = json!({ "title": title, "entries": entries });
        anyhow::ensure!(
            writes
                .insert(path.clone(), templates.index(&context)?)
                .is_none(),
            "View index conflicts with Transcript {}",
            path.display()
        );
        let mut inputs = index_inputs.remove(&path).unwrap_or_default();
        inputs.sort();
        semantic_inputs.insert(
            path.clone(),
            (super::changes::semantic(&json!(inputs)), "index".into()),
        );
        expected.insert(path);
        stats.indexes += 1;
    }

    for (path, input) in &assets.semantic_inputs {
        semantic_inputs.insert(path.clone(), (input.clone(), "asset".into()));
    }
    // Every config, layout, path, and per-unit template has rendered before
    // the first disposable View mutation.
    expected.extend(assets.writes.keys().cloned());
    expected.extend(assets.media_paths.iter().cloned());
    stats.preparation_errors.extend(assets.errors.clone());
    validate_manifest(writes.keys().chain(assets.writes.keys()))?;
    preflight_output(archive, &config, &output)?;
    let comparison = super::changes::compare(
        &output,
        &writes,
        &assets.writes,
        &expected,
        &semantic_inputs,
        &db.render_file_states(&output)?,
    )?;
    let renames = normalization::plan_renames(&output, writes.keys().chain(assets.writes.keys()))?;
    std::fs::create_dir_all(&output)?;
    stats.removed += remove_view_symlinks(
        &output,
        &assets
            .media_paths
            .iter()
            .map(|path| output.join(path))
            .collect(),
    )?;
    normalization::apply_renames(renames)?;
    stats.removed += reconcile_topology(&output, writes.keys().chain(assets.writes.keys()))?;
    for (relative, content) in writes {
        let path = output.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        stats.changed += usize::from(write_if_changed(&path, &content)?);
    }
    for (relative, bytes) in assets.writes {
        let path = output.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !path.is_file() || std::fs::read(&path)? != bytes {
            std::fs::write(&path, bytes)?;
            stats.changed += 1;
        }
    }
    stats.removed += sweep_orphaned_views(&output, &output, &expected)?;
    drop(db);
    archive
        .db()?
        .replace_render_file_states(&output, &comparison.states)?;
    stats.files = comparison.files;
    Ok(stats)
}

pub(super) fn plan_transcript_paths(
    archive: &Archive,
    overrides: &LockedTitleOverrides,
) -> Result<TranscriptPlan> {
    plan::transcript_paths(archive, overrides)
}

fn parent_links(
    sessions: &[SessionRow],
    db: &Db,
) -> Result<BTreeMap<i64, (i64, crate::model::SubagentLink)>> {
    let mut result = BTreeMap::new();
    for parent in sessions {
        for row in db.subagent_links(&parent.source, &parent.native_id)? {
            result.insert(row.child_session_id, (parent.id, row.link));
        }
    }
    Ok(result)
}

fn rendered_links(
    unit: &Unit,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
    db: &Db,
) -> Result<Vec<RenderedSubagentLink>> {
    let Some(own_path) = unit.relative_path.as_ref() else {
        return Ok(Vec::new());
    };
    let mut links = Vec::new();
    for row in db.subagent_links(&unit.session.source, &unit.session.native_id)? {
        let mut candidates = units
            .iter()
            .filter(|child| child.session.id == row.child_session_id)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.workday.cmp(&right.workday));
        let child = candidates.first().copied();
        let Some(child) = child else {
            continue;
        };
        if parent_unit_key(child, units).as_ref() != Some(&unit.key()) {
            continue;
        }
        let Some(target) = targets.get(&child.key()) else {
            continue;
        };
        links.push(RenderedSubagentLink {
            link: row.link,
            title: child.session.title.clone(),
            href: relative_href(own_path.parent().unwrap_or_else(|| Path::new("")), target),
        });
    }
    Ok(links)
}

pub(super) fn materialize_media(
    archive: &Archive,
    method: crate::media::MaterializationMethod,
) -> Result<usize> {
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let output = output::output_root(archive, &config.views)?;
    output::preflight_output(archive, &config, &output)?;
    crate::media::materialize(archive.root(), &output, method)
}
