use std::collections::BTreeMap;

use anyhow::Result;

use super::layout::Layouts;
use super::output::output_root;
use super::planning::assign_paths;
use super::{BuildInputs, build_units, parent_links};
use crate::archive::Archive;
use crate::archive::db::Db;
use crate::fleet::FleetConfig;
use crate::render::{
    LockedTitleOverrides, PlannedTranscript, RenderStats, TranscriptPlan, ViewGrouping,
};
use crate::workday::WorkdayConfig;

pub(super) fn transcript_paths(
    archive: &Archive,
    overrides: &LockedTitleOverrides,
) -> Result<TranscriptPlan> {
    let db = Db::open_read_only(&archive.db_path())?;
    let sessions = db.sessions_with_dialog()?;
    let messages_by_id = sessions
        .iter()
        .map(|session| Ok((session.id, db.messages(session.id)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let workday = WorkdayConfig::new(&config.views.workday)?;
    let assignments = crate::assignments::load(archive.root())?;
    let grouping = ViewGrouping::new(&sessions, &messages_by_id, &config.views, &db, &assignments)?;
    let layouts = Layouts::load(&config.views)?;
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
            overrides,
        },
    )?;
    assign_paths(&mut units, &layouts, &mut RenderStats::default())?;
    Ok(TranscriptPlan {
        output: output_root(archive, &config.views)?,
        transcripts: units
            .into_iter()
            .filter_map(|unit| {
                unit.relative_path.map(|relative| PlannedTranscript {
                    source: unit.session.source,
                    native_id: unit.session.native_id,
                    workday: unit.workday,
                    path: relative,
                })
            })
            .collect(),
    })
}
