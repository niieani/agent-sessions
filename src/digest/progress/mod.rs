//! Live progress of one digest or repair-digests run.
//!
//! Work is counted before any request: each selected Session Day plus each
//! planned Session title is one item. Workers report items as they start and
//! settle; every change rewrites `digest-status.json`, and a reporter thread
//! logs the progress line and refreshes the file every `progress-interval`.

mod rate;
mod report;
mod status;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;

pub use report::render_report;
pub use status::{
    CurrentItem, DigestStatus, ItemKind, Liveness, RunState, STATUS_FILE, SourceProgress,
    StatusReport, inspect,
};

pub(in crate::digest) use rate::format_duration;
use rate::{RATE_WINDOW_MS, RateWindow};
use status::StatusWriter;

/// The work one Session contributes to a run's total.
pub(super) struct PlannedSession<'a> {
    pub source: &'a str,
    pub session: &'a str,
    pub days: usize,
    pub title: bool,
}

pub(super) struct Progress {
    interval: Duration,
    /// `None` for dry runs, which neither write nor log progress.
    tracked: Option<(StatusWriter, Mutex<State>)>,
}

struct State {
    status: DigestStatus,
    started_ms: i64,
    /// Planned and settled item counts per (Source, native id).
    sessions: HashMap<(String, String), (usize, usize)>,
    /// In-flight items by Session, with their start order.
    in_flight: HashMap<(String, String), (u64, CurrentItem)>,
    next_seq: u64,
    rate: RateWindow,
}

impl Progress {
    /// Take the status lock and record a planning run.
    pub(super) fn begin(archive_root: &Path, command: &str, interval: Duration) -> Result<Self> {
        let writer = StatusWriter::acquire(archive_root)?;
        let now = crate::adapters::now_ms();
        let status = status::new_status(
            command,
            crate::timefmt::iso_ms(now),
            interval.as_millis() as u64,
        );
        writer.write(&status)?;
        Ok(Self {
            interval,
            tracked: Some((
                writer,
                Mutex::new(State {
                    status,
                    started_ms: now,
                    sessions: HashMap::new(),
                    in_flight: HashMap::new(),
                    next_seq: 0,
                    rate: RateWindow::new(RATE_WINDOW_MS, now),
                }),
            )),
        })
    }

    /// Progress that records nothing, for dry runs.
    pub(super) fn inert() -> Self {
        Self {
            interval: Duration::MAX,
            tracked: None,
        }
    }

    /// Fix the planned totals before any work starts.
    pub(super) fn plan<'a>(&self, sessions: impl IntoIterator<Item = PlannedSession<'a>>) {
        self.update(|state, now| {
            for planned in sessions {
                let items = planned.days + usize::from(planned.title);
                if items == 0 {
                    continue;
                }
                state.status.total += items;
                state
                    .status
                    .sources
                    .entry(planned.source.into())
                    .or_default()
                    .total += items;
                state
                    .sessions
                    .insert((planned.source.into(), planned.session.into()), (items, 0));
            }
            state.status.state = RunState::Running;
            state.rate = RateWindow::new(RATE_WINDOW_MS, now);
            let sources = state
                .status
                .sources
                .iter()
                .map(|(source, p)| format!("{source} {}", p.total))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!(
                "{} planned {} items ({sources})",
                state.status.command,
                state.status.total
            );
        });
    }

    pub(super) fn started(&self, source: &str, session: &str, workday: &str, kind: ItemKind) {
        self.update(|state, _| {
            state.next_seq += 1;
            let item = CurrentItem {
                source: source.into(),
                session: session.into(),
                workday: workday.into(),
                kind,
            };
            state
                .in_flight
                .insert((source.into(), session.into()), (state.next_seq, item));
        });
    }

    pub(super) fn settled(&self, source: &str, session: &str, ok: bool) {
        self.update(|state, now| {
            state.in_flight.remove(&(source.into(), session.into()));
            state.settle(source, session, 1, ok, now);
        });
    }

    /// Settle whatever the Session left unsettled: failed when the Session
    /// failed, done when it needed no further work.
    pub(super) fn session_finished(&self, source: &str, session: &str, ok: bool) {
        self.update(|state, now| {
            let key = (source.to_string(), session.to_string());
            state.in_flight.remove(&key);
            let Some((planned, settled)) = state.sessions.get(&key).copied() else {
                return;
            };
            state.settle(source, session, planned - settled, ok, now);
        });
    }

    pub(super) fn rejected(&self) {
        self.update(|state, _| state.status.rejected += 1);
    }

    /// Run `work` while a reporter logs and heartbeats every interval.
    pub(super) fn while_running<R>(&self, work: impl FnOnce() -> R) -> R {
        if self.tracked.is_none() {
            return work();
        }
        let (stop, stopped) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(self.interval) {
                    self.update(|state, now| {
                        tracing::info!(
                            "{}",
                            report::progress_line(&state.status, now - state.started_ms, true)
                        );
                    });
                }
            });
            let result = work();
            drop(stop);
            result
        })
    }

    /// Record the run's outcome, leave it in the status file, and log the
    /// final progress line.
    pub(super) fn finish<T>(&self, result: &Result<T>) -> Result<()> {
        let Some((writer, state)) = &self.tracked else {
            return Ok(());
        };
        let mut state = state.lock().expect("Digest progress poisoned");
        let now = crate::adapters::now_ms();
        state.in_flight.clear();
        state.status.state = match result {
            Ok(_) => RunState::Completed,
            Err(_) => RunState::Failed,
        };
        state.status.error = result.as_ref().err().map(|error| format!("{error:#}"));
        state.refresh(now);
        writer.write(&state.status)?;
        tracing::info!(
            "{}",
            report::progress_line(&state.status, now - state.started_ms, false)
        );
        Ok(())
    }

    fn update(&self, change: impl FnOnce(&mut State, i64)) {
        let Some((writer, state)) = &self.tracked else {
            return;
        };
        let mut state = state.lock().expect("Digest progress poisoned");
        let now = crate::adapters::now_ms();
        change(&mut state, now);
        state.refresh(now);
        // Progress is diagnostic: a failed status write is reported loudly
        // but never aborts paid-for Digest work. `finish` writes strictly.
        if let Err(error) = writer.write(&state.status) {
            tracing::error!(error = format!("{error:#}"), "writing Digest status failed");
        }
    }
}

impl State {
    fn settle(&mut self, source: &str, session: &str, items: usize, ok: bool, now: i64) {
        let Some((planned, settled)) = self
            .sessions
            .get_mut(&(source.to_string(), session.to_string()))
        else {
            tracing::error!(source, session, "Digest progress for unplanned Session");
            return;
        };
        if *settled + items > *planned {
            tracing::error!(source, session, "Digest progress settled beyond its plan");
            return;
        }
        *settled += items;
        let failed = if ok { 0 } else { items };
        self.status.done += items;
        self.status.failed += failed;
        let per_source = self.status.sources.entry(source.into()).or_default();
        per_source.done += items;
        per_source.failed += failed;
        self.rate.record(now, self.status.done);
    }

    fn refresh(&mut self, now: i64) {
        let status = &mut self.status;
        status.updated_at = crate::timefmt::iso_ms(now);
        status.in_flight = self.in_flight.len();
        status.current = self
            .in_flight
            .values()
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, item)| item.clone());
        if status.state != RunState::Running {
            status.current = None;
            (status.rate_per_hour, status.eta_seconds, status.eta_at) = (None, None, None);
            return;
        }
        let per_hour = self.rate.per_hour(now, status.done);
        let eta_ms = rate::eta_ms(status.total - status.done, per_hour);
        status.rate_per_hour = per_hour.map(|rate| (rate * 100.0).round() / 100.0);
        status.eta_seconds = eta_ms.map(|ms| (ms / 1000) as u64);
        status.eta_at = eta_ms.map(|ms| crate::timefmt::iso_ms(now + ms));
    }
}

/// Report the latest run through `digest status`.
pub fn report_for(archive_root: &Path) -> Result<(String, bool)> {
    let report = inspect(archive_root)?;
    let healthy = report.liveness != Liveness::Dead && report.status.state != RunState::Failed;
    Ok((render_report(&report, crate::adapters::now_ms())?, healthy))
}
