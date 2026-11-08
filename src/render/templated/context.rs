use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Unit, UnitKey};
use crate::fleet::ViewConfig;
use crate::model::Augmentation;
use crate::render::relative_href;
use crate::timefmt;
use crate::workday::WorkdayConfig;

pub(super) fn template_context(
    unit: &Unit,
    units: &[Unit],
    config: &ViewConfig,
    workday_config: &WorkdayConfig,
    augmentations: &[Augmentation],
    body: String,
    targets: &BTreeMap<UnitKey, PathBuf>,
) -> Value {
    let host = crate::render::host::display(&unit.session);
    let source_name = config
        .source_names
        .get(&unit.session.source)
        .cloned()
        .unwrap_or_else(|| unit.session.source.clone());
    let host_name = config
        .host_names
        .get(&host)
        .cloned()
        .unwrap_or_else(|| host.clone());
    let project_name = display_project(unit, config);
    let source_name_safe = wikilink_safe(&source_name);
    let host_name_safe = wikilink_safe(&host_name);
    let project_name_safe = project_name.as_deref().map(wikilink_safe);
    let project_dir = unit
        .group
        .relative_dir
        .strip_prefix("projects")
        .unwrap_or(&unit.group.relative_dir)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string();
    let facts = &unit.session.transcript_facts;
    let git = facts.git.as_ref();
    let mut session_json = json!({
        "id": unit.session.id,
        "source": unit.session.source,
        "source_name": source_name,
        "source_name_safe": source_name_safe,
        "host": host,
        "host_attribution": unit.session.host_attribution,
        "capture_host": unit.session.winner_host,
        "copy_hosts": unit.session.host_attribution.as_ref().map(|a| a.copy_hosts(&unit.session.hosts)).unwrap_or_default(),
        "host_name": host_name,
        "host_name_safe": host_name_safe,
        "native_id": unit.session.native_id,
        "project": {
            "name": project_name,
            "name_safe": project_name_safe,
            "dir": project_dir,
            "path": unit.session.project_path.as_deref().unwrap_or_default(),
            "unresolved": unit.session.repository_resolution.as_ref().is_some_and(|f| f.unresolved),
        },
        "native_title": unit.session.title,
        "session_title": unit.session_digest.as_ref().and_then(|value| value.get("title")),
        "topic": unit.session_digest.as_ref().and_then(|value| value.get("topic")),
        "starting_model": facts.starting_model,
        "recovered_via": facts.recovered_via,
        "starting_reasoning_effort": facts.starting_reasoning_effort,
        "git": {
            "branch": git.and_then(|git| git.branch.as_deref()),
            "commit": git.and_then(|git| git.commit.as_deref()),
            "repository_url": git.and_then(|git| git.repository_url.as_deref()),
        },
        "patched_files": facts.patched_files,
        "referenced_files": facts.referenced_files,
        "message_count": unit.branch.as_ref().map_or(unit.all_messages.len(),|b|b.message_count),
        "created_at": unit.session.created_at_ms.map(timefmt::frontmatter).unwrap_or_default(),
        "updated_at": unit.session.updated_at_ms.map(timefmt::frontmatter).unwrap_or_default(),
        "activity_days": workday_days(unit),
        "orchestrators": augmentations,
        "subagent": subagent_context(unit, units, targets),
        "inherited_context": unit.session.inherited_context.as_ref().map(|context| json!({
            "source_id": context.source_native_id,
            "copied_item_count": context.copied_item_count,
        })),
    });
    if !unit.day_unit
        && let Some(digest) = unit
            .digest
            .as_ref()
            .filter(|_| unit.branch.as_ref().is_none_or(|b| !b.shared))
    {
        session_json["digest"] = digest_context(digest);
    }
    if let Some(project) = &unit.session.native_project {
        session_json["project"]["native"] =
            serde_json::to_value(project).expect("NativeProject serializes");
    }
    let day = unit.day_unit.then(|| {
        let workday = unit.workday.as_ref().expect("Session Day has a Workday");
        let adjacent = adjacent_days(unit, units, targets);
        json!({
            "workday": workday,
            "index": workday_days(unit).iter().position(|day| day == workday).map(|index| index + 1),
            "first": workday_days(unit).first().and_then(|day| reference(unit, &(unit.session.id, unit.branch.as_ref().map(|b|b.id.clone()), Some(day.clone())), units, targets)),
            "created_at": unit.created_at_ms.map(|timestamp| workday_config.format_timestamp(timestamp)).unwrap_or_default(),
            "message_count": unit.messages.len(),
            "ongoing": unit.ongoing,
            "first_idx": unit.indices.first(),
            "last_idx": unit.indices.last(),
            "digest": unit.digest.as_ref().filter(|_|unit.branch.as_ref().is_none_or(|b|!b.shared)).map(digest_context),
            "previous": adjacent.0,
            "next": adjacent.1,
        })
    });
    let messages = unit
        .messages
        .iter()
        .zip(&unit.indices)
        .map(|(message, idx)| {
            json!({
                "idx": idx,
                "role": message.role.as_str(),
                "kind": message.kind.as_str(),
                "text": message.text,
                "timestamp": message.timestamp_ms.map(timefmt::frontmatter),
                "workday": unit.all_message_days[*idx],
            })
        })
        .collect::<Vec<_>>();
    let mut context =
        json!({ "session": session_json, "day": day, "body": body, "messages": messages });
    if let Some(branch) = &unit.branch {
        context["branch"] = json!({"id":branch.id,"parent_id":branch.parent_id,"selected":branch.selected,"shared":branch.shared,"number":branch.number,"path_id":branch.path_id,"continues":branch.parent_id.as_ref().and_then(|id| units.iter().filter(|u|u.session.id==unit.session.id && u.branch.as_ref().is_some_and(|b| &b.id==id) && u.relative_path.is_some()).max_by(|a,b| a.workday.cmp(&b.workday))).and_then(|parent|reference(unit,&parent.key(),units,targets))});
    }
    context
}

fn adjacent_days(
    unit: &Unit,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
) -> (Option<Value>, Option<Value>) {
    let Some(current_day) = unit.workday.as_deref() else {
        return (None, None);
    };
    let mut days = targets
        .iter()
        .filter(|((session_id, branch, day), _)| {
            *session_id == unit.session.id
                && *branch == unit.branch.as_ref().map(|b| b.id.clone())
                && day.is_some()
        })
        .map(|((_, _, day), path)| (day.as_deref().unwrap(), path))
        .collect::<Vec<_>>();
    days.sort_by_key(|(day, _)| *day);
    let Some(index) = days.iter().position(|(day, _)| *day == current_day) else {
        return (None, None);
    };
    let current_path = unit.relative_path.as_ref().expect("planned unit path");
    let make = |(day, path): &(&str, &PathBuf)| {
        let href = relative_href(current_path.parent().unwrap_or_else(|| Path::new("")), path);
        json!({
            "path": href,
            "workday": day,
            "title": units.iter().find(|candidate| candidate.session.id == unit.session.id && candidate.workday.as_deref() == Some(*day)).and_then(Unit::locked_title),
            "stem": path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default(),
        })
    };
    (
        index.checked_sub(1).map(|idx| make(&days[idx])),
        days.get(index + 1).map(make),
    )
}

pub(super) fn display_project(unit: &Unit, _config: &ViewConfig) -> Option<String> {
    unit.group.project_name.clone()
}

fn workday_days(unit: &Unit) -> Vec<String> {
    let mut days = unit
        .all_message_days
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if days.is_empty() && unit.branch.is_some() {
        days.extend(unit.workday.clone());
    }
    days.sort();
    days.dedup();
    days
}

fn digest_context(digest: &Value) -> Value {
    json!({
        "title": digest.get("title"),
        "locked_title": digest.get("lockedTitle"),
        "intent": digest.get("intent"),
        "tldr": digest.get("tldr"),
        "summary": digest.get("summary"),
        "classification": digest.get("classification").cloned().unwrap_or_else(|| json!([])),
        "truncated": digest.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "model": digest.get("model"),
    })
}

fn wikilink_safe(value: &str) -> String {
    value
        .replace("[[", "")
        .replace("]]", "")
        .replace(['|', '#'], "")
}

fn reference(
    unit: &Unit,
    key: &UnitKey,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
) -> Option<Value> {
    let path = targets.get(key)?;
    let current = unit.relative_path.as_ref()?;
    Some(json!({
        "path": relative_href(current.parent().unwrap_or_else(|| Path::new("")), path),
        "workday": key.2,
        "stem": path.file_stem()?.to_str()?,
        "title": units.iter().find(|candidate| candidate.key() == *key).and_then(Unit::locked_title),
    }))
}

fn subagent_context(
    unit: &Unit,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
) -> Option<Value> {
    let fact = unit.session.subagent_parent.as_ref();
    let link = unit.parent_link.as_ref();
    let parent_id = link
        .map(|link| link.parent_native_id.as_str())
        .or_else(|| fact.map(|fact| fact.parent_native_id.as_str()))?;
    let parent = unit
        .parent_id
        .and_then(|id| units.iter().find(|candidate| candidate.session.id == id));
    let spawn_day = parent.and_then(|parent| {
        unit.parent_spawned_at_ms.and_then(|spawned| {
            parent
                .all_messages
                .iter()
                .zip(&parent.all_message_days)
                .filter(|(message, _)| message.timestamp_ms.is_some_and(|at| at <= spawned))
                .filter_map(|(_, day)| day.as_ref())
                .next_back()
        })
    });
    let parent_day = parent.zip(spawn_day).and_then(|(parent, day)| {
        reference(
            unit,
            &(
                parent.session.id,
                parent.branch.as_ref().map(|b| b.id.clone()),
                Some(day.clone()),
            ),
            units,
            targets,
        )
    });
    Some(json!({
        "parent_id": parent_id,
        "parent_source": parent.map(|parent| parent.session.source.as_str()).unwrap_or(&unit.session.source),
        "name": link.and_then(|link| link.description.as_ref()).or_else(|| fact.and_then(|fact| fact.description.as_ref())),
        "agent_type": link.and_then(|link| link.agent_type.as_ref()).or_else(|| fact.and_then(|fact| fact.agent_type.as_ref())),
        "depth": link.and_then(|link| link.depth).or_else(|| fact.and_then(|fact| fact.depth)),
        "spawn_day_known": spawn_day.is_some(),
        "parent_day": parent_day,
    }))
}

/// Resolve only a Session Day present in this render's collision-resolved plan.
pub(super) fn add_lead_context(
    context: &mut Value,
    lead: Option<&crate::leads::SessionLead>,
    unit: &Unit,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
) -> anyhow::Result<()> {
    context["session"]["led_by"] = match lead {
        Some(lead) => {
            let mut by = serde_json::to_value(&lead.led_by)?;
            if let Some(origin) = &lead.origin {
                by["origin"] = serde_json::to_value(origin)?;
            }
            by
        }
        None => Value::Null,
    };
    context["session"]["requested_by"] = Value::Null;
    let Some(lead) = lead else {
        return Ok(());
    };
    let Some((source, native_id)) = lead.requested_by()? else {
        return Ok(());
    };
    if source == lead.source && native_id == lead.session_id {
        return Ok(());
    }
    let Some(day) = unit.workday.as_deref().filter(|_| unit.day_unit) else {
        return Ok(());
    };
    let requester = units
        .iter()
        .filter(|candidate| {
            candidate.day_unit
                && candidate.session.source == source
                && candidate.session.native_id == native_id
                && candidate
                    .workday
                    .as_deref()
                    .is_some_and(|requester_day| requester_day <= day)
                && targets.contains_key(&candidate.key())
        })
        .max_by(|left, right| left.workday.cmp(&right.workday));
    if let Some(mut note) =
        requester.and_then(|requester| reference(unit, &requester.key(), units, targets))
    {
        note["source"] = json!(source);
        note["native_id"] = json!(native_id);
        context["session"]["requested_by"] = note;
    }
    Ok(())
}
