mod common;

use agent_sessions::{
    archive::{Archive, rebuild},
    digest::{self, DigestOptions},
    fleet::FleetConfig,
    render::render_all,
};
use std::fs;

const CONFIG: &str = r#"
[views]
timezone = "UTC"
unit = "session-day"
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "UNUSED"
[views.kind.no-response]
match.dialog.assistant-messages.eq = 0
omit = true
[views.kind.tiny]
match.dialog.total-chars.lte = 50
omit = true
"#;

fn write_pi(store: &std::path::Path, lines: &[(&str, &str, &str)]) {
    fs::create_dir_all(store).unwrap();
    let mut text = "{\"type\":\"session\",\"version\":1,\"id\":\"statistics\",\"timestamp\":\"2026-08-01T10:00:00Z\",\"cwd\":\"/work/app\"}\n".to_string();
    for (role, content, timestamp) in lines {
        text.push_str(&serde_json::json!({"type":"message","timestamp":timestamp,"message":{"role":role,"content":[{"type":"text","text":content}]}}).to_string());
        text.push('\n');
    }
    fs::write(store.join("session.jsonl"), text).unwrap();
}

#[test]
fn growth_admits_previously_omitted_session_and_small_followup_after_rebuild() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("store");
    let archive = Archive::open(&root.path().join("archive")).unwrap();
    fs::write(archive.root().join("config.toml"), CONFIG).unwrap();
    let options = DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: true,
        now_ms: 1_788_480_000_000,
    };
    let mut lines = vec![("user", "test", "2026-08-01T10:00:00Z")];
    write_pi(&store, &lines);
    common::pull_pi(&store, archive.root());
    assert_eq!(render_all(&archive).unwrap().transcripts, 0);
    assert_eq!(digest::run(&archive, &options).unwrap().candidates, 0);
    lines.push((
        "assistant",
        "a substantive response that contains substantially more than fifty characters",
        "2026-08-01T10:01:00Z",
    ));
    lines.push(("user", "ok", "2026-08-02T10:00:00Z"));
    lines.push(("assistant", "done", "2026-08-02T10:01:00Z"));
    write_pi(&store, &lines);
    common::pull_pi(&store, archive.root());
    assert_eq!(render_all(&archive).unwrap().transcripts, 2);
    assert_eq!(digest::run(&archive, &options).unwrap().candidates, 2);
    rebuild(&archive).unwrap();
    assert_eq!(render_all(&archive).unwrap().transcripts, 2);
    assert_eq!(digest::run(&archive, &options).unwrap().candidates, 2);
}

#[test]
fn unicode_totals_and_boolean_composition() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("store");
    let archive = Archive::open(&root.path().join("archive")).unwrap();
    write_pi(
        &store,
        &[
            ("user", "é🦀", "2026-08-01T10:00:00Z"),
            ("assistant", "答", "2026-08-01T10:01:00Z"),
        ],
    );
    common::pull_pi(&store, archive.root());
    let rules = r#"[views.kind.measured]
omit = true
[[views.kind.measured.match]]
dialog.total-chars.eq = 999
[[views.kind.measured.match]]
source = "pi"
dialog.user-messages.eq = 1
dialog.assistant-messages.gte = 1
dialog.assistant-messages.lt = 2
dialog.user-chars.eq = 2
dialog.assistant-chars.eq = 1
dialog.total-chars.gt = 2
dialog.total-chars.lte = 3
"#;
    fs::write(archive.root().join("config.toml"), rules).unwrap();
    assert_eq!(render_all(&archive).unwrap().transcripts, 0);
    fs::write(
        archive.root().join("config.toml"),
        rules.replace("user-chars.eq = 2", "user-chars.eq = 3"),
    )
    .unwrap();
    assert_eq!(render_all(&archive).unwrap().transcripts, 1);
}

#[test]
fn invalid_numeric_predicates_are_config_errors() {
    for predicates in [
        "eq = -1",
        "eq = 1.5",
        "eq = []",
        "eq = 1\nlt = 1",
        "gt = 3\nlte = 3",
        "gt = 1\nlt = 2",
        "unknown = 1",
        "",
    ] {
        let config = format!(
            "[views.kind.invalid]\nomit = true\n[views.kind.invalid.match.dialog.total-chars]\n{predicates}\n"
        );
        assert!(FleetConfig::parse(&config).is_err(), "{predicates}");
    }
}
