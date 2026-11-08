//! Source-neutral branch Units use ordinary templates, workdays and path plans.
use super::units::{BuildInputs, build_linear_units};
use super::{Unit, UnitKey};
use crate::archive::db::{Db, SessionRow};
use crate::model::Message;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[derive(Clone)]
pub(super) struct BranchContext {
    pub id: String,
    pub parent_id: Option<String>,
    pub selected: bool,
    pub shared: bool,
    pub number: usize,
    pub path_id: Option<String>,
    pub fork_message_id: Option<String>,
    pub message_count: usize,
}
pub(super) fn build(
    sessions: &[SessionRow],
    messages: &BTreeMap<i64, Vec<Message>>,
    inputs: &BuildInputs<'_>,
) -> Result<Vec<Unit>> {
    let db = Db::open_read_only(&inputs.archive.db_path())?;
    let mut result = Vec::new();
    for session in sessions {
        let branches = db.branches(session.id)?;
        if branches.is_empty() {
            result.extend(build_linear_units(
                std::slice::from_ref(session),
                messages,
                inputs,
            )?);
            continue;
        }
        for path in crate::model::branch_paths(&branches)? {
            let branch = &path.segment;
            let inherited = path.ancestors.len();
            let mut basis = path.ancestors;
            basis.extend(branch.messages.clone());
            let mut segment_session = session.clone();
            segment_session.trailing_actions = i64::from(branch.trailing_actions);
            let segment_messages = BTreeMap::from([(session.id, basis)]);
            let mut units = build_linear_units(
                std::slice::from_ref(&segment_session),
                &segment_messages,
                inputs,
            )?;
            let root = crate::digest::digest_dir(
                inputs.archive.root(),
                &session.source,
                &session.native_id,
            )?;
            let naming = first_digest(&root)?
                .and_then(|d| d.get("lockedTitle")?.as_str().map(str::to_owned));
            for unit in &mut units {
                unit.naming_title = unit
                    .locked_title()
                    .map(str::to_owned)
                    .or_else(|| naming.clone());
                if !path.shared {
                    let dir = crate::digest::path_digest_dir(&root, path.path_id.as_deref());
                    unit.digest = if unit.day_unit {
                        unit.workday
                            .as_ref()
                            .map(|day| crate::digest::read_day(&dir.join(format!("{day}.json"))))
                            .transpose()?
                            .flatten()
                            .map(serde_json::to_value)
                            .transpose()?
                    } else {
                        first_digest(&dir)?
                    };
                }

                unit.branch = Some(BranchContext {
                    id: branch.id.clone(),
                    parent_id: branch.parent_id.clone(),
                    selected: branch.selected,
                    shared: path.shared,
                    number: path.number,
                    path_id: path.path_id.clone(),
                    fork_message_id: branch.fork_message_id.clone(),
                    message_count: branch.messages.len(),
                });
                unit.indices.retain(|index| *index >= inherited);
                unit.messages = unit
                    .indices
                    .iter()
                    .map(|index| unit.all_messages[*index].clone())
                    .collect();
                for day in unit.all_message_days.iter_mut().take(inherited) {
                    *day = None;
                }
            }
            result.extend(units.into_iter().filter(|u| !u.messages.is_empty()));
        }
    }
    Ok(result)
}
pub(super) fn suffix(unit: &Unit) -> String {
    match &unit.branch {
        Some(branch) if branch.shared => format!(
            " ({})",
            branch
                .fork_message_id
                .as_deref()
                .expect("Shared Segment has Fork Message")
                .chars()
                .take(8)
                .collect::<String>()
        ),
        Some(branch) if branch.number > 1 => format!(" (branch {})", branch.number),
        _ => String::new(),
    }
}
pub(super) fn path(unit: &Unit, candidate: &Path) -> PathBuf {
    let suffix = suffix(unit);
    if suffix.is_empty() {
        return candidate.to_path_buf();
    }
    candidate.with_file_name(format!(
        "{}{}.{}",
        candidate.file_stem().unwrap_or_default().to_string_lossy(),
        suffix,
        candidate.extension().unwrap_or_default().to_string_lossy()
    ))
}
fn branch_units<'a>(unit: &Unit, units: &'a [Unit], id: &str) -> Vec<&'a Unit> {
    let mut found = units
        .iter()
        .filter(|candidate| {
            candidate.session.id == unit.session.id
                && candidate.branch.as_ref().is_some_and(|b| b.id == id)
                && candidate.relative_path.is_some()
        })
        .collect::<Vec<_>>();
    found.sort_by(|a, b| a.workday.cmp(&b.workday));
    found
}
pub(super) fn navigation(
    unit: &Unit,
    units: &[Unit],
    targets: &BTreeMap<UnitKey, PathBuf>,
    mut body: String,
) -> Result<String> {
    let Some(branch) = &unit.branch else {
        return Ok(body);
    };
    let own = unit
        .relative_path
        .as_ref()
        .context("branch Unit path absent")?;
    let href = |target: &Unit| -> Result<String> {
        Ok(crate::render::relative_href(
            own.parent().unwrap_or_else(|| Path::new("")),
            targets
                .get(&target.key())
                .context("branch target unavailable")?,
        ))
    };
    let siblings = branch_units(unit, units, &branch.id);
    if siblings.last().is_some_and(|last| last.key() == unit.key()) {
        let mut children = BTreeMap::new();
        for child in units {
            if child.session.id == unit.session.id
                && let Some(context) = &child.branch
                && context.parent_id.as_deref() == Some(&branch.id)
            {
                children.entry(&context.id).or_insert(child);
            }
        }
        if !children.is_empty() {
            body.push_str("\n<branches>\n\n");
            for child in children.values() {
                let label = if child.branch.as_ref().unwrap().selected {
                    "Selected continuation"
                } else {
                    "Alternate continuation"
                };
                body.push_str(&format!("- [{label}]({})\n", href(child)?));
            }
            body.push_str("\n</branches>\n");
        }
    }
    Ok(body)
}

fn first_digest(dir: &Path) -> Result<Option<serde_json::Value>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut paths = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in paths {
        if path.is_file()
            && path.extension().is_some_and(|e| e == "json")
            && path.file_name().is_some_and(|n| n != "session.json")
        {
            return crate::digest::read_day(&path)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(Into::into);
        }
    }
    Ok(None)
}
