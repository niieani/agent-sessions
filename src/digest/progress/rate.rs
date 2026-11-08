//! Recent throughput over a moving window, and compact duration text.

use std::collections::VecDeque;

/// How far back the ETA's rate looks. Long enough to smooth over slow
/// Sessions, short enough to follow a backlog whose pace changes by Source.
pub(super) const RATE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Settled-work samples inside the window, plus the newest sample taken at
/// or before the window's start as its anchor.
pub(super) struct RateWindow {
    span_ms: i64,
    samples: VecDeque<(i64, usize)>,
}

impl RateWindow {
    pub(super) fn new(span_ms: i64, start_ms: i64) -> Self {
        Self {
            span_ms,
            samples: VecDeque::from([(start_ms, 0)]),
        }
    }

    pub(super) fn record(&mut self, now_ms: i64, done: usize) {
        self.samples.push_back((now_ms, done));
        self.prune(now_ms);
    }

    /// Settled items per hour between the anchor and `now_ms`; `None` until
    /// time has passed. A window without settled work reports zero.
    pub(super) fn per_hour(&mut self, now_ms: i64, done: usize) -> Option<f64> {
        self.prune(now_ms);
        let (since_ms, base) = self.samples[0];
        let elapsed_ms = now_ms - since_ms;
        (elapsed_ms > 0).then(|| done.saturating_sub(base) as f64 * 3_600_000.0 / elapsed_ms as f64)
    }

    fn prune(&mut self, now_ms: i64) {
        while self.samples.len() > 1 && self.samples[1].0 <= now_ms - self.span_ms {
            self.samples.pop_front();
        }
    }
}

/// Time left at `per_hour`; `None` while the rate is unknown or zero.
pub(super) fn eta_ms(remaining: usize, per_hour: Option<f64>) -> Option<i64> {
    if remaining == 0 {
        return Some(0);
    }
    let rate = per_hour.filter(|rate| *rate > 0.0)?;
    Some((remaining as f64 / rate * 3_600_000.0).round() as i64)
}

/// `21h40m`, `7m`, or `42s`.
pub(in crate::digest) fn format_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    match seconds {
        3600.. => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
        60.. => format!("{}m", seconds / 60),
        _ => format!("{seconds}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    #[test]
    fn rate_follows_the_recent_window_not_the_all_time_average() {
        let mut window = RateWindow::new(30 * MINUTE, 0);
        // A slow first 10 hours: one item per hour.
        for hour in 1..=10 {
            window.record(hour * 60 * MINUTE, hour as usize);
        }
        // Then one item per minute for the last half hour.
        let start = 10 * 60 * MINUTE;
        for minute in 1..=30 {
            window.record(start + minute * MINUTE, 10 + minute as usize);
        }
        let rate = window.per_hour(start + 30 * MINUTE, 40).unwrap();
        assert!((rate - 60.0).abs() < 0.01, "{rate}");
    }

    #[test]
    fn a_stalled_window_has_no_eta() {
        let mut window = RateWindow::new(30 * MINUTE, 0);
        window.record(10 * MINUTE, 5);
        let rate = window.per_hour(3 * 60 * MINUTE, 5);
        assert_eq!(rate, Some(0.0));
        assert_eq!(eta_ms(10, rate), None);
        assert_eq!(eta_ms(0, rate), Some(0));
    }

    #[test]
    fn eta_divides_remaining_work_by_the_rate() {
        assert_eq!(eta_ms(30, Some(60.0)), Some(30 * MINUTE));
        assert_eq!(eta_ms(3, None), None);
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(format_duration((21 * 60 + 40) * MINUTE + 59_000), "21h40m");
        assert_eq!(format_duration(3 * 60 * MINUTE + 5 * MINUTE), "3h05m");
        assert_eq!(format_duration(7 * MINUTE), "7m");
        assert_eq!(format_duration(42_000), "42s");
    }
}
