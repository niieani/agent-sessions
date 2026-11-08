//! The human `pull` report: one row per Host/Source, one row per Unreachable
//! Host, and the final `pull complete` line.

use super::collect::FleetStats;
use crate::timefmt::format_elapsed;

/// Report lines for `fleet`, which pulled from `host_count` configured Hosts.
pub fn summary_lines(fleet: &FleetStats, host_count: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source in &fleet.totals.sources {
        lines.push(format!(
            "{}/{}  found {:>5}  new {:>5}  extended {:>5}  revised {:>3}  unchanged {:>5}  refused {:>3}  retry-later {:>3}  failed {:>3}  {:.2}s",
            source.host,
            source.source,
            source.sessions_found,
            source.sessions_new,
            source.sessions_extended,
            source.sessions_revised,
            source.sessions_unchanged,
            source.sessions_refused,
            source.sessions_retry_later,
            source.sessions_failed,
            source.elapsed_ms as f64 / 1000.0,
        ));
    }
    for refresh in &fleet.totals.repository_refresh {
        lines.push(format!(
            "{}/repository-resolution  failed {} · retry-later {}{}{}",
            refresh.host,
            refresh.units_failed,
            refresh.retry_later,
            refresh
                .retry_sample
                .as_ref()
                .map(|p| format!(" · sample {p:?}"))
                .unwrap_or_default(),
            refresh
                .error
                .as_ref()
                .map(|e| format!("  error: {e}"))
                .unwrap_or_default()
        ));
    }
    for host in &fleet.unreachable_hosts {
        let state = match (&host.failure, host.last_success_ms) {
            (Some(failure), _) => format!("FAILED: {failure}"),
            (None, Some(_)) => format!(
                "skipped; last successful pull {} ago",
                format_elapsed(host.unpulled_for_ms)
            ),
            (None, None) => format!(
                "skipped; no successful pull recorded in {} of tracking",
                format_elapsed(host.unpulled_for_ms)
            ),
        };
        lines.push(format!(
            "{}  unreachable  {state}  (unreachable-grace {})  ({})",
            host.host, host.grace, host.detail
        ));
    }
    let totals = &fleet.totals;
    let overdue = fleet
        .unreachable_hosts
        .iter()
        .filter(|host| host.failure.is_some())
        .count();
    let unreachable = match (fleet.unreachable_hosts.len(), overdue) {
        (0, _) => String::new(),
        (count, 0) => format!(", {count} unreachable"),
        (count, overdue) => format!(", {count} unreachable ({overdue} beyond unreachable-grace)"),
    };
    let refresh_suffix = if totals.repository_refresh.is_empty() {
        String::new()
    } else {
        format!(
            ", {} repository units failed, {} repository refresh errors",
            totals
                .repository_refresh
                .iter()
                .map(|r| r.units_failed)
                .sum::<usize>(),
            totals
                .repository_refresh
                .iter()
                .filter(|r| r.error.is_some())
                .count()
        )
    };
    lines.push(format!(
        "pull {}: {}/{} hosts{unreachable}, {} changed ({} revised), {} unchanged, {} retry-later, {} failed ({} refused){refresh_suffix}",
        if totals.sessions_captured == 0 {
            "complete (nothing new)"
        } else {
            "complete"
        },
        fleet.hosts_pulled,
        host_count,
        totals.sessions_captured,
        totals.sessions_revised,
        totals.sessions_unchanged,
        totals.sessions_retry_later,
        totals.sessions_failed,
        totals.sessions_refused,
    ));
    lines
}

#[cfg(test)]
mod repository_refresh_tests {
    use super::*;
    use crate::pull::{PullStats, SourcePullStats};
    #[test]
    fn refresh_error_preserves_successful_source_counts_and_reports_its_own_failure() {
        let mut stats = PullStats::default();
        stats.add_source(SourcePullStats {
            host: "studio".into(),
            source: "claude".into(),
            sessions_found: 12,
            sessions_new: 7,
            sessions_unchanged: 5,
            conversations_extracted: 12,
            ..Default::default()
        });
        stats.record_repository_refresh("studio", Err(anyhow::anyhow!("database refresh failed")));
        let mut fleet = FleetStats {
            hosts_pulled: 1,
            ..Default::default()
        };
        fleet.totals.merge(stats);
        assert_eq!(fleet.hosts_failed, 0);
        assert_eq!(fleet.totals.sessions_found, 12);
        assert_eq!(fleet.totals.sessions_new, 7);
        assert_eq!(fleet.totals.sessions_failed, 0);
        assert_eq!(fleet.totals.sources.len(), 1);
        assert!(fleet.failed());
        let lines = summary_lines(&fleet, 1);
        assert!(lines[0].contains("found    12"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("studio/repository-resolution")
                    && l.contains("database refresh failed"))
        );
        assert!(lines.last().unwrap().contains("1/1 hosts"));
    }
}
