//! The `activity` query: which Sessions saw Dialog on a given Workday.
//!
//! Candidate selection uses the regenerable `messages(session_id,
//! timestamp_ms)` index; Workday attribution (turn ownership, day-start
//! rollover) is computed by [`crate::workday`] from the full Message list, so
//! the answer is exactly the set of Workdays rendered into `activityDays`.

use anyhow::Result;

use crate::archive::Archive;
use crate::archive::db::SessionRow;
use crate::fleet::FleetConfig;
use crate::workday::{self, WorkdayConfig};

/// Sessions with Dialog activity on `day` (`YYYY-MM-DD` in the configured
/// Workday timezone), ordered by creation time then identity.
pub fn sessions_active_on(archive: &Archive, day: &str) -> Result<Vec<SessionRow>> {
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let workday = WorkdayConfig::new(&config.views.workday)?;
    sessions_active_on_with(archive, &workday, day)
}

/// As [`sessions_active_on`], with an explicit Workday clock.
pub fn sessions_active_on_with(
    archive: &Archive,
    workday: &WorkdayConfig,
    day: &str,
) -> Result<Vec<SessionRow>> {
    let (start_ms, end_ms) = workday.day_bounds(day)?;
    let candidates = archive
        .db()?
        .sessions_with_messages_between(start_ms, end_ms)?;
    let mut active: Vec<SessionRow> = candidates
        .into_iter()
        .filter(|(_, messages)| {
            workday::activity_days(workday, messages)
                .iter()
                .any(|d| d == day)
        })
        .map(|(session, _)| session)
        .collect();
    active.sort_by(|a, b| {
        (a.created_at_ms, &a.source, &a.native_id).cmp(&(b.created_at_ms, &b.source, &b.native_id))
    });
    Ok(active)
}

/// Whether `session` is Ongoing on `day`. Earlier Session Days are always
/// closed; only the final attributed Workday can inherit the Capture signal.
pub fn session_is_ongoing_on(archive: &Archive, session: &SessionRow, day: &str) -> Result<bool> {
    let config = FleetConfig::load(archive.root())?.unwrap_or_default();
    let workday = WorkdayConfig::new(&config.views.workday)?;
    let messages = archive.db()?.messages(session.id)?;
    let days = workday::message_workdays(&workday, &messages);
    let final_day = days.iter().rev().flatten().next();
    if final_day.map(String::as_str) != Some(day) {
        return Ok(false);
    }
    Ok(crate::session_day::is_ongoing_for(
        &session.source,
        session.captured_at_ms,
        &messages,
        session.trailing_actions as u32,
    ))
}
