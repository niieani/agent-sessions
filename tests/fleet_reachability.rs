//! Unreachable Hosts: a Host whose first ssh contact fails at the connection
//! level is skipped without failing the Pull, until its last successful Pull
//! is older than `[pull] unreachable-grace`. Uses a fake ssh transport only.

mod common;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_sessions::archive::Archive;
use agent_sessions::fleet::{FleetConfig, FleetPull, FleetStats, pull_hosts, summary_lines};

use common::fake_remote::FakeRemote;
use common::temp_store;

const BIN: &str = env!("CARGO_BIN_EXE_agent-sessions");
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const TIMED_OUT: &str = "ssh: connect to host 192.0.2.10 port 22: Operation timed out";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn archive_with(config: &str) -> (tempfile::TempDir, Archive) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    (dir, archive)
}

const LAPTOP: &str = "[[host]]\nname = \"laptop\"\nssh = \"laptop\"\n";

fn pull(archive: &Archive, remote: &FakeRemote) -> FleetStats {
    let config = FleetConfig::load(archive.root()).unwrap().unwrap();
    pull_hosts(
        archive,
        &config.hosts,
        &FleetPull {
            transport: remote,
            local_binary: Path::new(BIN),
            full_source: None,
        },
    )
    .unwrap()
}

#[test]
fn unreachable_host_within_grace_is_skipped_without_failing() {
    let (_dir, archive) = archive_with(LAPTOP);
    let last_success = now_ms() - 2 * DAY_MS;
    archive
        .db()
        .unwrap()
        .record_host_pull_success("laptop", last_success)
        .unwrap();

    let remote = FakeRemote::failing_connection(TIMED_OUT);
    let fleet = pull(&archive, &remote);

    assert_eq!(fleet.hosts_failed, 0);
    assert!(fleet.failed_hosts.is_empty());
    assert!(!fleet.failed());
    assert_eq!(fleet.unreachable_hosts.len(), 1);
    let unreachable = &fleet.unreachable_hosts[0];
    assert_eq!(unreachable.host, "laptop");
    assert!(unreachable.detail.contains("Operation timed out"));
    assert_eq!(unreachable.last_success_ms, Some(last_success));
    assert!(unreachable.failure.is_none());
    assert!(
        fleet.totals.sources.iter().all(|row| row.host != "laptop"),
        "an Unreachable Host contributes no Source rows"
    );
    assert_eq!(remote.calls().len(), 1, "only the probe was attempted");

    let db = archive.db().unwrap();
    assert!(db.bookmark("laptop", "claude").unwrap().is_none());
    let record = db.host_pull_record("laptop").unwrap().unwrap();
    assert_eq!(record.last_success_ms, Some(last_success), "unchanged");

    let lines = summary_lines(&fleet, 1);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("laptop") && line.contains("unreachable")),
        "{lines:#?}"
    );
    assert!(
        lines.last().unwrap().contains("1 unreachable"),
        "{lines:#?}"
    );
}

#[test]
fn unreachable_host_past_grace_fails_naming_host_and_age() {
    let (_dir, archive) = archive_with(LAPTOP);
    archive
        .db()
        .unwrap()
        .record_host_pull_success("laptop", now_ms() - 8 * DAY_MS - 60_000)
        .unwrap();

    let fleet = pull(&archive, &FakeRemote::failing_connection(TIMED_OUT));

    assert!(fleet.failed());
    assert_eq!(fleet.hosts_failed, 1);
    assert_eq!(fleet.failed_hosts, vec!["laptop".to_string()]);
    let failure = fleet.unreachable_hosts[0].failure.as_deref().unwrap();
    assert!(failure.contains("laptop"), "{failure}");
    assert!(failure.contains("8d"), "{failure}");
    let lines = summary_lines(&fleet, 1);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("laptop") && line.contains("8d")),
        "{lines:#?}"
    );
    assert!(
        lines.last().unwrap().contains("1 beyond unreachable-grace"),
        "{lines:#?}"
    );
}

#[test]
fn unreachable_grace_is_configurable() {
    let (_dir, archive) = archive_with(&format!("{LAPTOP}[pull]\nunreachable-grace = \"1d\"\n"));
    archive
        .db()
        .unwrap()
        .record_host_pull_success("laptop", now_ms() - 2 * DAY_MS)
        .unwrap();

    let fleet = pull(&archive, &FakeRemote::failing_connection(TIMED_OUT));
    assert_eq!(fleet.failed_hosts, vec!["laptop".to_string()]);

    assert!(FleetConfig::parse("[pull]\nunreachable-grace = \"soon\"\n").is_err());
}

#[test]
fn host_without_recorded_success_is_judged_from_when_tracking_began() {
    let (_dir, archive) = archive_with(LAPTOP);

    // First sighting of a new Host: skipped,
    // and the grace window starts now.
    let fleet = pull(&archive, &FakeRemote::failing_connection(TIMED_OUT));
    assert!(!fleet.failed());
    assert_eq!(fleet.unreachable_hosts[0].last_success_ms, None);
    let record = archive
        .db()
        .unwrap()
        .host_pull_record("laptop")
        .unwrap()
        .unwrap();
    assert!(record.tracked_since_ms > now_ms() - 60_000);
    assert_eq!(record.last_success_ms, None);

    // A Host tracked for longer than the grace without any success fails.
    let (_dir, archive) = archive_with(LAPTOP);
    archive
        .db()
        .unwrap()
        .track_host("laptop", now_ms() - 9 * DAY_MS)
        .unwrap();
    let fleet = pull(&archive, &FakeRemote::failing_connection(TIMED_OUT));
    assert!(fleet.failed());
    let failure = fleet.unreachable_hosts[0].failure.as_deref().unwrap();
    assert!(failure.contains("no successful pull"), "{failure}");
}

#[test]
fn ssh_failure_other_than_connecting_is_a_failure() {
    for stderr in [
        "example@192.0.2.10: Permission denied (publickey).",
        "Host key verification failed.",
    ] {
        let (_dir, archive) = archive_with(LAPTOP);
        let fleet = pull(&archive, &FakeRemote::failing_connection(stderr));
        assert!(fleet.unreachable_hosts.is_empty(), "{stderr}");
        assert_eq!(fleet.failed_hosts, vec!["laptop".to_string()], "{stderr}");
    }
}

#[test]
fn reachable_host_pull_records_last_success() {
    let store = temp_store();
    let missing = store.path().join("missing-store");
    let (_dir, archive) = archive_with(&format!(
        "{LAPTOP}claude-root = {:?}\ncodex-root = {:?}\npi-root = {:?}\ncursor-root = {:?}\nchatgpt-root = {:?}\n",
        store.path().to_string_lossy(),
        missing.to_string_lossy(),
        missing.to_string_lossy(),
        missing.to_string_lossy(),
        missing.to_string_lossy(),
    ));
    let before = now_ms();

    let fleet = pull(&archive, &FakeRemote::reachable());

    assert!(!fleet.failed(), "{:?}", fleet.failed_hosts);
    assert!(fleet.unreachable_hosts.is_empty());
    assert_eq!(fleet.totals.sessions_found, 4);
    let record = archive
        .db()
        .unwrap()
        .host_pull_record("laptop")
        .unwrap()
        .unwrap();
    assert!(record.last_success_ms.unwrap() >= before);
}

#[test]
fn per_host_grace_overrides_global_and_report_shows_effective_grace() {
    let (_dir, archive) = archive_with(&format!(
        "{LAPTOP}unreachable-grace = \"30d\"\n[[host]]\nname = \"server\"\nssh = \"server\"\n[pull]\nunreachable-grace = \"1d\"\n"
    ));
    for host in ["laptop", "server"] {
        archive
            .db()
            .unwrap()
            .record_host_pull_success(host, now_ms() - 8 * DAY_MS)
            .unwrap();
    }
    let fleet = pull(&archive, &FakeRemote::failing_connection(TIMED_OUT));
    assert_eq!(fleet.failed_hosts, ["server"]);
    let lines = summary_lines(&fleet, 2);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("laptop") && l.contains("unreachable-grace 30d")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("server") && l.contains("unreachable-grace 1d")),
        "{lines:?}"
    );
}

#[test]
fn invalid_host_grace_is_rejected_at_load() {
    for value in ["soon", "-1d", "1.5d", "18446744073709551615d", ""] {
        let text = format!("{LAPTOP}unreachable-grace = {value:?}\n");
        let error = FleetConfig::parse(&text).unwrap_err();
        assert!(format!("{error:#}").contains("unreachable-grace"));
    }
}

#[test]
fn rebuild_preserves_legacy_pull_health_and_database_loss_restores_it() {
    let (_dir, archive) = archive_with(LAPTOP);
    let tracked = now_ms() - 40 * DAY_MS;
    let success = now_ms() - 8 * DAY_MS;
    let db = archive.db().unwrap();
    db.track_host("laptop", tracked).unwrap();
    db.record_host_pull_success("laptop", success).unwrap();
    db.track_host("never-succeeded", tracked).unwrap();
    drop(db);
    // Also exercise migration of health written by an older release.
    std::fs::remove_file(archive.root().join("host-pulls.json")).ok();
    agent_sessions::archive::rebuild(&archive).unwrap();
    let expected = archive
        .db()
        .unwrap()
        .host_pull_record("laptop")
        .unwrap()
        .unwrap();
    assert_eq!(expected.tracked_since_ms, tracked);
    assert_eq!(expected.last_success_ms, Some(success));
    assert!(pull(&archive, &FakeRemote::failing_connection(TIMED_OUT)).failed());
    std::fs::remove_file(archive.db_path()).unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(
        archive.db().unwrap().host_pull_record("laptop").unwrap(),
        Some(expected)
    );
    let never = archive
        .db()
        .unwrap()
        .host_pull_record("never-succeeded")
        .unwrap()
        .unwrap();
    assert_eq!(never.tracked_since_ms, tracked);
    assert_eq!(never.last_success_ms, None);
}

#[test]
fn host_grace_accepts_all_units_and_zero_and_rejects_bad_values_on_load() {
    for value in ["0ms", "1ms", "2s", "3m", "4h", "30d"] {
        let (dir, _) = archive_with(&format!("{LAPTOP}unreachable-grace = {value:?}\n"));
        FleetConfig::load(dir.path()).unwrap();
    }
    let (dir, _) = archive_with(&format!("{LAPTOP}unreachable-grace = \"soon\"\n"));
    let error = FleetConfig::load(dir.path()).unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("laptop") && error.contains("unreachable-grace"),
        "{error}"
    );
}

#[test]
fn pull_health_is_monotonic_and_corruption_does_not_reset_grace() {
    let (_dir, archive) = archive_with(LAPTOP);
    let db = archive.db().unwrap();
    db.track_host("laptop", 10).unwrap();
    db.track_host("laptop", 20).unwrap();
    db.record_host_pull_success("laptop", 30).unwrap();
    db.record_host_pull_success("laptop", 25).unwrap();
    let expected = db.host_pull_record("laptop").unwrap();
    assert_eq!(expected.unwrap().tracked_since_ms, 10);
    assert_eq!(expected.unwrap().last_success_ms, Some(30));
    drop(db);
    std::fs::remove_file(archive.db_path()).unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(
        archive.db().unwrap().host_pull_record("laptop").unwrap(),
        expected
    );
    for bad in ["{", "{\"version\":2,\"hosts\":{}}"] {
        std::fs::write(archive.root().join("host-pulls.json"), bad).unwrap();
        assert!(agent_sessions::archive::rebuild(&archive).is_err());
        assert_eq!(
            archive.db().unwrap().host_pull_record("laptop").unwrap(),
            expected
        );
    }
}

#[test]
fn rebuild_with_durable_health_can_replace_a_corrupt_database() {
    let (_dir, archive) = archive_with(LAPTOP);
    archive
        .db()
        .unwrap()
        .record_host_pull_success("laptop", 123)
        .unwrap();
    std::fs::write(archive.db_path(), "corrupt database").unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(
        archive
            .db()
            .unwrap()
            .host_pull_record("laptop")
            .unwrap()
            .unwrap()
            .last_success_ms,
        Some(123)
    );
}

#[test]
fn rebuild_can_repair_legacy_corruption_without_a_health_file() {
    let (_dir, archive) = archive_with(LAPTOP);
    std::fs::write(archive.db_path(), "corrupt legacy database").unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert!(!pull(&archive, &FakeRemote::failing_connection(TIMED_OUT)).failed());
}

#[test]
fn render_cannot_claim_the_health_file_path_before_first_pull() {
    let (_dir, archive) =
        archive_with("[views]\ntimezone = \"UTC\"\noutput = \"host-pulls.json\"\n");
    drop(archive.db().unwrap());
    let error = agent_sessions::render::render_all(&archive).unwrap_err();
    assert!(
        format!("{error:#}").contains("durable Archive data"),
        "{error:#}"
    );
    assert!(!archive.root().join("host-pulls.json").exists());
}
