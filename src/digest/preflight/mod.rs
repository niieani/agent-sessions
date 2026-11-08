//! Pre-flight view of a planned digest run: which model work is new, which
//! regenerates existing Digests and why, and how long it should take.
//!
//! A dry-run before a nightly run (especially right after an upgrade) uses
//! this to spot a release that invalidates existing Digests before paying
//! for the regeneration.

mod pace;
mod render;

use std::collections::BTreeMap;

use serde::Serialize;

pub use pace::{LastRun, Pace, last_run_pace};
pub use render::{DEFAULT_REDO_WARN_FRACTION, Preflight, preflight};

use super::backfill::Backfill;
use super::{DayPlan, SessionPlan, retry};

/// Why an existing Digest is regenerated through the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedoReason {
    /// The Session Day gained Messages after its Digest was written.
    RangeGrew,
    /// The Session Day's Messages sit at other indices: earlier Dialog
    /// changed, or the day lost Messages.
    RangeMoved,
    /// Same covered range, different Dialog fingerprint.
    DialogChanged,
    /// `--force` regenerates a current Digest.
    Forced,
    /// The Session title follows its regenerated or new Session Days.
    TitleRefresh,
    /// The stored Session title is invalid.
    InvalidTitle,
}

impl RedoReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::RangeGrew => "range grew",
            Self::RangeMoved => "range moved",
            Self::DialogChanged => "dialog changed",
            Self::Forced => "forced",
            Self::TitleRefresh => "title refresh",
            Self::InvalidTitle => "invalid title",
        }
    }

    pub fn is_title(self) -> bool {
        matches!(self, Self::TitleRefresh | Self::InvalidTitle)
    }
}

/// Planned work of one Source, or of the whole run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkCounts {
    /// Session Days without a Digest.
    pub new_days: usize,
    /// Session titles without a Session Digest.
    pub new_titles: usize,
    /// Existing Day and Session Digests regenerated, by reason.
    pub redo: BTreeMap<RedoReason, usize>,
    /// Fingerprint-less Digests adopted without a model call.
    pub fingerprints: usize,
    /// Day Digests that already exist for the selected Workdays.
    pub existing_day_digests: usize,
    pub input_tokens: usize,
}

impl WorkCounts {
    pub fn new_items(&self) -> usize {
        self.new_days + self.new_titles
    }

    pub fn redo_items(&self) -> usize {
        self.redo.values().sum()
    }

    /// Regenerated Session Days, excluding titles.
    pub fn redo_days(&self) -> usize {
        self.redo
            .iter()
            .filter(|(reason, _)| !reason.is_title())
            .map(|(_, count)| count)
            .sum()
    }

    /// Model items, counted as `digest-status.json` counts them.
    pub fn items(&self) -> usize {
        self.new_items() + self.redo_items()
    }

    fn add(&mut self, other: &Self) {
        self.new_days += other.new_days;
        self.new_titles += other.new_titles;
        for (reason, count) in &other.redo {
            *self.redo.entry(*reason).or_default() += count;
        }
        self.fingerprints += other.fingerprints;
        self.existing_day_digests += other.existing_day_digests;
        self.input_tokens += other.input_tokens;
    }
}

/// Planned work of a digest run, by Source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSummary {
    pub sources: BTreeMap<String, WorkCounts>,
}

impl PlanSummary {
    pub fn total(&self) -> WorkCounts {
        let mut total = WorkCounts::default();
        for counts in self.sources.values() {
            total.add(counts);
        }
        total
    }

    /// Classify every planned item, backfill, and existing Day Digest.
    pub(super) fn of(
        plans: &[SessionPlan],
        backfills: &[Backfill],
        existing_days: &BTreeMap<String, usize>,
    ) -> Self {
        let mut summary = Self::default();
        for (source, count) in existing_days {
            summary.source(source).existing_day_digests += count;
        }
        for backfill in backfills {
            summary.source(backfill.source()).fingerprints += 1;
        }
        for plan in plans {
            let counts = summary.source(&plan.session.source);
            let mut stale = 0;
            for day in plan.days.iter().filter(|day| day.stale) {
                stale += 1;
                match day_reason(day) {
                    None => counts.new_days += 1,
                    Some(reason) => *counts.redo.entry(reason).or_default() += 1,
                }
            }
            if stale == 0 && !plan.refresh_session {
                continue;
            }
            match &plan.existing_session {
                None => counts.new_titles += 1,
                Some(session) => {
                    let reason =
                        if plan.refresh_session && !retry::valid_session_title(&session.title) {
                            RedoReason::InvalidTitle
                        } else {
                            RedoReason::TitleRefresh
                        };
                    *counts.redo.entry(reason).or_default() += 1;
                }
            }
        }
        summary
    }

    /// Attribute the dry-run's measured input token counts to their Sources.
    pub(super) fn record_tokens(&mut self, counts: &BTreeMap<String, usize>) {
        for (source, tokens) in counts {
            self.source(source).input_tokens += tokens;
        }
    }

    fn source(&mut self, source: &str) -> &mut WorkCounts {
        self.sources.entry(source.to_string()).or_default()
    }
}

/// `None` for a Session Day without a Digest.
fn day_reason(day: &DayPlan) -> Option<RedoReason> {
    let existing = day.existing.as_ref()?;
    let covered = &existing.covered;
    Some(
        if (covered.first_idx, covered.last_idx) != (day.first_idx, day.last_idx) {
            if covered.first_idx == day.first_idx && covered.last_idx < day.last_idx {
                RedoReason::RangeGrew
            } else {
                RedoReason::RangeMoved
            }
        } else if existing.dialog_fingerprint.as_deref() != Some(day.dialog_fingerprint.as_str()) {
            RedoReason::DialogChanged
        } else {
            RedoReason::Forced
        },
    )
}
