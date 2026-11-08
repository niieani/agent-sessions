//! Throughput of the last completed run, read from `digest-status.json`.

use std::path::Path;

use anyhow::{Context, Result};

use crate::digest::progress::{self, RunState, STATUS_FILE};

/// The completed run whose pace estimates the planned work.
#[derive(Debug, Clone, PartialEq)]
pub struct LastRun {
    pub command: String,
    pub started_at: String,
    pub finished_at: String,
    /// Settled items, successful or failed.
    pub done: usize,
    pub per_hour: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pace {
    LastRun(LastRun),
    /// No usable rate; the reason names what is missing.
    Unknown {
        reason: String,
    },
}

/// The whole-run average of the latest run when it completed. A missing
/// status file means no rate history; an unreadable one is an error.
pub fn last_run_pace(archive_root: &Path) -> Result<Pace> {
    let path = archive_root.join(STATUS_FILE);
    let exists = path
        .try_exists()
        .with_context(|| format!("checking {}", path.display()))?;
    if !exists {
        return Ok(unknown(format!("no {STATUS_FILE}")));
    }
    let status = progress::inspect(archive_root)?.status;
    let state = match status.state {
        RunState::Completed => None,
        RunState::Planning | RunState::Running => Some("is still running"),
        RunState::Failed => Some("failed"),
    };
    if let Some(state) = state {
        return Ok(unknown(format!(
            "the latest {} run {state}",
            status.command
        )));
    }
    let started = crate::timefmt::parse_iso_ms(&status.started_at)
        .with_context(|| format!("reading started_at in {}", path.display()))?;
    let finished = crate::timefmt::parse_iso_ms(&status.updated_at)
        .with_context(|| format!("reading updated_at in {}", path.display()))?;
    let elapsed_ms = finished - started;
    if status.done == 0 || elapsed_ms <= 0 {
        return Ok(unknown(format!(
            "the latest {} run settled no items",
            status.command
        )));
    }
    Ok(Pace::LastRun(LastRun {
        per_hour: status.done as f64 * 3_600_000.0 / elapsed_ms as f64,
        command: status.command,
        started_at: status.started_at,
        finished_at: status.updated_at,
        done: status.done,
    }))
}

fn unknown(reason: String) -> Pace {
    Pace::Unknown { reason }
}
