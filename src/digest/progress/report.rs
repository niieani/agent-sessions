//! Human text for progress: the periodic log line and `digest status`.

use std::fmt::Write as _;

use anyhow::Result;

use super::rate::format_duration;
use super::status::{DigestStatus, ItemKind, Liveness, RunState, StatusReport};

/// `digest progress 412/1180 (35%) · chatgpt 2026-04-12 · failed 0 · rejected 3 · elapsed 21h40m · ETA 3h10m`
///
/// The ETA appears only for a run still in progress.
pub(super) fn progress_line(status: &DigestStatus, elapsed_ms: i64, live: bool) -> String {
    let label = match status.state {
        RunState::Planning | RunState::Running => "progress",
        RunState::Completed => "finished",
        RunState::Failed => "failed",
    };
    let mut line = format!(
        "{} {label} {}/{} ({}%)",
        status.command,
        status.done,
        status.total,
        percent(status.done, status.total)
    );
    if let Some(current) = &status.current {
        write!(line, " · {} {}", current.source, current.workday).expect("writing to String");
    }
    write!(
        line,
        " · failed {} · rejected {} · elapsed {}",
        status.failed,
        status.rejected,
        format_duration(elapsed_ms)
    )
    .expect("writing to String");
    if live && matches!(status.state, RunState::Planning | RunState::Running) {
        match status.eta_seconds {
            Some(seconds) => write!(line, " · ETA {}", format_duration(seconds as i64 * 1000)),
            None => write!(line, " · ETA unknown"),
        }
        .expect("writing to String");
    }
    line
}

/// Rounded, but never 100% while work remains.
fn percent(done: usize, total: usize) -> usize {
    if total == 0 || done >= total {
        return 100;
    }
    ((done as f64 * 100.0 / total as f64).round() as usize).min(99)
}

/// The `digest status` text for a report read at `now_ms`.
pub fn render_report(report: &StatusReport, now_ms: i64) -> Result<String> {
    let status = &report.status;
    let started_ms = crate::timefmt::parse_iso_ms(&status.started_at)?;
    let updated_ms = crate::timefmt::parse_iso_ms(&status.updated_at)?;
    let state = match status.state {
        RunState::Planning => "planning",
        RunState::Running => "running",
        RunState::Completed => "completed",
        RunState::Failed => "failed",
    };
    let mut out = String::new();
    let who = format!("pid {} on {}", status.pid, status.host);
    match report.liveness {
        Liveness::Live => writeln!(out, "{} {state} · {who}", status.command),
        Liveness::Finished => writeln!(out, "{} {state} · {who} has exited", status.command),
        Liveness::Starting => writeln!(
            out,
            "a new run holds the Digest status lock and has not reported yet; previous {} {state} · {who}",
            status.command
        ),
        Liveness::Dead => writeln!(
            out,
            "STALE: {} {state} but {who} no longer holds the Digest status lock; it died without finishing",
            status.command
        ),
    }?;
    let live = report.liveness == Liveness::Live;
    let end_ms = if live { now_ms } else { updated_ms };
    writeln!(out, "{}", progress_line(status, end_ms - started_ms, live))?;
    if !status.sources.is_empty() {
        let sources = status
            .sources
            .iter()
            .map(|(source, p)| format!("{source} {}/{} (failed {})", p.done, p.total, p.failed))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "sources: {sources}")?;
    }
    if let Some(current) = &status.current {
        writeln!(
            out,
            "{}: {}:{} {} {} ({} in flight)",
            if live { "current" } else { "last started" },
            current.source,
            current.session,
            current.workday,
            match current.kind {
                ItemKind::Day => "Session Day",
                ItemKind::Title => "Session title",
            },
            status.in_flight
        )?;
    }
    if live && let Some(rate) = status.rate_per_hour {
        write!(out, "rate {rate:.1}/h")?;
        match &status.eta_at {
            Some(eta_at) => writeln!(out, " · expected to finish {eta_at}"),
            None => writeln!(out, " · no recent progress to estimate from"),
        }?;
    }
    writeln!(
        out,
        "started {} · updated {} ({} ago)",
        status.started_at,
        status.updated_at,
        format_duration(now_ms - updated_ms)
    )?;
    if let Some(error) = &status.error {
        writeln!(out, "error: {error}")?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::status::{CurrentItem, ItemKind, new_status};
    use super::*;

    #[test]
    fn progress_line_matches_the_documented_shape() {
        let mut status = new_status("digest", "2026-10-21T03:00:00.000Z".into(), 300_000);
        status.state = RunState::Running;
        (status.done, status.total, status.rejected) = (412, 1180, 3);
        status.current = Some(CurrentItem {
            source: "chatgpt".into(),
            session: "abc".into(),
            workday: "2026-04-12".into(),
            kind: ItemKind::Day,
        });
        status.eta_seconds = Some(3 * 3600 + 10 * 60);
        assert_eq!(
            progress_line(&status, (21 * 60 + 40) * 60_000, true),
            "digest progress 412/1180 (35%) · chatgpt 2026-04-12 · failed 0 · rejected 3 · elapsed 21h40m · ETA 3h10m"
        );
    }

    #[test]
    fn percent_never_claims_completion_early() {
        assert_eq!(percent(999, 1000), 99);
        assert_eq!(percent(0, 0), 100);
    }
}
