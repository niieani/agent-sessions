//! Session Intent: established from the user's opening turns, carried to
//! continuation Session Days, and changed only when the user redirects.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};
use serde_json::Value;

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

/// Replays Session Day outputs in request order; Session titles are fixed.
#[derive(Default)]
struct ScriptedClient {
    requests: Mutex<Vec<ModelRequest>>,
    days: Mutex<VecDeque<&'static str>>,
}

impl ScriptedClient {
    fn new(days: &[&'static str]) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            days: Mutex::new(days.iter().copied().collect()),
        }
    }

    fn day_requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.kind != CompletionKind::SessionTitle)
            .cloned()
            .collect()
    }
}

impl ModelClient for ScriptedClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        self.requests.lock().unwrap().push(request.clone());
        if request.kind == CompletionKind::SessionTitle {
            return Ok(r#"{"title":"Example opt-out"}"#.into());
        }
        let output = self.days.lock().unwrap().pop_front();
        Ok(output.expect("unexpected Session Day request").into())
    }
}

fn config(extra: &str) -> String {
    format!(
        r#"[views]
timezone = "UTC"
unit = "session-day"
{extra}
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
concurrency = 1
max-input-tokens = 60000
request-timeout = "1s"
"#
    )
}

fn options() -> DigestOptions {
    DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: false,
        now_ms: ms("2026-09-03T10:00:00Z"),
    }
}

fn day_path(root: &Path, id: &str, workday: &str) -> PathBuf {
    agent_sessions::digest::digest_dir(root, "codex", id)
        .unwrap()
        .join(format!("{workday}.json"))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// The structured Context block of a Session Day request.
fn context(request: &ModelRequest) -> Value {
    let rest = request.user.strip_prefix("Context:\n").unwrap();
    let end = rest.find("\n\nSession Day body:\n").unwrap();
    serde_json::from_str(&rest[..end]).unwrap()
}

const FIRST: &str = r#"{"topic":"Example opt-out","title":"Found the opt-out process","intent":"Opt the member out of the example plan","tldr":null,"summary":"Found that the plan requires a personal call; the member must call personally.","classification":["research"]}"#;
const UNCHANGED: &str = r#"{"title":"Call script","intent":null,"tldr":null,"summary":"Drafted a call script for the member.","classification":["write"]}"#;
const EMPTY_INTENT: &str = r#"{"title":"Cancel instead","intent":"  ","tldr":null,"summary":"User switched to cancelling.","classification":["plan"]}"#;
const REDIRECTED: &str = r#"{"title":"Cancel instead","intent":"Cancel the example plan entirely","tldr":null,"summary":"User switched to cancelling.","classification":["plan"]}"#;

fn msu_session() -> Conversation {
    conversation(
        "example-case",
        vec![
            message(
                Role::User,
                "Help the member opt out of the example plan",
                "2026-08-28T10:00:00Z",
            ),
            message(
                Role::Assistant,
                "The plan only accepts a call from the member; the member must call personally.",
                "2026-08-28T10:01:00Z",
            ),
            message(Role::User, "draft the call script", "2026-08-29T10:00:00Z"),
            message(
                Role::User,
                "actually, cancel the plan entirely instead",
                "2026-08-30T10:00:00Z",
            ),
        ],
    )
}

#[test]
fn first_day_establishes_intent_and_continuations_carry_it_until_the_user_redirects() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&msu_session())
        .unwrap();
    let client = ScriptedClient::new(&[FIRST, UNCHANGED, EMPTY_INTENT, REDIRECTED]);

    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!((stats.day_written, stats.failed), (3, 0));

    let intents = ["2026-08-28", "2026-08-29", "2026-08-30"]
        .map(|day| read_json(&day_path(root.path(), "example-case", day))["intent"].clone());
    assert_eq!(
        intents,
        [
            "Opt the member out of the example plan",
            "Opt the member out of the example plan",
            "Cancel the example plan entirely"
        ]
        .map(Value::from)
    );

    let requests = client.day_requests();
    let kinds = requests.iter().map(|r| r.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            CompletionKind::FirstSessionDay,
            CompletionKind::ContinuationSessionDay,
            CompletionKind::ContinuationSessionDay,
            CompletionKind::ContinuationSessionDay,
        ]
    );
    let contexts = requests.iter().map(context).collect::<Vec<_>>();
    assert_eq!(contexts[0]["intent"], Value::Null);
    assert_eq!(
        contexts[1]["intent"],
        "Opt the member out of the example plan"
    );
    assert_eq!(
        contexts[2]["intent"],
        "Opt the member out of the example plan"
    );
    assert!(
        contexts
            .iter()
            .all(|context| context.get("opening").is_none())
    );

    let rejected = fs::read_to_string(
        agent_sessions::digest::digest_dir(root.path(), "codex", "example-case")
            .unwrap()
            .join("2026-08-30.rejected.jsonl"),
    )
    .unwrap();
    assert!(rejected.contains("Digest intent is empty"));
}

#[test]
fn continuation_after_pre_intent_digests_establishes_intent_from_opening_turns() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), config("")).unwrap();
    let long_turn = format!("OPENING_THREE {}", "x".repeat(3000));
    let first_day = vec![
        message(Role::User, "OPENING_ONE ship it", "2026-08-28T10:00:00Z"),
        message(
            Role::Assistant,
            "AGENT_PLAN do more",
            "2026-08-28T10:01:00Z",
        ),
        message(Role::User, "OPENING_TWO carefully", "2026-08-28T10:02:00Z"),
        message(Role::User, &long_turn, "2026-08-28T10:03:00Z"),
        message(Role::User, "OPENING_FOUR", "2026-08-28T10:04:00Z"),
    ];
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("legacy", first_day.clone()))
        .unwrap();
    agent_sessions::digest::run_with_client(&archive, &ScriptedClient::new(&[FIRST]), &options())
        .unwrap();
    // Simulate a Digest written before Session Intent existed.
    let first_path = day_path(root.path(), "legacy", "2026-08-28");
    let mut legacy = read_json(&first_path);
    legacy.as_object_mut().unwrap().remove("intent");
    legacy["promptVersion"] = Value::from(2);
    fs::write(&first_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let legacy_bytes = fs::read(&first_path).unwrap();

    let mut both_days = first_day;
    both_days.push(message(Role::User, "continue", "2026-08-29T10:00:00Z"));
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("legacy", both_days))
        .unwrap();
    let client = ScriptedClient::new(&[
        r#"{"title":"Continued","intent":null,"tldr":null,"summary":"Continued.","classification":["build"]}"#,
        r#"{"title":"Continued","intent":"Ship it carefully","tldr":null,"summary":"Continued.","classification":["build"]}"#,
    ]);
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!(
        (stats.candidates, stats.day_written, stats.failed),
        (1, 1, 0)
    );

    assert_eq!(fs::read(&first_path).unwrap(), legacy_bytes);
    let second = read_json(&day_path(root.path(), "legacy", "2026-08-29"));
    assert_eq!(second["intent"], "Ship it carefully");
    assert_eq!(
        second["promptVersion"],
        agent_sessions::digest::PROMPT_VERSION
    );

    let requests = client.day_requests();
    assert_eq!(
        requests[0].kind,
        CompletionKind::ContinuationSessionDayWithoutIntent
    );
    let context = context(&requests[0]);
    assert_eq!(context["intent"], Value::Null);
    let opening = context["opening"].as_array().unwrap();
    assert_eq!(opening.len(), 3);
    assert!(opening[0].as_str().unwrap().starts_with("OPENING_ONE"));
    assert!(opening[1].as_str().unwrap().starts_with("OPENING_TWO"));
    let clipped = opening[2].as_str().unwrap();
    assert!(clipped.starts_with("OPENING_THREE") && clipped.ends_with('…'));
    assert!(clipped.chars().count() < long_turn.chars().count());
    assert!(!context.to_string().contains("OPENING_FOUR"));
    assert!(!context.to_string().contains("AGENT_PLAN"));
}

#[test]
fn vault_template_renders_intent_in_the_digest_callout() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/templates/vault-agent-session.md.j2");
    fs::write(
        root.path().join("config.toml"),
        config(&format!(
            r#"template = "{}"
[views.layout]
session = "notes/{{{{ workday }}}} {{{{ title }}}}.md"
loose = "loose/{{{{ workday }}}} {{{{ title }}}}.md"
subagent = "{{{{ parent_dir }}}}/subagents/{{{{ short_id }}}}.md""#,
            template.display()
        )),
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&msu_session())
        .unwrap();
    agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[FIRST, UNCHANGED, REDIRECTED]),
        &options(),
    )
    .unwrap();

    agent_sessions::render::render_all(&archive).unwrap();
    let rendered = fs::read_to_string(
        archive
            .views_dir()
            .join("notes/2026-08-29 Example opt-out - Call script.md"),
    )
    .unwrap();
    assert!(rendered.contains(
        "> [!INFO] AI-derived\n> **Intent:** Opt the member out of the example plan\n>\n> Drafted a call script for the member.\n"
    ));
}
