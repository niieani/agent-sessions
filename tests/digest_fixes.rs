use std::collections::VecDeque;
use std::fs;
use std::process::Command;
use std::sync::Mutex;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{DigestOptions, ModelClient, ModelRequest};
use agent_sessions::fleet::FleetConfig;
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn conversation(id: &str, path: &str, text: &str) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}"),
        },
        title: None,
        project_path: Some(path.into()),
        native_project: None,
        created_at_ms: Some(ms("2026-08-30T10:00:00Z")),
        updated_at_ms: Some(ms("2026-08-30T10:00:00Z")),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: text.into(),
            timestamp_ms: Some(ms("2026-08-30T10:00:00Z")),
            actions_before: 0,
        }],
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

fn config(extra: &str) -> String {
    format!(
        r#"[views]
timezone = "UTC"
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
concurrency = 1
max-input-tokens = 60000
request-timeout = "1s"
retries = 2
{extra}
"#
    )
}

struct SequenceClient {
    responses: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl SequenceClient {
    fn new(responses: &[&str]) -> Self {
        Self {
            responses: Mutex::new(responses.iter().map(|value| (*value).into()).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl ModelClient for SequenceClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.responses.lock().unwrap().pop_front().unwrap())
    }
}

#[test]
fn rejected_day_is_recorded_and_corrected_before_storage() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("retry", "/work/project", "build it"))
        .unwrap();
    let before_attempt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let client = SequenceClient::new(&[
        r#"{"title":"First pass","intent":"Do the work","tldr":null,"summary":"Missing topic.","classification":["build"]}"#,
        r#"{"topic":"Archive","title":"First pass","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#,
        r#"{"title":"Archive Session"}"#,
    ]);

    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();
    assert_eq!((stats.requests, stats.day_written, stats.failed), (3, 1, 0));

    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "retry").unwrap();
    let day: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("2026-08-30.json")).unwrap()).unwrap();
    assert_eq!(day["title"], "Archive - First pass");
    assert_eq!(day["promptSource"], "builtin");
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(session["title"], "Archive Session");
    assert_eq!(session["promptSource"], "builtin");

    let lines = fs::read_to_string(dir.join("2026-08-30.rejected.jsonl")).unwrap();
    let rejected: serde_json::Value = serde_json::from_str(lines.trim()).unwrap();
    assert!(rejected["attemptedAtMs"].as_i64().unwrap() >= before_attempt);
    assert_eq!(rejected["kind"], "day");
    assert_eq!(rejected["attempt"], 1);
    assert!(rejected["error"].as_str().unwrap().contains("topic"));
    assert_eq!(
        rejected["raw"],
        client.requests.lock().unwrap()[1].corrections[0].raw
    );
}

#[test]
fn list_omitted_returns_only_existing_digest_directories_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        format!(
            "{}\n[views.kind.capture]\nmatch.message.starts-with = \"---\\naction: \"\nmatch.source = 'codex'\nomit = true\n",
            config("")
        ),
    )
    .unwrap();
    for (id, text) in [("omit", "---\naction: archive"), ("keep", "hello")] {
        archive
            .db()
            .unwrap()
            .upsert_conversation(&conversation(id, "/work/project", text))
            .unwrap();
        fs::create_dir_all(agent_sessions::digest::digest_dir(root.path(), "codex", id).unwrap())
            .unwrap();
    }
    let before = fs::read(root.path().join("archive.db")).unwrap();
    let omitted = agent_sessions::digest::list_omitted(&archive).unwrap();
    assert_eq!(
        omitted,
        vec![agent_sessions::digest::digest_dir(root.path(), "codex", "omit").unwrap()]
    );
    assert_eq!(fs::read(root.path().join("archive.db")).unwrap(), before);
}

#[test]
fn digest_prompt_overrides_require_placeholders() {
    assert!(FleetConfig::parse(&config("[digest.prompts]\nday = 'No fields'\n")).is_err());
    let parsed = FleetConfig::parse(&config(
        "[digest.prompts]\nday = 'Kinds: {classifications}; budget: {summary_budget}'\nsession-title = 'Short stable title'\n",
    ))
    .unwrap();
    let digest = parsed.digest.unwrap();
    assert_eq!(
        digest.prompts.day.as_deref(),
        Some("Kinds: {classifications}; budget: {summary_budget}")
    );
    assert_eq!(
        digest.prompts.session_title.as_deref(),
        Some("Short stable title")
    );
}

#[test]
fn list_omitted_cli_does_not_create_missing_archive_directories() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    drop(archive.db().unwrap());
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    drop(archive);
    fs::remove_dir(root.path().join("captures")).unwrap();
    fs::remove_dir(root.path().join("views")).unwrap();
    let database_before = fs::read(root.path().join("archive.db")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args([
            "digest",
            "--archive",
            root.path().to_str().unwrap(),
            "--list-omitted",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.path().join("captures").exists());
    assert!(!root.path().join("views").exists());
    assert_eq!(
        fs::read(root.path().join("archive.db")).unwrap(),
        database_before
    );
}

#[test]
fn custom_prompts_are_expanded_and_recorded_without_changing_version() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        config(
            "[digest.prompts]\nday = 'Kinds {classifications}; budget {summary_budget}'\nsession-title = 'Stable session title'\n",
        ),
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("custom", "/work/project", "build it"))
        .unwrap();
    let client = SequenceClient::new(&[
        r#"{"topic":"Archive","title":"Built","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#,
        r#"{"title":"Archive Work"}"#,
    ]);

    agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();

    let requests = client.requests.lock().unwrap();
    assert!(requests[0].system.starts_with("Kinds build, fix, research"));
    assert!(requests[0].system.contains("budget 60"));
    assert_eq!(requests[1].system, "Stable session title");
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "custom").unwrap();
    for filename in ["2026-08-30.json", "session.json"] {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(filename)).unwrap()).unwrap();
        assert_eq!(value["promptSource"], "config");
        assert_eq!(
            value["promptVersion"],
            agent_sessions::digest::PROMPT_VERSION
        );
    }
}

#[test]
fn session_title_rejections_share_the_day_sidecar_and_keep_attempt_numbers() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("title-retry", "/work/project", "build it"))
        .unwrap();
    let client = SequenceClient::new(&[
        r#"{"topic":"Archive","title":"Built","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#,
        r#"{"title":""}"#,
        &format!(r#"{{"title":"{}"}}"#, "x".repeat(81)),
        r#"{"title":"Archive Session"}"#,
    ]);

    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();
    assert_eq!((stats.requests, stats.failed), (4, 0));
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "title-retry").unwrap();
    let rejected = fs::read_to_string(dir.join("2026-08-30.rejected.jsonl")).unwrap();
    let rows = rejected
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0]["kind"].as_str(), rows[0]["attempt"].as_u64()),
        (Some("session-title"), Some(1))
    );
    assert_eq!(
        (rows[1]["kind"].as_str(), rows[1]["attempt"].as_u64()),
        (Some("session-title"), Some(2))
    );
}

#[test]
fn dry_run_range_selects_only_workdays_inside_both_bounds() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    let mut session = conversation("range", "/work/project", "first");
    session.messages = vec![
        Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "older".into(),
            timestamp_ms: Some(ms("2025-11-30T10:00:00Z")),
            actions_before: 0,
        },
        Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "lower bound".into(),
            timestamp_ms: Some(ms("2025-12-01T10:00:00Z")),
            actions_before: 0,
        },
        Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "inside".into(),
            timestamp_ms: Some(ms("2025-12-15T10:00:00Z")),
            actions_before: 0,
        },
        Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "upper bound".into(),
            timestamp_ms: Some(ms("2025-12-30T10:00:00Z")),
            actions_before: 0,
        },
        Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "newer".into(),
            timestamp_ms: Some(ms("2025-12-31T10:00:00Z")),
            actions_before: 0,
        },
    ];
    session.created_at_ms = session.messages.first().unwrap().timestamp_ms;
    session.updated_at_ms = session.messages.last().unwrap().timestamp_ms;
    archive.db().unwrap().upsert_conversation(&session).unwrap();

    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &SequenceClient::new(&[]),
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: Some("2025-12-01".into()),
            through: Some("2025-12-30".into()),
            dry_run: true,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();
    assert_eq!(stats.candidates, 3);
    assert_eq!(
        stats
            .previews
            .iter()
            .map(|preview| preview.workday.as_str())
            .collect::<Vec<_>>(),
        ["2025-12-01", "2025-12-15", "2025-12-30"]
    );
}

#[test]
fn digest_rejects_a_reversed_workday_range() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();

    let error = agent_sessions::digest::run_with_client(
        &archive,
        &SequenceClient::new(&[]),
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: Some("2025-12-30".into()),
            through: Some("2025-12-01".into()),
            dry_run: true,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("range is reversed"));
}

#[test]
fn digest_rejects_day_combined_with_a_workday_range() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();

    let error = agent_sessions::digest::run_with_client(
        &archive,
        &SequenceClient::new(&[]),
        &DigestOptions {
            session: None,
            force: false,
            day: Some("2025-12-15".into()),
            from: Some("2025-12-01".into()),
            through: Some("2025-12-30".into()),
            dry_run: true,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cannot combine"));
}
