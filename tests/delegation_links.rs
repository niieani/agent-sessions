#[path = "delegation_links/support.rs"]
mod support;

use agent_sessions::{
    leads::{self, SessionLead},
    render::render_all,
};
use serde_json::{Value, json};
use std::fs;
use support::*;

#[test]
fn malformed_requested_by_identifies_cursor_and_field() {
    for invalid in [
        "missing-colon",
        ":id",
        "   :id",
        "claude:",
        "claude:   ",
        "claude:id\n",
    ] {
        let error = lead(invalid).validate().expect_err(invalid);
        let message = format!("{error:#}");
        assert!(message.contains("delegation-42"), "{message}");
        assert!(message.contains("origin.requestedBy"), "{message}");
    }
}

#[test]
fn requested_by_accepts_nonempty_producer_sources_and_native_ids_containing_colons() {
    for valid in [
        "claude:requester",
        "unknown:id",
        "producer.source:native:id",
    ] {
        lead(valid).validate().unwrap();
    }
}

#[test]
fn requested_by_links_the_delegated_workday_and_preserves_origin() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    render_all(&fixture.archive).unwrap();
    let value = fixture.context();
    assert_eq!(value["session"]["led_by"]["origin"]["runState"], "running");
    assert_eq!(
        value["session"]["requested_by"]["stem"],
        "claude 2026-09-01"
    );
    assert_eq!(value["session"]["requested_by"]["source"], "claude");
    assert_eq!(value["session"]["requested_by"]["native_id"], "requester");
    let note = fixture.note();
    assert!(
        note.contains("session-requested-by: \"[[claude 2026-09-01]]\""),
        "{note}"
    );
    assert!(
        note.contains("session-t3-thread: \"workstation:thread-42\""),
        "{note}"
    );
    assert_eq!(render_all(&fixture.archive).unwrap().changed, 0);
}

#[test]
fn absent_requester_links_after_its_session_arrives() {
    let fixture = Fixture::new("claude:requester");
    fixture.assert_unresolved();
    fixture.requester();
    render_all(&fixture.archive).unwrap();
    assert_eq!(
        fixture.context()["session"]["requested_by"]["stem"],
        "claude 2026-09-01"
    );
}

#[test]
fn omitted_requester_removes_the_link_to_its_old_note() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    render_all(&fixture.archive).unwrap();
    assert!(!fixture.context()["session"]["requested_by"].is_null());
    fs::write(
        fixture.root.path().join("config.toml"),
        format!("{CONFIG}\n[views.kind.omitted-requester]\nmatch.source='claude'\nomit=true\n"),
    )
    .unwrap();
    fixture.assert_unresolved();
}

#[test]
fn session_level_views_do_not_supply_requester_session_days() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    fs::write(
        fixture.root.path().join("config.toml"),
        CONFIG.replace("session-day", "session"),
    )
    .unwrap();
    fixture.assert_unresolved();
}

#[test]
fn requester_waiting_for_digest_links_when_its_note_is_rendered() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    fs::write(
        fixture.root.path().join("config.toml"),
        CONFIG.replace(
            "{{ source }} {{ workday }}",
            "{{ source }} {{ workday }} {{ title }}",
        ),
    )
    .unwrap();
    fixture.digest("codex", "worker", "2026-09-01", "Worker");
    render_all(&fixture.archive).unwrap();
    assert!(fixture.context()["session"]["requested_by"].is_null());
    assert!(
        !fs::read_to_string(
            fixture
                .archive
                .views_dir()
                .join("notes/codex 2026-09-01 Worker.md")
        )
        .unwrap()
        .contains("session-requested-by:")
    );
    assert!(
        !fixture
            .archive
            .views_dir()
            .join("notes/claude 2026-08-31 Requester.md")
            .exists()
    );
    fixture.digest("claude", "requester", "2026-08-31", "Requester");
    render_all(&fixture.archive).unwrap();
    assert!(
        fixture
            .archive
            .views_dir()
            .join("notes/claude 2026-08-31 Requester.md")
            .exists()
    );
    assert_eq!(
        fixture.context()["session"]["requested_by"]["stem"],
        "claude 2026-08-31 Requester"
    );
    assert!(
        fs::read_to_string(
            fixture
                .archive
                .views_dir()
                .join("notes/codex 2026-09-01 Worker.md")
        )
        .unwrap()
        .contains("session-requested-by: \"[[claude 2026-08-31 Requester]]\"")
    );
    assert_eq!(render_all(&fixture.archive).unwrap().changed, 0);
}

#[test]
fn self_requester_does_not_link_even_to_another_day_of_its_own_session() {
    let fixture = Fixture::new("codex:worker");
    insert(
        &fixture.archive,
        "codex",
        "worker",
        &["2026-08-31T12:00:00Z", "2026-09-01T12:00:00Z"],
    );
    fixture.assert_unresolved();
    for entry in fs::read_dir(fixture.archive.views_dir().join("json")).unwrap() {
        let value: Value =
            serde_json::from_str(&fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap();
        assert!(value["session"]["requested_by"].is_null());
    }
}

#[test]
fn unknown_requester_source_survives_feed_sync_rebuild_render_and_inspection() {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        process::Command,
        thread,
    };
    let fixture = Fixture::new("claude:requester");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/leads", listener.local_addr().unwrap());
    let record = lead("producer.source:unknown-session");
    let expected = record.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        let body = json!({"version":1,"leads":[record],"nextCursor":"delegation-42","more":false})
            .to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    fs::write(
        fixture.root.path().join("config.toml"),
        format!("{CONFIG}\n[[lead-feeds]]\nname='producer'\nurl='{url}'\n"),
    )
    .unwrap();
    let synced = leads::sync(&fixture.archive, None);
    server.join().unwrap();
    assert_eq!(synced.unwrap(), 1);
    fixture.assert_unresolved();
    agent_sessions::archive::rebuild(&fixture.archive).unwrap();
    assert_eq!(
        fixture
            .archive
            .db()
            .unwrap()
            .session_leads()
            .unwrap()
            .into_values()
            .next()
            .unwrap(),
        expected
    );
    render_all(&fixture.archive).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["leads", "list", "--archive"])
        .arg(fixture.root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<SessionLead>>(&output.stdout).unwrap(),
        vec![expected]
    );
}

#[test]
fn reemitted_lead_with_later_recorded_at_keeps_the_requester_link() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    render_all(&fixture.archive).unwrap();
    let before = fixture.context()["session"]["requested_by"].clone();
    let note = fixture.note();
    let mut reemitted = lead("claude:requester");
    reemitted.cursor = "delegation-43".into();
    reemitted.recorded_at = "2026-10-09T18:00:00Z".into();
    let origin = reemitted.origin.as_mut().unwrap();
    origin.fields.insert("runState".into(), "completed".into());
    origin.fields.insert("latestRunId".into(), "run-43".into());
    leads::store(fixture.root.path(), &reemitted).unwrap();
    fixture
        .archive
        .db()
        .unwrap()
        .upsert_session_leads(&[reemitted])
        .unwrap();
    render_all(&fixture.archive).unwrap();
    assert_eq!(fixture.context()["session"]["requested_by"], before);
    assert_eq!(fixture.note(), note);
    assert_eq!(render_all(&fixture.archive).unwrap().changed, 0);
}

#[test]
fn requester_links_to_its_latest_rendered_day_before_the_delegated_day() {
    let fixture = Fixture::new("claude:requester");
    insert(
        &fixture.archive,
        "claude",
        "requester",
        &["2026-08-29T12:00:00Z", "2026-08-30T12:00:00Z"],
    );
    render_all(&fixture.archive).unwrap();
    assert_eq!(
        fixture.context()["session"]["requested_by"]["stem"],
        "claude 2026-08-30"
    );
    assert!(
        fixture
            .note()
            .contains("session-requested-by: \"[[claude 2026-08-30]]\"")
    );
}

#[test]
fn multi_day_delegation_resolves_each_day_independently_without_using_future_requester_days() {
    let fixture = Fixture::new("claude:requester");
    insert(
        &fixture.archive,
        "codex",
        "worker",
        &[
            "2026-08-31T03:00:00Z",
            "2026-09-01T12:00:00Z",
            "2026-09-02T12:00:00Z",
        ],
    );
    insert(
        &fixture.archive,
        "claude",
        "requester",
        &[
            "2026-08-30T12:00:00Z",
            "2026-09-01T12:00:00Z",
            "2026-09-03T12:00:00Z",
        ],
    );
    render_all(&fixture.archive).unwrap();
    // The first delegated turn belongs to Aug 30 under the configured 04:00 cutoff.
    for (day, requester_day) in [
        ("2026-08-30", "2026-08-30"),
        ("2026-09-01", "2026-09-01"),
        ("2026-09-02", "2026-09-01"),
    ] {
        assert_eq!(
            fixture.context_on(day)["session"]["requested_by"]["workday"],
            requester_day
        );
        let note = fs::read_to_string(
            fixture
                .archive
                .views_dir()
                .join(format!("notes/codex {day}.md")),
        )
        .unwrap();
        assert!(note.contains(&format!(
            "session-requested-by: \"[[claude {requester_day}]]\""
        )));
    }
}

#[test]
fn requester_with_only_future_rendered_days_stays_unresolved() {
    let fixture = Fixture::new("claude:requester");
    insert(
        &fixture.archive,
        "claude",
        "requester",
        &["2026-09-02T12:00:00Z"],
    );
    fixture.assert_unresolved();
}

#[test]
fn earlier_rendered_requester_day_is_used_until_the_same_day_digest_arrives() {
    let fixture = Fixture::new("claude:requester");
    fixture.requester();
    fs::write(
        fixture.root.path().join("config.toml"),
        CONFIG.replace(
            "{{ source }} {{ workday }}",
            "{{ source }} {{ workday }} {{ title }}",
        ),
    )
    .unwrap();
    fixture.digest("codex", "worker", "2026-09-01", "Worker");
    fixture.digest("claude", "requester", "2026-08-31", "Earlier");
    render_all(&fixture.archive).unwrap();
    assert_eq!(
        fixture.context()["session"]["requested_by"]["stem"],
        "claude 2026-08-31 Earlier"
    );
    fixture.digest("claude", "requester", "2026-09-01", "Same day");
    render_all(&fixture.archive).unwrap();
    assert_eq!(
        fixture.context()["session"]["requested_by"]["stem"],
        "claude 2026-09-01 Same day"
    );
}
