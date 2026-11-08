//! Timestamp parsing and formatting shared across Adapters and Render.
//!
//! Everything internal is epoch milliseconds UTC (`i64`); strings exist only
//! at the edges (native records in, Transcript frontmatter and filenames
//! out).

use anyhow::{Context, Result};
use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

const FRONTMATTER: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
const FILENAME: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour][minute][second]Z");
const MILLIS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
const DATE_ONLY: &[BorrowedFormatItem<'_>] = format_description!("[year]-[month]-[day]");

/// Parse an ISO 8601 / RFC 3339 timestamp (with or without fractional
/// seconds) to epoch milliseconds UTC.
pub fn parse_iso_ms(s: &str) -> Result<i64> {
    let dt = OffsetDateTime::parse(s, &Rfc3339).with_context(|| format!("bad timestamp: {s}"))?;
    Ok((dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn utc(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .expect("epoch ms out of range")
}

/// `2026-08-20T21:45:20Z` — second precision, for frontmatter.
pub fn frontmatter(ms: i64) -> String {
    utc(ms).format(FRONTMATTER).expect("formatting UTC time")
}

/// `2026-08-20T21:45:20.123Z` — millisecond precision, for heartbeats.
pub fn iso_ms(ms: i64) -> String {
    utc(ms).format(MILLIS).expect("formatting UTC time")
}

/// `2026-08-20T214520Z` — filesystem-safe, sortable, for Transcript filenames.
pub fn filename(ms: i64) -> String {
    utc(ms).format(FILENAME).expect("formatting UTC time")
}

/// `2026-08-20` — for index rows and day-change detection.
pub fn date_only(ms: i64) -> String {
    utc(ms).format(DATE_ONLY).expect("formatting UTC time")
}

/// Parse a configuration duration: an integer followed by `ms`, `s`, `m`,
/// `h`, or `d` (e.g. `10m`, `7d`).
pub fn parse_duration(value: &str) -> Result<std::time::Duration> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .context("missing duration unit")?;
    let amount = value[..split].parse::<u64>()?;
    let unit_ms: u64 = match &value[split..] {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => anyhow::bail!("unknown duration unit"),
    };
    let millis = amount.checked_mul(unit_ms).context("duration overflow")?;
    Ok(std::time::Duration::from_millis(millis))
}

/// A coarse human age with its two largest units, omitting a zero second
/// unit: `8d 2h`, `7d`, `5h 12m`, `37m`.
pub fn format_elapsed(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    let (days, hours, minutes) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    let (major, minor) = match (days, hours) {
        (0, 0) => return format!("{minutes}m"),
        (0, _) => (
            format!("{hours}h"),
            (minutes > 0).then(|| format!("{minutes}m")),
        ),
        _ => (format!("{days}d"), (hours > 0).then(|| format!("{hours}h"))),
    };
    match minor {
        Some(minor) => format!("{major} {minor}"),
        None => major,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_day_unit() {
        assert_eq!(parse_duration("7d").unwrap().as_secs(), 7 * 86_400);
        assert_eq!(parse_duration("10m").unwrap().as_secs(), 600);
        assert_eq!(parse_duration("0d").unwrap().as_secs(), 0);
        assert!(parse_duration("7").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("1w").is_err());
    }

    #[test]
    fn elapsed_uses_two_largest_units() {
        assert_eq!(
            format_elapsed(8 * 86_400_000 + 2 * 3_600_000 + 59_000),
            "8d 2h"
        );
        assert_eq!(format_elapsed(5 * 3_600_000 + 12 * 60_000), "5h 12m");
        assert_eq!(format_elapsed(37 * 60_000), "37m");
        assert_eq!(format_elapsed(7 * 86_400_000), "7d");
        assert_eq!(format_elapsed(3 * 3_600_000), "3h");
        assert_eq!(format_elapsed(-5), "0m");
    }

    #[test]
    fn parses_iso_with_millis() {
        assert_eq!(
            parse_iso_ms("2026-08-20T21:45:20.051Z").unwrap(),
            1787262320051
        );
        assert_eq!(parse_iso_ms("2026-08-20T21:45:20Z").unwrap(), 1787262320000);
    }

    #[test]
    fn formats_round_trip() {
        let ms = parse_iso_ms("2026-08-20T21:45:20.051Z").unwrap();
        assert_eq!(frontmatter(ms), "2026-08-20T21:45:20Z");
        assert_eq!(filename(ms), "2026-08-20T214520Z");
        assert_eq!(date_only(ms), "2026-08-20");
    }
}
