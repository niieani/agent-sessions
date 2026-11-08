//! Workday classification: the configurable "day of work" shared by Render
//! and activity queries (SPEC "Workdays"; CONTEXT.md "Workday").
//!
//! A Workday is a local date in an IANA timezone, beginning at a configured
//! day-start time rather than necessarily at midnight. A user Message starts
//! a turn and owns the Workday its timestamp falls into; following assistant
//! Messages inherit that turn's Workday even when they cross the boundary. A
//! leading assistant Message (no preceding user Message) takes the Workday of
//! its own timestamp. All classification is a pure function of Dialog Message
//! timestamps plus config — never of Capture file times.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, PrimitiveDateTime, Time};
use time_tz::{OffsetDateTimeExt, PrimitiveDateTimeExt, TimeZone, Tz, timezones};

use crate::model::{Message, Role};

/// One validated Workday configuration: timezone plus day-start time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkdayConfig {
    timezone_name: String,
    timezone: &'static Tz,
    day_start: Time,
}

/// The deserialized `[views]` Workday keys (kebab-case in config.toml).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WorkdaySettings {
    /// IANA timezone name (e.g. "America/Los_Angeles"). Absent = the central
    /// host's local zone.
    pub timezone: Option<String>,
    /// Local time the Workday begins, "HH:MM". Absent = midnight.
    pub day_start: Option<String>,
}

impl WorkdayConfig {
    /// Resolve settings into a validated clock. An unknown timezone or a
    /// malformed day-start is a hard config error.
    pub fn new(settings: &WorkdaySettings) -> Result<Self> {
        let timezone_name = match &settings.timezone {
            Some(name) => name.clone(),
            None => local_timezone_name()?,
        };
        let timezone = timezones::get_by_name(&timezone_name)
            .with_context(|| format!("unknown IANA timezone {timezone_name:?}"))?;
        let day_start = match &settings.day_start {
            Some(value) => {
                parse_day_start(value).with_context(|| format!("invalid day-start {value:?}"))?
            }
            None => Time::MIDNIGHT,
        };
        Ok(Self {
            timezone_name,
            timezone,
            day_start,
        })
    }

    /// The configured timezone's IANA name (diagnostics).
    pub fn timezone_name(&self) -> &str {
        &self.timezone_name
    }

    /// The Workday (`YYYY-MM-DD`) owning one timestamp.
    pub fn workday_of(&self, timestamp_ms: i64) -> String {
        let local = to_local(self.timezone, timestamp_ms);
        let date = if local.time() < self.day_start {
            date_minus_one(local.date())
        } else {
            local.date()
        };
        format_date(date)
    }

    /// RFC 3339 timestamp in the configured Workday timezone, at second
    /// precision, for Session Day template metadata.
    pub fn format_timestamp(&self, timestamp_ms: i64) -> String {
        OffsetDateTime::from_unix_timestamp_nanos(timestamp_ms as i128 * 1_000_000)
            .expect("epoch ms out of range")
            .to_timezone(self.timezone)
            .replace_nanosecond(0)
            .expect("zero nanoseconds are valid")
            .format(&Rfc3339)
            .expect("formatting Workday timestamp")
    }

    /// The UTC epoch-ms window `[start, end)` covered by one Workday.
    pub fn day_bounds(&self, workday: &str) -> Result<(i64, i64)> {
        let date = parse_date(workday).with_context(|| format!("invalid workday {workday:?}"))?;
        let start_local = PrimitiveDateTime::new(date, self.day_start);
        let end_local = PrimitiveDateTime::new(date_plus_one(date), self.day_start);
        Ok((
            local_to_utc(self.timezone, start_local),
            local_to_utc(self.timezone, end_local),
        ))
    }
}

/// The Workday of each Message, positionally aligned with the input slice.
/// `None` entries correspond to user or Agent Messages without timestamps, or
/// to assistant Messages before a timestamped user or Agent turn when they
/// also lack one.
///
/// Turn attribution: a user or Agent Message owns the Workday of its own
/// timestamp; assistant Messages inherit the Workday of the most recent
/// timestamped such Message even when the assistant timestamp is absent. An
/// untimestamped user or Agent Message explicitly ends that inheritance; its
/// turn cannot be attributed. Assistant Messages before the first attributable
/// user or Agent Message take their own.
pub fn message_workdays(config: &WorkdayConfig, messages: &[Message]) -> Vec<Option<String>> {
    enum Turn {
        None,
        Attributed(String),
        Unattributable,
    }
    let mut current = Turn::None;
    messages
        .iter()
        .map(|message| match message.role {
            Role::User | Role::Agent => {
                let Some(ts) = message.timestamp_ms else {
                    current = Turn::Unattributable;
                    return None;
                };
                let day = config.workday_of(ts);
                current = Turn::Attributed(day.clone());
                Some(day)
            }
            Role::Assistant => match &current {
                Turn::Attributed(day) => Some(day.clone()),
                Turn::Unattributable => None,
                Turn::None => message.timestamp_ms.map(|ts| config.workday_of(ts)),
            },
        })
        .collect()
}

/// The sorted, deduplicated Workdays on which a Conversation saw Dialog.
pub fn activity_days(config: &WorkdayConfig, messages: &[Message]) -> Vec<String> {
    let mut days: Vec<String> = message_workdays(config, messages)
        .into_iter()
        .flatten()
        .collect();
    days.sort();
    days.dedup();
    days
}

fn to_local(tz: &'static Tz, timestamp_ms: i64) -> PrimitiveDateTime {
    let utc = OffsetDateTime::from_unix_timestamp_nanos(timestamp_ms as i128 * 1_000_000)
        .expect("epoch ms out of range");
    let local = utc.to_timezone(tz);
    PrimitiveDateTime::new(local.date(), local.time())
}

fn local_to_utc(tz: &'static Tz, local: PrimitiveDateTime) -> i64 {
    // Fold (fall back): the local time occurs twice; the first occurrence
    // starts the Workday. Gap (spring forward): the local time never occurs;
    // resolving against the offset in effect at that instant shifts the
    // boundary past the gap (e.g. a 02:30 day-start lands at 03:30).
    let dt = match local.assume_timezone(tz) {
        time_tz::OffsetResult::Some(dt) => dt,
        time_tz::OffsetResult::Ambiguous(first, _second) => first,
        time_tz::OffsetResult::None => local.assume_timezone_utc(tz),
    };
    (dt.unix_timestamp_nanos() / 1_000_000) as i64
}

fn parse_day_start(value: &str) -> Result<Time> {
    let mut parts = value.split(':');
    let (Some(hour), Some(minute), None) = (parts.next(), parts.next(), parts.next()) else {
        bail!("expected HH:MM");
    };
    if hour.len() != 2 || minute.len() != 2 {
        bail!("expected zero-padded HH:MM");
    }
    let hour: u8 = hour.parse().context("hour")?;
    let minute: u8 = minute.parse().context("minute")?;
    Time::from_hms(hour, minute, 0).context("time out of range")
}

fn parse_date(value: &str) -> Result<time::Date> {
    let mut parts = value.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("expected YYYY-MM-DD");
    };
    let year: i32 = year.parse().context("year")?;
    let month: u8 = month.parse().context("month")?;
    let day: u8 = day.parse().context("day")?;
    time::Date::from_calendar_date(year, time::Month::try_from(month)?, day)
        .context("date out of range")
}

fn date_minus_one(date: time::Date) -> time::Date {
    date.previous_day().expect("date out of range")
}

fn date_plus_one(date: time::Date) -> time::Date {
    date.next_day().expect("date out of range")
}

fn format_date(date: time::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn local_timezone_name() -> Result<String> {
    let tz = time_tz::system::get_timezone()
        .context("detecting the local timezone; set [views] timezone explicitly")?;
    Ok(tz.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(tz: &str, day_start: Option<&str>) -> WorkdayConfig {
        WorkdayConfig::new(&WorkdaySettings {
            timezone: Some(tz.into()),
            day_start: day_start.map(str::to_string),
        })
        .unwrap()
    }

    fn ms(iso: &str) -> i64 {
        crate::timefmt::parse_iso_ms(iso).unwrap()
    }

    fn msg(role: Role, iso: &str) -> Message {
        Message {
            role,
            kind: crate::model::MessageKind::Dialog,
            text: "x".into(),
            timestamp_ms: Some(ms(iso)),
            actions_before: 0,
        }
    }

    #[test]
    fn midnight_default_uses_local_date() {
        // 2026-08-31 06:00 UTC = 2026-08-30 23:00 in Los Angeles (PDT).
        let config = config("America/Los_Angeles", None);
        assert_eq!(config.workday_of(ms("2026-08-31T06:00:00Z")), "2026-08-30");
        assert_eq!(config.workday_of(ms("2026-08-31T07:00:00Z")), "2026-08-31");
    }

    #[test]
    fn day_start_rolls_early_hours_into_previous_workday() {
        let config = config("America/Los_Angeles", Some("04:00"));
        // 2026-08-31 08:00 UTC = 01:00 local → still the previous Workday.
        assert_eq!(config.workday_of(ms("2026-08-31T08:00:00Z")), "2026-08-30");
        // 11:00 UTC = 04:00 local → the new Workday begins.
        assert_eq!(config.workday_of(ms("2026-08-31T11:00:00Z")), "2026-08-31");
    }

    #[test]
    fn utc_config_matches_plain_dates() {
        let config = config("UTC", None);
        assert_eq!(config.workday_of(ms("2026-08-31T23:59:59Z")), "2026-08-31");
        assert_eq!(config.workday_of(ms("2026-09-01T00:00:00Z")), "2026-09-01");
    }

    #[test]
    fn day_timestamp_formats_in_the_workday_timezone() {
        let config = config("America/Los_Angeles", Some("04:00"));
        assert_eq!(
            config.format_timestamp(ms("2026-08-31T10:00:00Z")),
            "2026-08-31T03:00:00-07:00"
        );
    }

    #[test]
    fn assistant_inherits_user_turn_workday_across_midnight() {
        let config = config("UTC", None);
        let messages = vec![
            msg(Role::User, "2026-08-31T23:50:00Z"),
            msg(Role::Assistant, "2026-09-01T00:10:00Z"),
        ];
        assert_eq!(
            activity_days(&config, &messages),
            vec!["2026-08-31".to_string()]
        );
    }

    #[test]
    fn assistant_inherits_agent_turn_workday_across_midnight() {
        let config = config("UTC", None);
        let messages = vec![
            msg(Role::Agent, "2026-08-31T23:50:00Z"),
            msg(Role::Assistant, "2026-09-01T00:10:00Z"),
        ];
        assert_eq!(
            activity_days(&config, &messages),
            vec!["2026-08-31".to_string()]
        );
    }

    #[test]
    fn new_user_message_starts_new_workday() {
        let config = config("UTC", None);
        let messages = vec![
            msg(Role::User, "2026-08-31T23:50:00Z"),
            msg(Role::Assistant, "2026-09-01T00:10:00Z"),
            msg(Role::User, "2026-09-01T09:00:00Z"),
            msg(Role::Assistant, "2026-09-01T09:05:00Z"),
        ];
        assert_eq!(
            activity_days(&config, &messages),
            vec!["2026-08-31".to_string(), "2026-09-01".to_string()]
        );
    }

    #[test]
    fn untimestamped_assistant_stays_with_active_turn() {
        let config = config("UTC", None);
        let mut assistant = msg(Role::Assistant, "2026-09-01T00:10:00Z");
        assistant.timestamp_ms = None;
        let messages = vec![msg(Role::User, "2026-08-31T23:50:00Z"), assistant];
        assert_eq!(
            message_workdays(&config, &messages),
            vec![Some("2026-08-31".into()), Some("2026-08-31".into())]
        );
    }

    #[test]
    fn untimestamped_user_ends_turn_attribution() {
        let config = config("UTC", None);
        let mut user = msg(Role::User, "2026-09-01T00:00:00Z");
        user.timestamp_ms = None;
        let mut assistant = msg(Role::Assistant, "2026-09-01T00:01:00Z");
        assistant.timestamp_ms = None;
        let messages = vec![msg(Role::User, "2026-08-31T23:50:00Z"), user, assistant];
        assert_eq!(
            message_workdays(&config, &messages),
            vec![Some("2026-08-31".into()), None, None]
        );
    }

    #[test]
    fn timestamped_assistant_cannot_detach_from_untimestamped_user() {
        let config = config("UTC", None);
        let mut user = msg(Role::User, "2026-09-01T00:00:00Z");
        user.timestamp_ms = None;
        let messages = vec![user, msg(Role::Assistant, "2026-09-01T00:01:00Z")];
        assert_eq!(message_workdays(&config, &messages), vec![None, None]);
    }

    #[test]
    fn leading_assistant_takes_its_own_workday() {
        let config = config("UTC", None);
        let messages = vec![
            msg(Role::Assistant, "2026-08-31T23:55:00Z"),
            msg(Role::User, "2026-09-01T00:05:00Z"),
        ];
        assert_eq!(
            activity_days(&config, &messages),
            vec!["2026-08-31".to_string(), "2026-09-01".to_string()]
        );
    }

    #[test]
    fn untimestamped_messages_are_skipped() {
        let config = config("UTC", None);
        let mut messages = vec![msg(Role::User, "2026-08-31T10:00:00Z")];
        messages[0].timestamp_ms = None;
        assert!(activity_days(&config, &messages).is_empty());
    }

    #[test]
    fn day_bounds_respect_day_start_and_dst() {
        let with_start = config("America/Los_Angeles", Some("04:00"));
        let (start, end) = with_start.day_bounds("2026-08-31").unwrap();
        assert_eq!(start, ms("2026-08-31T11:00:00Z")); // 04:00 PDT
        assert_eq!(end, ms("2026-09-01T11:00:00Z"));

        // Spring forward 2026-03-08: 02:00–03:00 never occurs; the Workday
        // is 23 hours long.
        let config = config("America/Los_Angeles", None);
        let (start, end) = config.day_bounds("2026-03-08").unwrap();
        assert_eq!(start, ms("2026-03-08T08:00:00Z"));
        assert_eq!(end, ms("2026-03-09T07:00:00Z"));
        assert_eq!(end - start, 23 * 3_600_000);

        // Fall back 2026-11-01: the Workday is 25 hours long.
        let (start, end) = config.day_bounds("2026-11-01").unwrap();
        assert_eq!(start, ms("2026-11-01T07:00:00Z"));
        assert_eq!(end, ms("2026-11-02T08:00:00Z"));
        assert_eq!(end - start, 25 * 3_600_000);
    }

    #[test]
    fn config_validation_is_strict() {
        assert!(
            WorkdayConfig::new(&WorkdaySettings {
                timezone: Some("Not/AZone".into()),
                day_start: None,
            })
            .is_err()
        );
        for bad in ["24:00", "12:60", "noon", "4:00", "04:00:00"] {
            assert!(
                WorkdayConfig::new(&WorkdaySettings {
                    timezone: Some("UTC".into()),
                    day_start: Some(bad.into()),
                })
                .is_err(),
                "{bad} must be rejected"
            );
        }
    }
}
