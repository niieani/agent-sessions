//! Slice 14 — Workdays: the `activity` query, `activityDays` frontmatter,
//! and attribute-based `<gap … />` markers agree with each other and with
//! the configured day-start rollover. All against TEMP Archives built from
//! fixtures.

mod common;

use std::fs;

use agent_sessions::activity::sessions_active_on;
use agent_sessions::archive::Archive;
use agent_sessions::render::render_all;

use common::{pin_utc_workday, pull, temp_store};

/// The claude `webapp` fixture crosses UTC midnight: 2026-03-06T22:01 →
/// 2026-03-07T00:31.
const WEBAPP_ID: &str = "22222222-2222-4222-8222-222222222222";
const EXAMPLE_ID: &str = "11111111-1111-4111-8111-111111111111";

fn pulled_utc_archive() -> (tempfile::TempDir, tempfile::TempDir) {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pin_utc_workday(archive_dir.path());
    // These archival/workday fixtures deliberately use nonexistent workspaces;
    // pin their Project membership independently of repository lookup.
    use std::io::Write as _;
    writeln!(fs::OpenOptions::new().append(true).open(archive_dir.path().join("config.toml")).unwrap(), "[views.project-names]\n\"/workspace/Projects/example\" = \"example\"\n\"/workspace/Projects/webapp\" = \"webapp\"").unwrap();
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_captured, 4);
    (store, archive_dir)
}

fn active_ids(archive: &Archive, day: &str) -> Vec<String> {
    sessions_active_on(archive, day)
        .unwrap()
        .iter()
        .map(|s| s.native_id.clone())
        .collect()
}

#[test]
fn activity_query_matches_rendered_activity_days() {
    let (_store, archive_dir) = pulled_utc_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    render_all(&archive).unwrap();

    // The midnight-crossing Session is active on both UTC Workdays; the
    // single-day Session only on its own.
    let march_6 = active_ids(&archive, "2026-03-06");
    assert!(march_6.contains(&WEBAPP_ID.to_string()), "{march_6:?}");
    assert!(!march_6.contains(&EXAMPLE_ID.to_string()), "{march_6:?}");
    let march_7 = active_ids(&archive, "2026-03-07");
    assert!(march_7.contains(&WEBAPP_ID.to_string()), "{march_7:?}");
    assert!(active_ids(&archive, "2026-03-05").contains(&EXAMPLE_ID.to_string()));

    // The rendered frontmatter says the same thing the query does.
    let views = archive_dir.path().join("views/projects");
    let webapp = fs::read_dir(views.join("webapp"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .find(|name| name.ends_with(".md") && name != "INDEX.md")
        .unwrap();
    let transcript = fs::read_to_string(views.join("webapp").join(webapp)).unwrap();
    assert!(
        transcript.contains("activityDays:\n  - 2026-03-06\n  - 2026-03-07\n"),
        "{transcript}"
    );
    assert!(
        transcript.contains("<gap workday=\"2026-03-07\" />"),
        "{transcript}"
    );

    // A day with no activity yields nothing, not an error.
    assert!(active_ids(&archive, "2026-03-09").is_empty());
}

#[test]
fn day_start_rolls_past_midnight_activity_into_previous_workday() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    fs::write(
        archive_dir.path().join("config.toml"),
        "[views]\ntimezone = \"UTC\"\nday-start = \"04:00\"\n",
    )
    .unwrap();
    pull(store.path(), archive_dir.path());
    use std::io::Write as _;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(archive_dir.path().join("config.toml"))
            .unwrap(),
        "[views.project-names]\n\"/workspace/Projects/webapp\" = \"webapp\""
    )
    .unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    render_all(&archive).unwrap();

    // With a 04:00 day-start, the 00:31 UTC turn belongs to 2026-03-06, so
    // the whole Session reports one Workday.
    let march_6 = active_ids(&archive, "2026-03-06");
    assert!(march_6.contains(&WEBAPP_ID.to_string()), "{march_6:?}");
    assert!(
        !active_ids(&archive, "2026-03-07").contains(&WEBAPP_ID.to_string()),
        "00:31 turn must roll into the previous Workday"
    );
    let views = archive_dir.path().join("views/projects");
    let webapp = fs::read_dir(views.join("webapp"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .find(|name| name.ends_with(".md") && name != "INDEX.md")
        .unwrap();
    let transcript = fs::read_to_string(views.join("webapp").join(webapp)).unwrap();
    assert!(
        transcript.contains("activityDays:\n  - 2026-03-06\n"),
        "{transcript}"
    );
    assert!(!transcript.contains("2026-03-07\n  -"), "{transcript}");
    // No Workday change remains, and the 2h28m gap is under the duration
    // threshold: no gap marker at all. (The fixture embeds an escaped
    // `\`-prefixed gap lookalike in message content; that must stay.)
    assert!(
        !transcript.lines().any(|line| line.starts_with("<gap")),
        "{transcript}"
    );
    assert!(transcript.contains("\\<gap>2 hours</gap>"), "{transcript}");
}

#[test]
fn invalid_workday_config_fails_before_touching_views() {
    let (_store, archive_dir) = pulled_utc_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    render_all(&archive).unwrap();
    let sentinel = archive_dir.path().join("views/projects/example/INDEX.md");
    let before = fs::read_to_string(&sentinel).unwrap();

    for bad in [
        "[views]\ntimezone = \"Not/AZone\"\n",
        "[views]\nday-start = \"25:00\"\n",
        "[views]\nday-start = \"4:00\"\n",
    ] {
        fs::write(archive_dir.path().join("config.toml"), bad).unwrap();
        assert!(render_all(&archive).is_err(), "{bad} must fail");
        assert!(
            sessions_active_on(&archive, "2026-03-06").is_err(),
            "{bad} must fail"
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), before);
    }
}

#[test]
fn invalid_day_argument_is_rejected() {
    let (_store, archive_dir) = pulled_utc_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    assert!(sessions_active_on(&archive, "2026-13-01").is_err());
    assert!(sessions_active_on(&archive, "not-a-day").is_err());
}
