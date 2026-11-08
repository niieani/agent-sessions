use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role, SubagentParent};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn message(role: Role, text: &str, timestamp: &str) -> Message {
    Message {
        role,
        kind: MessageKind::Dialog,
        text: text.into(),
        timestamp_ms: Some(ms(timestamp)),
        actions_before: 0,
    }
}

fn conversation(id: &str, messages: Vec<Message>) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}"),
        },
        title: None,
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: messages.first().and_then(|message| message.timestamp_ms),
        updated_at_ms: messages.last().and_then(|message| message.timestamp_ms),
        messages,
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

struct FakeClient {
    requests: Mutex<Vec<ModelRequest>>,
    day_output: Mutex<String>,
}

impl Default for FakeClient {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            day_output: Mutex::new(
                r#"{"topic":"Archive","title":"First pass","intent":"Do the work","tldr":null,"summary":"Built the archive.","classification":["build"]}"#.into(),
            ),
        }
    }
}

impl ModelClient for FakeClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(match request.kind {
            CompletionKind::FirstSessionDay
            | CompletionKind::ContinuationSessionDay
            | CompletionKind::ContinuationSessionDayWithoutIntent => {
                let output = self.day_output.lock().unwrap().clone();
                if request.user.contains("\"topic\": \"Archive\"") {
                    output.replace("\"topic\":\"Archive\",", "")
                } else {
                    output
                }
            }
            CompletionKind::SessionTitle => r#"{"title":"Archive Agent Session Work"}"#.into(),
        })
    }
}

fn config() -> &'static str {
    r#"[views]
timezone = "UTC"
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
concurrency = 2
max-input-tokens = 60000
request-timeout = "1s"
"#
}

fn digest_dir(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    agent_sessions::digest::digest_dir(root, "codex", id).unwrap()
}

#[test]
fn first_digest_locks_title_refreshes_session_and_second_run_is_idle() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "one",
            vec![message(Role::User, "build it", "2026-08-30T10:00:00Z")],
        ))
        .unwrap();
    let client = FakeClient::default();
    assert!(!root.path().join(".agent-sessions.lock").exists());
    let options = DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: false,
        now_ms: ms("2026-09-03T10:00:00Z"),
    };
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!(
        (stats.day_written, stats.session_written, stats.failed),
        (1, 1, 0)
    );
    let day: serde_json::Value = serde_json::from_slice(
        &fs::read(digest_dir(root.path(), "one").join("2026-08-30.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(day["lockedTitle"], "Archive - First pass");
    let session: serde_json::Value = serde_json::from_slice(
        &fs::read(digest_dir(root.path(), "one").join("session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(session["topic"], "Archive");
    assert_eq!(session["title"], "Archive Agent Session Work");

    let before = client.requests.lock().unwrap().len();
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!(stats.candidates, 0);
    assert_eq!(client.requests.lock().unwrap().len(), before);

    // A Digest written under an older prompt contract is still a Digest:
    // promptVersion is provenance, never a staleness signal.
    let day_path = digest_dir(root.path(), "one").join("2026-08-30.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&day_path).unwrap()).unwrap();
    old["promptVersion"] = serde_json::json!(1);
    fs::write(&day_path, serde_json::to_vec(&old).unwrap()).unwrap();
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!(stats.candidates, 0);
    assert_eq!(client.requests.lock().unwrap().len(), before);

    client.day_output.lock().unwrap().clone_from(
        &r#"{"title":"Expanded pass","intent":"Do the work","tldr":null,"summary":"Expanded the archive.","classification":["fix"]}"#.into(),
    );
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "one",
            vec![
                message(Role::User, "build it", "2026-08-30T10:00:00Z"),
                message(Role::Assistant, "done", "2026-08-30T10:01:00Z"),
            ],
        ))
        .unwrap();
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!((stats.day_written, stats.session_written), (1, 1));
    let day: serde_json::Value = serde_json::from_slice(
        &fs::read(digest_dir(root.path(), "one").join("2026-08-30.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(day["title"], "Archive - Expanded pass");
    assert_eq!(day["lockedTitle"], "Archive - First pass");
    assert_eq!(day["topic"], "Archive");
}

#[test]
fn continuation_prompt_contains_prior_digest_and_dry_run_makes_no_request() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "two-days",
            vec![
                message(Role::User, "first", "2026-08-30T10:00:00Z"),
                message(Role::User, "second", "2026-08-31T10:00:00Z"),
            ],
        ))
        .unwrap();
    let client = FakeClient::default();
    let options = DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: true,
        now_ms: ms("2026-09-03T10:00:00Z"),
    };
    let stats = agent_sessions::digest::run(&archive, &options).unwrap();
    assert_eq!(stats.candidates, 2);
    assert_eq!(stats.previews.len(), 2);
    assert_eq!(stats.failed, 0);
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(!root.path().join("digests").exists());
    assert!(!root.path().join(".agent-sessions.lock").exists());

    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            dry_run: false,
            ..options
        },
    )
    .unwrap();
    assert_eq!(stats.day_written, 2);
    let requests = client.requests.lock().unwrap();
    let days = requests
        .iter()
        .filter(|request| {
            matches!(
                request.kind,
                CompletionKind::FirstSessionDay | CompletionKind::ContinuationSessionDay
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(days.len(), 2);
    assert!(days[1].user.contains("Built the archive."));
    assert!(days[1].user.contains("\"facet\": \"First pass\""));
    assert!(!days[1].user.contains("Archive - First pass"));
}

#[test]
fn malformed_model_json_is_counted_without_a_partial_file() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "bad",
            vec![message(Role::User, "bad", "2026-08-30T10:00:00Z")],
        ))
        .unwrap();
    let client = FakeClient::default();
    client
        .day_output
        .lock()
        .unwrap()
        .clone_from(&"not json".into());
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
    assert_eq!(stats.failed, 1);
    assert!(
        !digest_dir(root.path(), "bad")
            .join("2026-08-30.json")
            .exists()
    );
}

#[test]
fn kind_and_session_state_gates_requests_and_pin_classification() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        format!(
            "{}\n[views.kind.omitted]\nmatch.path.is = '/work/omit'\nomit = true\n\
             [views.kind.disabled]\nmatch.path.is = '/work/disabled'\nloose = true\ndigest = false\n\
             [views.kind.pinned]\nmatch.path.is = '/work/pinned'\nloose = true\nclassification = ['write', 'plan']\n",
            config()
        ),
    )
    .unwrap();
    for (id, path) in [
        ("omit", "/work/omit"),
        ("disabled", "/work/disabled"),
        ("pinned", "/work/pinned"),
    ] {
        let mut session = conversation(id, vec![message(Role::User, id, "2026-08-30T10:00:00Z")]);
        session.project_path = Some(path.into());
        archive.db().unwrap().upsert_conversation(&session).unwrap();
    }
    let mut current = conversation(
        "current",
        vec![message(Role::User, "current", "2026-09-03T10:00:00Z")],
    );
    current.project_path = Some("/work/current".into());
    archive.db().unwrap().upsert_conversation(&current).unwrap();
    let mut ongoing = conversation(
        "ongoing",
        vec![message(Role::User, "ongoing", "2026-08-30T10:00:00Z")],
    );
    ongoing.project_path = Some("/work/ongoing".into());
    ongoing.trailing_actions = 1;
    archive.db().unwrap().upsert_conversation(&ongoing).unwrap();
    let mut subagent = conversation(
        "subagent",
        vec![message(Role::User, "subagent", "2026-08-30T10:00:00Z")],
    );
    subagent.subagent_parent = Some(SubagentParent {
        parent_native_id: "parent".into(),
        description: None,
        agent_type: None,
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: None,
    });
    archive
        .db()
        .unwrap()
        .upsert_conversation(&subagent)
        .unwrap();

    let client = FakeClient::default();
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
    assert_eq!(stats.day_written, 1);
    let digest: serde_json::Value = serde_json::from_slice(
        &fs::read(digest_dir(root.path(), "pinned").join("2026-08-30.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        digest["classification"],
        serde_json::json!(["write", "plan"])
    );
    for id in ["omit", "disabled", "current", "ongoing", "subagent"] {
        assert!(!digest_dir(root.path(), id).exists());
    }
}

#[test]
fn oversized_input_middle_cuts_largest_message_and_keeps_short_turns() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        config().replace("max-input-tokens = 60000", "max-input-tokens = 800"),
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "large",
            vec![
                message(Role::User, "start", "2026-08-30T10:00:00Z"),
                message(
                    Role::Assistant,
                    &format!("MIDDLE_SENTINEL {}", "m ".repeat(4_000)),
                    "2026-08-30T10:01:00Z",
                ),
                message(
                    Role::Assistant,
                    &format!("LAST_SENTINEL {}", "z".repeat(500)),
                    "2026-08-30T10:02:00Z",
                ),
            ],
        ))
        .unwrap();
    let client = FakeClient::default();
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
    let request = client
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| {
            matches!(
                request.kind,
                CompletionKind::FirstSessionDay | CompletionKind::ContinuationSessionDay
            )
        })
        .unwrap()
        .clone();
    assert!(request.user.contains("MIDDLE_SENTINEL"));
    assert!(request.user.contains("[message middle truncated]"));
    assert!(request.user.contains("start"));
    assert!(request.user.contains("LAST_SENTINEL"));
    let digest: serde_json::Value = serde_json::from_slice(
        &fs::read(digest_dir(root.path(), "large").join("2026-08-30.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(digest["truncated"], true);
}

struct ConcurrencyClient {
    active: AtomicUsize,
    maximum: AtomicUsize,
}

impl ModelClient for ConcurrencyClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        // Hold each request until a second one is in flight (or a deadline
        // passes), so overlap never depends on scheduler timing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.maximum.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(20));
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(match request.kind {
            CompletionKind::FirstSessionDay => r#"{"topic":"Work","title":"Done","intent":"Do the work","tldr":null,"summary":"Done.","classification":["build"]}"#.into(),
            CompletionKind::ContinuationSessionDay => r#"{"title":"Done","intent":null,"tldr":null,"summary":"Done.","classification":["build"]}"#.into(),
            CompletionKind::ContinuationSessionDayWithoutIntent => r#"{"title":"Done","intent":"Do the work","tldr":null,"summary":"Done.","classification":["build"]}"#.into(),
            CompletionKind::SessionTitle => r#"{"title":"Work Agent Session Summary"}"#.into(),
        })
    }
}

#[test]
fn configured_concurrency_bounds_parallel_sessions() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    for id in ["parallel-a", "parallel-b", "parallel-c"] {
        archive
            .db()
            .unwrap()
            .upsert_conversation(&conversation(
                id,
                vec![message(Role::User, id, "2026-08-30T10:00:00Z")],
            ))
            .unwrap();
    }
    let client = ConcurrencyClient {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    };
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
    assert_eq!(client.maximum.load(Ordering::SeqCst), 2);
}

#[test]
fn opaque_native_id_is_encoded_into_one_digest_path_component() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "nested/id",
            vec![message(Role::User, "build it", "2026-08-30T10:00:00Z")],
        ))
        .unwrap();

    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &FakeClient::default(),
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

    assert_eq!(stats.day_written, 1);
    let dir = digest_dir(root.path(), "nested/id");
    assert_eq!(dir.parent().unwrap(), root.path().join("digests/codex"));
    assert!(dir.join("2026-08-30.json").is_file());
}

#[test]
fn impossible_input_budget_fails_without_a_model_request() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        config().replace("max-input-tokens = 60000", "max-input-tokens = 1"),
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "too-small",
            vec![message(Role::User, "build it", "2026-08-30T10:00:00Z")],
        ))
        .unwrap();
    let client = FakeClient::default();

    let preview = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: true,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();
    assert!(preview.previews.is_empty());
    assert_eq!(preview.failed, 1);

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

    assert_eq!(stats.failed, 1);
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(
        !digest_dir(root.path(), "too-small")
            .join("2026-08-30.json")
            .exists()
    );
}
