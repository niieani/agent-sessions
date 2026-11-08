//! Session/Session Day unit construction and durable Digest inputs.
use super::UnitKey;
use crate::archive::{Archive, db::SessionRow};
use crate::fleet::{ViewConfig, ViewUnit};
use crate::model::{Message, Role};
use crate::render::{LockedTitleOverrides, ViewGroup, ViewGrouping, transcript_filename};
use crate::session_day;
use crate::workday::{self, WorkdayConfig};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) struct BuildInputs<'a> {
    pub(super) archive: &'a Archive,
    pub(super) config: &'a ViewConfig,
    pub(super) workday: &'a WorkdayConfig,
    pub(super) grouping: &'a ViewGrouping,
    pub(super) parent_links: &'a BTreeMap<i64, (i64, crate::model::SubagentLink)>,
    pub(super) overrides: &'a LockedTitleOverrides,
}

#[derive(Clone)]
pub(super) struct Unit {
    pub(super) session: SessionRow,
    pub(super) branch: Option<super::branches::BranchContext>,
    pub(super) all_messages: Vec<Message>,
    pub(super) all_message_days: Vec<Option<String>>,
    pub(super) messages: Vec<Message>,
    pub(super) indices: Vec<usize>,
    pub(super) day_unit: bool,
    pub(super) workday: Option<String>,
    pub(super) created_at_ms: Option<i64>,
    pub(super) ongoing: bool,
    pub(super) digest: Option<Value>,
    pub(super) naming_title: Option<String>,
    pub(super) session_digest: Option<Value>,
    pub(super) group: ViewGroup,
    pub(super) child: bool,
    pub(super) parent_id: Option<i64>,
    pub(super) parent_spawned_at_ms: Option<i64>,
    pub(super) parent_link: Option<crate::model::SubagentLink>,
    pub(super) owns_trailing_actions: bool,
    pub(super) default_stem: String,
    pub(super) relative_path: Option<PathBuf>,
}

impl Unit {
    pub(super) fn key(&self) -> UnitKey {
        (
            self.session.id,
            self.branch.as_ref().map(|branch| branch.id.clone()),
            self.day_unit.then_some(self.workday.clone()).flatten(),
        )
    }

    pub(super) fn locked_title(&self) -> Option<&str> {
        self.naming_title.as_deref().or_else(|| {
            self.digest
                .as_ref()
                .and_then(|digest| digest.get("lockedTitle"))
                .and_then(Value::as_str)
        })
    }
}

pub(super) fn build_linear_units(
    sessions: &[SessionRow],
    messages_by_id: &BTreeMap<i64, Vec<Message>>,
    inputs: &BuildInputs<'_>,
) -> Result<Vec<Unit>> {
    let BuildInputs {
        archive,
        config,
        workday,
        grouping,
        parent_links,
        overrides,
    } = inputs;
    let mut units = Vec::new();
    for session in sessions {
        let all_messages = &messages_by_id[&session.id];
        let Some(default_filename) = transcript_filename(session, all_messages) else {
            continue;
        };
        let default_stem = default_filename.trim_end_matches(".md").to_string();
        let Some(group) = grouping.classify(session, &default_filename) else {
            continue;
        };
        let session_digest = read_json_optional(
            &archive
                .root()
                .join("digests")
                .join(&session.source)
                .join(crate::path_key::identity_component(&session.native_id))
                .join("session.json"),
        )?;
        let all_message_days = workday::message_workdays(workday, all_messages);
        match config.unit {
            ViewUnit::Session => {
                let first_workday = workday::activity_days(workday, all_messages)
                    .first()
                    .cloned();
                let digest = overridden_digest(
                    first_day_digest(archive, session, workday, all_messages)?,
                    session,
                    first_workday.as_deref(),
                    overrides,
                );
                units.push(Unit {
                    session: session.clone(),
                    branch: None,
                    all_messages: all_messages.clone(),
                    all_message_days,
                    messages: all_messages.clone(),
                    indices: (0..all_messages.len()).collect(),
                    day_unit: false,
                    workday: first_workday,
                    created_at_ms: session.created_at_ms,
                    ongoing: session_day::is_ongoing_for(
                        &session.source,
                        session.captured_at_ms,
                        all_messages,
                        session.trailing_actions as u32,
                    ),
                    digest,
                    naming_title: None,
                    session_digest,
                    group,
                    child: grouping.is_child(session.id),
                    parent_id: grouping.parent_id(session.id),
                    parent_spawned_at_ms: parent_links
                        .get(&session.id)
                        .and_then(|(_, link)| link.spawned_at_ms),
                    parent_link: parent_links.get(&session.id).map(|(_, link)| link.clone()),
                    owns_trailing_actions: true,
                    default_stem,
                    relative_path: None,
                })
            }
            ViewUnit::SessionDay => {
                let message_days = workday::message_workdays(workday, all_messages);
                let mut by_day: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for (idx, day) in message_days.into_iter().enumerate() {
                    if let Some(day) = day {
                        by_day.entry(day).or_default().push(idx);
                    }
                }
                let last_day = by_day.keys().next_back().cloned();
                for (day, indices) in by_day {
                    let messages = indices
                        .iter()
                        .map(|idx| all_messages[*idx].clone())
                        .collect::<Vec<_>>();
                    let created_at_ms = indices
                        .iter()
                        .find_map(|idx| {
                            let message = &all_messages[*idx];
                            (message.role == Role::User)
                                .then_some(message.timestamp_ms)
                                .flatten()
                        })
                        .or_else(|| messages.iter().find_map(|message| message.timestamp_ms));
                    let digest = overridden_digest(
                        read_json_optional(
                            &archive
                                .root()
                                .join("digests")
                                .join(&session.source)
                                .join(crate::path_key::identity_component(&session.native_id))
                                .join(format!("{day}.json")),
                        )?,
                        session,
                        Some(&day),
                        overrides,
                    );
                    units.push(Unit {
                        session: session.clone(),
                        branch: None,
                        all_messages: all_messages.clone(),
                        all_message_days: all_message_days.clone(),
                        messages,
                        indices,
                        day_unit: true,
                        workday: Some(day.clone()),
                        created_at_ms,
                        ongoing: last_day.as_deref() == Some(day.as_str())
                            && session_day::is_ongoing_for(
                                &session.source,
                                session.captured_at_ms,
                                all_messages,
                                session.trailing_actions as u32,
                            ),
                        digest,
                        naming_title: None,
                        session_digest: session_digest.clone(),
                        group: group.clone(),
                        child: grouping.is_child(session.id),
                        parent_id: grouping.parent_id(session.id),
                        parent_spawned_at_ms: parent_links
                            .get(&session.id)
                            .and_then(|(_, link)| link.spawned_at_ms),
                        parent_link: parent_links.get(&session.id).map(|(_, link)| link.clone()),
                        owns_trailing_actions: last_day.as_deref() == Some(day.as_str()),
                        default_stem: default_stem.clone(),
                        relative_path: None,
                    });
                }
            }
        }
    }
    Ok(units)
}

fn overridden_digest(
    mut digest: Option<Value>,
    session: &SessionRow,
    workday: Option<&str>,
    overrides: &LockedTitleOverrides,
) -> Option<Value> {
    let Some(workday) = workday else {
        return digest;
    };
    let Some(title) = overrides.get(&(
        session.source.clone(),
        session.native_id.clone(),
        workday.to_string(),
    )) else {
        return digest;
    };
    if let Some(digest) = digest.as_mut() {
        digest["lockedTitle"] = Value::String(title.clone());
    }
    digest
}

fn first_day_digest(
    archive: &Archive,
    session: &SessionRow,
    workday: &WorkdayConfig,
    messages: &[Message],
) -> Result<Option<Value>> {
    let Some(day) = workday::activity_days(workday, messages).first().cloned() else {
        return Ok(None);
    };
    read_json_optional(
        &archive
            .root()
            .join("digests")
            .join(&session.source)
            .join(crate::path_key::identity_component(&session.native_id))
            .join(format!("{day}.json")),
    )
}

fn read_json_optional(path: &Path) -> Result<Option<Value>> {
    crate::digest::ensure_ready(path)?;
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing Digest {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading Digest {}", path.display())),
    }
}

pub(super) fn build_units(
    sessions: &[SessionRow],
    messages: &BTreeMap<i64, Vec<Message>>,
    inputs: &BuildInputs<'_>,
) -> Result<Vec<Unit>> {
    super::branches::build(sessions, messages, inputs)
}
