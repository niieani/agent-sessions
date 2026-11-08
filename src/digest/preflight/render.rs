//! The pre-flight report: summary and per-Source text, JSON, and the warning
//! raised when a run would regenerate many existing Digests.

use std::fmt::Write as _;

use anyhow::Result;
use serde_json::{Value, json};

use super::{LastRun, Pace, PlanSummary, WorkCounts, last_run_pace};
use crate::archive::Archive;
use crate::digest::progress::format_duration;

/// Regenerating more than this share of existing Day Digests warns.
pub const DEFAULT_REDO_WARN_FRACTION: f64 = 0.05;

#[derive(Debug, Clone)]
pub struct Preflight {
    pub plan: PlanSummary,
    pub total: WorkCounts,
    pub pace: Pace,
    pub redo_warn_fraction: f64,
    pub warning: Option<String>,
}

/// Judge a dry-run's plan against the Archive's last completed run.
pub fn preflight(
    archive: &Archive,
    plan: &PlanSummary,
    redo_warn_fraction: f64,
) -> Result<Preflight> {
    anyhow::ensure!(
        redo_warn_fraction.is_finite() && (0.0..=1.0).contains(&redo_warn_fraction),
        "redo-warn-fraction must be between 0 and 1, got {redo_warn_fraction}"
    );
    let total = plan.total();
    let pace = last_run_pace(archive.root())?;
    let warning = warning(&total, redo_warn_fraction);
    Ok(Preflight {
        plan: plan.clone(),
        total,
        pace,
        redo_warn_fraction,
        warning,
    })
}

fn warning(total: &WorkCounts, fraction: f64) -> Option<String> {
    let redo = total.redo_days();
    if redo == 0 {
        return None;
    }
    let mut conditions = Vec::new();
    if redo > total.new_days {
        conditions.push(format!("more than the {} new Session Days", total.new_days));
    }
    let existing = total.existing_day_digests;
    if redo as f64 > fraction * existing as f64 {
        conditions.push(format!(
            "{} of the {existing} existing Session Day Digests (threshold {})",
            percent(redo as f64 / existing.max(1) as f64),
            percent(fraction)
        ));
    }
    if conditions.is_empty() {
        return None;
    }
    let reasons = total
        .redo
        .iter()
        .filter(|(reason, _)| !reason.is_title())
        .map(|(reason, count)| format!("{} {count}", reason.label()))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{redo} existing Session Day Digests would be regenerated ({reasons}), {}; a release may be invalidating existing Digests",
        conditions.join(" and ")
    ))
}

fn percent(fraction: f64) -> String {
    format!("{}%", (fraction * 1000.0).round() / 10.0)
}

impl Preflight {
    /// Estimated run time at the last run's pace.
    pub fn duration_ms(&self) -> Option<i64> {
        match &self.pace {
            Pace::LastRun(run) => Some(self.duration_at(run)),
            Pace::Unknown { .. } => None,
        }
    }

    fn duration_at(&self, run: &LastRun) -> i64 {
        (self.total.items() as f64 / run.per_hour * 3_600_000.0).round() as i64
    }

    /// `digest dry-run: 412 new · 37 redo (dialog changed 37) · 1180 fingerprints · ~3h10m at last run's pace (chatgpt 400/10, codex 12/27)`
    pub fn summary_line(&self) -> String {
        let total = &self.total;
        let mut line = format!("digest dry-run: {}", counts_text(total));
        if total.items() == 0 {
            line.push_str(" · no model work");
        } else {
            match &self.pace {
                Pace::LastRun(run) => write!(
                    line,
                    " · ~{} at last run's pace",
                    format_duration(self.duration_at(run))
                ),
                Pace::Unknown { reason } => write!(
                    line,
                    " · no rate history ({reason}); {} input tokens",
                    total.input_tokens
                ),
            }
            .expect("writing to String");
        }
        let sources = self
            .plan
            .sources
            .iter()
            .filter(|(_, counts)| counts.items() > 0)
            .map(|(source, counts)| {
                format!("{source} {}/{}", counts.new_items(), counts.redo_items())
            })
            .collect::<Vec<_>>();
        if !sources.is_empty() {
            write!(line, " ({})", sources.join(", ")).expect("writing to String");
        }
        line
    }

    /// One line per Source with planned work or fingerprints.
    pub fn source_lines(&self) -> Vec<String> {
        self.plan
            .sources
            .iter()
            .filter(|(_, counts)| counts.items() > 0 || counts.fingerprints > 0)
            .map(|(source, counts)| {
                format!(
                    "dry-run {source}: {} · {} input tokens",
                    counts_text(counts),
                    counts.input_tokens
                )
            })
            .collect()
    }

    pub fn to_json(&self) -> Value {
        let mut summary = counts_json(&self.total);
        let object = summary.as_object_mut().expect("counts are an object");
        object.insert(
            "sources".into(),
            self.plan
                .sources
                .iter()
                .map(|(source, counts)| (source.clone(), counts_json(counts)))
                .collect(),
        );
        let items = self.total.items();
        let estimate = match &self.pace {
            Pace::LastRun(run) => json!({
                "basis": "last_run",
                "items": items,
                "duration_seconds": self.duration_ms().map(|ms| ms / 1000),
                "rate_per_hour": (run.per_hour * 100.0).round() / 100.0,
                "run": {
                    "command": run.command,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "done": run.done,
                },
            }),
            Pace::Unknown { reason } => json!({
                "basis": "token_count",
                "items": items,
                "duration_seconds": null,
                "reason": reason,
                "input_tokens": self.total.input_tokens,
            }),
        };
        object.insert("estimate".into(), estimate);
        object.insert("redo_warn_fraction".into(), self.redo_warn_fraction.into());
        object.insert("warning".into(), self.warning.clone().into());
        summary
    }
}

/// `412 new · 37 redo (dialog changed 37) · 1180 fingerprints`
fn counts_text(counts: &WorkCounts) -> String {
    let mut text = format!("{} new · {} redo", counts.new_items(), counts.redo_items());
    if !counts.redo.is_empty() {
        let reasons = counts
            .redo
            .iter()
            .map(|(reason, count)| format!("{} {count}", reason.label()))
            .collect::<Vec<_>>()
            .join(", ");
        write!(text, " ({reasons})").expect("writing to String");
    }
    write!(text, " · {} fingerprints", counts.fingerprints).expect("writing to String");
    text
}

fn counts_json(counts: &WorkCounts) -> Value {
    json!({
        "new": counts.new_items(),
        "new_days": counts.new_days,
        "new_titles": counts.new_titles,
        "redo": counts.redo_items(),
        "redo_days": counts.redo_days(),
        "redo_reasons": counts.redo,
        "fingerprints": counts.fingerprints,
        "existing_day_digests": counts.existing_day_digests,
        "input_tokens": counts.input_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::super::RedoReason;
    use super::*;

    fn counts(new_days: usize, redo: &[(RedoReason, usize)], existing: usize) -> WorkCounts {
        WorkCounts {
            new_days,
            redo: redo.iter().copied().collect(),
            existing_day_digests: existing,
            ..WorkCounts::default()
        }
    }

    #[test]
    fn titles_never_trigger_the_warning() {
        let total = counts(0, &[(RedoReason::TitleRefresh, 40)], 100);
        assert_eq!(warning(&total, 0.05), None);
    }

    #[test]
    fn redo_within_new_work_and_fraction_is_quiet() {
        let total = counts(10, &[(RedoReason::RangeGrew, 2)], 100);
        assert_eq!(warning(&total, 0.05), None);
    }

    #[test]
    fn warning_names_every_condition_met() {
        let total = counts(1, &[(RedoReason::DialogChanged, 37)], 400);
        let text = warning(&total, 0.05).unwrap();
        assert!(text.starts_with(
            "37 existing Session Day Digests would be regenerated (dialog changed 37)"
        ));
        assert!(text.contains("more than the 1 new Session Days"));
        assert!(text.contains("9.3% of the 400 existing Session Day Digests (threshold 5%)"));
    }

    #[test]
    fn an_empty_plan_reports_no_model_work() {
        let preflight = Preflight {
            plan: PlanSummary::default(),
            total: WorkCounts::default(),
            pace: Pace::Unknown {
                reason: "no digest-status.json".into(),
            },
            redo_warn_fraction: DEFAULT_REDO_WARN_FRACTION,
            warning: None,
        };
        assert_eq!(
            preflight.summary_line(),
            "digest dry-run: 0 new · 0 redo · 0 fingerprints · no model work"
        );
    }
}
