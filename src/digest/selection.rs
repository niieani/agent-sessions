//! Narrowing a Digest run to one Session, and the preconditions of forcing
//! one of its Session Days.

use std::fmt;

use anyhow::{Context, Result};

use super::DayPlan;

/// A Session named on the command line as `SOURCE:NATIVE-ID`. The native id
/// is everything after the first colon, so opaque ids keep their colons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
    pub source: String,
    pub native_id: String,
}

impl SessionKey {
    pub fn parse(value: &str) -> Result<Self> {
        let (source, native_id) = value
            .split_once(':')
            .context("Session must be SOURCE:NATIVE-ID")?;
        anyhow::ensure!(
            !source.is_empty() && !native_id.is_empty(),
            "Session must be SOURCE:NATIVE-ID, got {value:?}"
        );
        Ok(Self {
            source: source.to_string(),
            native_id: native_id.to_string(),
        })
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.source, self.native_id)
    }
}

/// Mark `day` for regeneration. Every earlier Session Day must already have
/// a Digest covering exactly its current Messages: they supply the Topic,
/// Intent, and prior Digests the forced day is written against.
pub(super) fn force_day(session: &SessionKey, days: &mut [DayPlan], day: &str) -> Result<()> {
    let position = days
        .iter()
        .position(|plan| plan.workday == day)
        .with_context(|| {
            format!(
                "{session} has no closed Session Day {day}: it has no Dialog that Workday, \
                 or the day is the current Workday or still Ongoing"
            )
        })?;
    for earlier in &days[..position] {
        let current = earlier.existing.as_ref().is_some_and(|digest| {
            digest.covered.first_idx == earlier.first_idx
                && digest.covered.last_idx == earlier.last_idx
                && digest.dialog_fingerprint.as_deref() == Some(&earlier.dialog_fingerprint)
        });
        anyhow::ensure!(
            current,
            "earlier Session Day {} of {session} has no Digest covering its current Messages; \
             digest it first (`digest --session {session} --day {} [--force]`) so the Intent \
             chain reaches {day}",
            earlier.workday,
            earlier.workday,
        );
    }
    days[position].stale = true;
    Ok(())
}
