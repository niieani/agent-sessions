use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use super::collisions::{Allocator, case_variants, with_identity_suffix};
use super::layout::{LayoutTemplate, Layouts};
use super::{Unit, UnitKey};
use crate::render::RenderStats;

/// Assigns every unit's path. Case-variant detection needs every candidate,
/// and a Subagent's candidate depends on its parent's final path, so planning
/// repeats until no new case variant appears; marks only grow, so it ends.
pub(super) fn assign_paths(
    units: &mut [Unit],
    layouts: &Layouts,
    stats: &mut RenderStats,
) -> Result<()> {
    units.sort_by(|left, right| {
        left.session
            .created_at_ms
            .cmp(&right.session.created_at_ms)
            .then_with(|| left.session.source.cmp(&right.session.source))
            .then_with(|| left.session.native_id.cmp(&right.session.native_id))
            .then_with(|| left.workday.cmp(&right.workday))
    });
    let key_to_index = units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.key(), index))
        .collect::<BTreeMap<_, _>>();
    let parent_indices = units
        .iter()
        .map(|unit| parent_unit_key(unit, units).and_then(|key| key_to_index.get(&key).copied()))
        .collect::<Vec<_>>();
    let mut variants = BTreeSet::new();
    loop {
        for unit in units.iter_mut() {
            unit.relative_path = None;
        }
        let mut pass = Pass {
            parent_indices: &parent_indices,
            layouts,
            variants: &variants,
            allocator: Allocator::default(),
            visits: vec![Visit::Pending; units.len()],
            candidates: BTreeMap::new(),
            skipped: Vec::new(),
        };
        for index in 0..units.len() {
            pass.assign(index, units)?;
        }
        let found = case_variants(&pass.candidates);
        if found.is_subset(&variants) {
            stats.skipped.extend(pass.skipped);
            return Ok(());
        }
        variants.extend(found);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Visit {
    Pending,
    Active,
    Done,
}

struct Pass<'a> {
    parent_indices: &'a [Option<usize>],
    layouts: &'a Layouts,
    variants: &'a BTreeSet<UnitKey>,
    allocator: Allocator,
    visits: Vec<Visit>,
    candidates: BTreeMap<UnitKey, PathBuf>,
    skipped: Vec<String>,
}

impl Pass<'_> {
    fn skip(&mut self, unit: &Unit, why: &str) {
        let reason = format!(
            "{}/{}{}: {why}",
            unit.session.source,
            unit.session.native_id,
            unit.workday
                .as_ref()
                .map(|day| format!("/{day}"))
                .unwrap_or_default()
        );
        tracing::debug!(unit = %reason, "View unit skipped");
        self.skipped.push(reason);
    }

    fn assign(&mut self, index: usize, units: &mut [Unit]) -> Result<()> {
        if self.visits[index] == Visit::Done {
            return Ok(());
        }
        if units[index].relative_path.is_some() {
            self.visits[index] = Visit::Done;
            return Ok(());
        }
        anyhow::ensure!(
            self.visits[index] != Visit::Active,
            "Subagent parent cycle in View plan"
        );
        self.visits[index] = Visit::Active;
        if let Some(parent_index) = self.parent_indices[index] {
            self.assign(parent_index, units)?;
            if units[parent_index].relative_path.is_none() {
                self.skip(&units[index], "rendered parent View unit is unavailable");
                self.visits[index] = Visit::Done;
                return Ok(());
            }
        }
        let unit = &units[index];
        let (parent_dir, parent_stem) = if self.layouts.defaults {
            (
                unit.group.relative_dir.to_string_lossy().into_owned(),
                unit.default_stem.clone(),
            )
        } else {
            self.parent_indices[index]
                .and_then(|parent_index| units[parent_index].relative_path.as_ref())
                .map(|path| {
                    (
                        path.parent()
                            .unwrap_or_else(|| Path::new(""))
                            .to_string_lossy()
                            .into_owned(),
                        path.file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        unit.group.relative_dir.to_string_lossy().into_owned(),
                        unit.default_stem.clone(),
                    )
                })
        };
        let layout = self.layouts.transcript(unit);
        if self.layouts.unit_requires_title(unit) && unit.locked_title().is_none() {
            self.skip(unit, "layout requires Digest Locked Title");
            self.visits[index] = Visit::Done;
            return Ok(());
        }
        let context = layout_context(unit, Some(&parent_dir), Some(&parent_stem));
        let mut candidate = self
            .layouts
            .branch_path(unit, &layout.render(&context)?, context)?;
        if self.variants.contains(&unit.key()) {
            candidate = with_identity_suffix(&candidate, &short_identity(unit));
        }
        let path = self.allocator.allocate(&candidate);
        self.candidates.insert(unit.key(), candidate);
        units[index].relative_path = Some(path);
        self.visits[index] = Visit::Done;
        Ok(())
    }
}

/// JSON View paths for every unit with a Transcript path, in planning order,
/// under the same collision rules as Transcripts.
pub(super) fn json_paths(
    units: &[Unit],
    layout: &LayoutTemplate,
) -> Result<BTreeMap<UnitKey, PathBuf>> {
    let mut candidates = Vec::new();
    for unit in units.iter().filter(|unit| unit.relative_path.is_some()) {
        let (parent_dir, parent_stem) = resolved_parent_context(unit, units);
        let candidate =
            layout.render(&layout_context(unit, Some(&parent_dir), Some(&parent_stem)))?;
        candidates.push((unit, super::branches::path(unit, &candidate)));
    }
    let keys = candidates
        .iter()
        .map(|(unit, _)| unit.key())
        .collect::<Vec<_>>();
    let variants = case_variants(keys.iter().zip(candidates.iter().map(|(_, path)| path)));
    let mut allocator = Allocator::default();
    Ok(candidates
        .into_iter()
        .map(|(unit, candidate)| {
            let candidate = if variants.contains(&unit.key()) {
                with_identity_suffix(&candidate, &short_identity(unit))
            } else {
                candidate
            };
            (unit.key(), allocator.allocate(&candidate))
        })
        .collect())
}

fn short_identity(unit: &Unit) -> String {
    crate::render::short_hex(&unit.session.source, &unit.session.native_id)
}

pub(super) fn parent_unit_key(unit: &Unit, units: &[Unit]) -> Option<UnitKey> {
    let parent_id = unit.parent_id?;
    if !unit.day_unit {
        return Some((parent_id, None, None));
    }
    let parent = units
        .iter()
        .find(|candidate| candidate.session.id == parent_id)?;
    let spawn_day = unit.parent_spawned_at_ms.and_then(|spawned| {
        parent
            .all_messages
            .iter()
            .zip(&parent.all_message_days)
            .filter(|(message, _)| message.timestamp_ms.is_some_and(|at| at <= spawned))
            .filter_map(|(_, day)| day.as_ref())
            .next_back()
            .cloned()
    });
    let day = spawn_day.or_else(|| {
        units
            .iter()
            .filter(|candidate| candidate.session.id == parent_id)
            .filter_map(|candidate| candidate.workday.clone())
            .min()
    });
    Some((parent_id, parent.branch.as_ref().map(|b| b.id.clone()), day))
}

pub(super) fn layout_context(
    unit: &Unit,
    parent_dir: Option<&str>,
    parent_stem: Option<&str>,
) -> Value {
    let project_dir = unit
        .group
        .relative_dir
        .strip_prefix("projects")
        .unwrap_or(&unit.group.relative_dir)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string();
    let workday = unit.workday.clone().unwrap_or_default();
    let month = workday
        .get(..7)
        .or_else(|| unit.default_stem.get(..7))
        .unwrap_or("unknown")
        .to_string();
    let mut object = Map::new();
    object.insert(
        "project".into(),
        Value::String(unit.group.project_path_name.clone().unwrap_or_default()),
    );
    object.insert("project_dir".into(), Value::String(project_dir));
    object.insert("workday".into(), Value::String(workday));
    object.insert("month".into(), Value::String(month));
    object.insert(
        "default_stem".into(),
        Value::String(unit.default_stem.clone()),
    );
    object.insert(
        "short_id".into(),
        Value::String(crate::render::short_hex(
            &unit.session.source,
            &unit.session.native_id,
        )),
    );
    object.insert("source".into(), Value::String(unit.session.source.clone()));
    object.insert(
        "native_title".into(),
        Value::String(unit.session.title.clone().unwrap_or_default()),
    );
    object.insert(
        "parent_dir".into(),
        Value::String(parent_dir.unwrap_or_default().into()),
    );
    object.insert(
        "parent_stem".into(),
        Value::String(parent_stem.unwrap_or_default().into()),
    );
    if let Some(title) = unit.locked_title() {
        object.insert("title".into(), Value::String(title.into()));
    }
    // Layout literals and directory variables define topology; scalar facts never do.
    for (name, value) in &mut object {
        if !matches!(name.as_str(), "parent_dir" | "project_dir")
            && let Value::String(text) = value
        {
            *text = crate::render::filename::scalar(text);
        }
    }
    Value::Object(object)
}

pub(super) fn resolved_parent_context(unit: &Unit, units: &[Unit]) -> (String, String) {
    parent_unit_key(unit, units)
        .and_then(|key| units.iter().find(|candidate| candidate.key() == key))
        .and_then(|parent| parent.relative_path.as_ref())
        .map(|path| {
            (
                path.parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_string_lossy()
                    .into_owned(),
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .unwrap_or_else(|| {
            (
                unit.group.relative_dir.to_string_lossy().into_owned(),
                unit.default_stem.clone(),
            )
        })
}
