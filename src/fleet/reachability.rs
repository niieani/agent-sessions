//! Unreachable Hosts and the Unreachable Grace: an Unreachable Host is
//! skipped without failing the Pull until its last successful Pull (or,
//! without one, the start of tracking) is older than `[pull]
//! unreachable-grace` or its per-Host override.

use std::time::Duration;

use crate::archive::HostPullRecord;
use crate::timefmt::{format_elapsed, frontmatter};

/// A Host that could not be contacted during this Pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableHost {
    pub host: String,
    /// Effective Unreachable Grace, rendered without rounding.
    pub grace: String,
    /// ssh's connection error.
    pub detail: String,
    pub last_success_ms: Option<i64>,
    /// How long the Host has gone without a successful Pull (since tracking
    /// began, when it has none).
    pub unpulled_for_ms: i64,
    /// Set when the Host exceeded its Unreachable Grace: the Pull fails.
    pub failure: Option<String>,
}

/// Judge an Unreachable Host against its Pull health record at `now_ms`.
pub(super) fn judge(
    host: &str,
    detail: String,
    record: HostPullRecord,
    now_ms: i64,
    grace: Duration,
) -> UnreachableHost {
    let reference = record.last_success_ms.unwrap_or(record.tracked_since_ms);
    let unpulled_for_ms = now_ms.saturating_sub(reference);
    let overdue = i128::from(unpulled_for_ms) > grace.as_millis() as i128;
    let millis = grace.as_millis();
    let (amount, unit) = [
        (86_400_000, "d"),
        (3_600_000, "h"),
        (60_000, "m"),
        (1_000, "s"),
        (1, "ms"),
    ]
    .into_iter()
    .find(|(factor, _)| millis.is_multiple_of(*factor))
    .map(|(factor, unit)| (millis / factor, unit))
    .expect("milliseconds always divide by one");
    let grace_text = format!("{amount}{unit}");
    let failure = overdue.then(|| match record.last_success_ms {
        Some(last) => format!(
            "{host} is unreachable and its last successful pull was {} ago ({}), \
             beyond unreachable-grace {grace_text}",
            format_elapsed(unpulled_for_ms),
            frontmatter(last)
        ),
        None => format!(
            "{host} is unreachable and has no successful pull recorded in the {} since \
             tracking began ({}), beyond unreachable-grace {grace_text}",
            format_elapsed(unpulled_for_ms),
            frontmatter(record.tracked_since_ms)
        ),
    });
    UnreachableHost {
        host: host.to_string(),
        grace: grace_text,
        detail,
        last_success_ms: record.last_success_ms,
        unpulled_for_ms,
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const WEEK: Duration = Duration::from_secs(7 * 86_400);

    fn record(tracked: i64, last: Option<i64>) -> HostPullRecord {
        HostPullRecord {
            tracked_since_ms: tracked,
            last_success_ms: last,
        }
    }

    #[test]
    fn grace_is_measured_from_last_success_else_from_tracking() {
        let now = 100 * DAY;
        let within = judge("h", "x".into(), record(0, Some(now - 7 * DAY)), now, WEEK);
        assert!(within.failure.is_none(), "exactly at the grace boundary");
        let over = judge(
            "h",
            "x".into(),
            record(0, Some(now - 7 * DAY - 1)),
            now,
            WEEK,
        );
        assert!(over.failure.unwrap().contains("last successful pull"));

        let fresh = judge("h", "x".into(), record(now - DAY, None), now, WEEK);
        assert!(fresh.failure.is_none());
        let stale = judge("h", "x".into(), record(now - 8 * DAY, None), now, WEEK);
        assert!(stale.failure.unwrap().contains("no successful pull"));
    }

    #[test]
    fn zero_grace_fails_any_unreachable_host_with_history() {
        let now = 100 * DAY;
        let host = judge(
            "h",
            "x".into(),
            record(0, Some(now - 1)),
            now,
            Duration::ZERO,
        );
        assert!(host.failure.is_some());
    }
}
