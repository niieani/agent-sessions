//! Forced re-digest of one Session Day: regenerates a Digest that already
//! covers its Messages, following the Intent chain of the earlier days.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{
    CompletionKind, DigestOptions, ModelClient, ModelRequest, SessionKey,
};
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
            return Ok(r#"{"title":"Junk Mail Opt-out"}"#.into());
        }
        let output = self.days.lock().unwrap().pop_front();
        Ok(output.expect("unexpected Session Day request").into())
    }
}

const CONFIG: &str = r#"[views]
timezone = "UTC"
unit = "session-day"

[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
concurrency = 1
max-input-tokens = 60000
request-timeout = "1s"
"#;

const FIRST: &str = r#"{"topic":"Junk Mail Opt-out","title":"Opened with Proceed","intent":"Proceed","tldr":null,"summary":"User said proceed.","classification":["ops"]}"#;
const SECOND: &str = r#"{"title":"Catalog Choice batch","intent":null,"tldr":null,"summary":"Submitted opt-outs.","classification":["ops"]}"#;
const REDIGESTED_FIRST: &str = r#"{"title":"Cataloged photographed mail","intent":"Catalog the photographed junk mail and opt out of each sender","tldr":null,"summary":"Cataloged the mail photos.","classification":["ops"]}"#;
const REDIGESTED_SECOND: &str = r#"{"title":"Example opt-outs","intent":null,"tldr":null,"summary":"Submitted opt-outs; the member must call the example organization.","classification":["ops"]}"#;

fn two_day_session() -> Conversation {
    conversation(
        "mail",
        vec![
            message(
                Role::User,
                "Catalog the photographed junk mail and opt us out",
                "2026-08-28T10:00:00Z",
            ),
            message(
                Role::Assistant,
                "Cataloged 40 senders.",
                "2026-08-28T10:01:00Z",
            ),
            message(Role::User, "Proceed", "2026-08-28T10:02:00Z"),
            message(
                Role::User,
                "continue with the forms",
                "2026-08-29T10:00:00Z",
            ),
            message(Role::Assistant, "Submitted.", "2026-08-29T10:01:00Z"),
        ],
    )
}

fn other_session() -> Conversation {
    conversation(
        "other",
        vec![
            message(Role::User, "unrelated work", "2026-08-28T11:00:00Z"),
            message(Role::Assistant, "done", "2026-08-28T11:01:00Z"),
        ],
    )
}

fn options(session: Option<&str>, day: Option<&str>, force: bool) -> DigestOptions {
    DigestOptions {
        day: day.map(str::to_string),
        from: None,
        through: None,
        session: session.map(|key| SessionKey::parse(key).unwrap()),
        force,
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

fn context(request: &ModelRequest) -> Value {
    let rest = request.user.strip_prefix("Context:\n").unwrap();
    let end = rest.find("\n\nSession Day body:\n").unwrap();
    serde_json::from_str(&rest[..end]).unwrap()
}

/// An Archive whose two Sessions are fully digested.
fn digested_archive() -> (tempfile::TempDir, Archive) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), CONFIG).unwrap();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&two_day_session()).unwrap();
    db.upsert_conversation(&other_session()).unwrap();
    drop(db);
    let other = r#"{"topic":"Other","title":"Unrelated chores","intent":"Do other work","tldr":null,"summary":"Other.","classification":["build"]}"#;
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[FIRST, SECOND, other]),
        &options(None, None, false),
    )
    .unwrap();
    assert_eq!((stats.day_written, stats.failed), (3, 0));
    (root, archive)
}

#[test]
fn forced_days_regenerate_in_order_and_carry_the_new_intent() {
    let (root, archive) = digested_archive();
    let other_bytes = fs::read(day_path(root.path(), "other", "2026-08-28")).unwrap();

    let unforced = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[]),
        &options(Some("codex:mail"), Some("2026-08-28"), false),
    )
    .unwrap();
    assert_eq!(unforced.candidates, 0, "a covered Session Day needs force");

    let client = ScriptedClient::new(&[REDIGESTED_FIRST]);
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &options(Some("codex:mail"), Some("2026-08-28"), true),
    )
    .unwrap();
    assert_eq!(
        (stats.candidates, stats.day_written, stats.failed),
        (1, 1, 0)
    );
    let first = read_json(&day_path(root.path(), "mail", "2026-08-28"));
    assert_eq!(
        first["intent"],
        "Catalog the photographed junk mail and opt out of each sender"
    );
    assert_eq!(
        first["title"],
        "Junk Mail Opt-out - Cataloged photographed mail"
    );
    assert_eq!(
        first["lockedTitle"], "Junk Mail Opt-out - Opened with Proceed",
        "force never changes a Locked Title"
    );
    assert_eq!(
        client.day_requests()[0].kind,
        CompletionKind::ContinuationSessionDayWithoutIntent,
        "the Topic is kept; the Intent is re-established from the first day"
    );

    let client = ScriptedClient::new(&[REDIGESTED_SECOND]);
    agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &options(Some("codex:mail"), Some("2026-08-29"), true),
    )
    .unwrap();
    assert_eq!(
        context(&client.day_requests()[0])["intent"],
        "Catalog the photographed junk mail and opt out of each sender"
    );
    let second = read_json(&day_path(root.path(), "mail", "2026-08-29"));
    assert_eq!(
        second["intent"],
        "Catalog the photographed junk mail and opt out of each sender"
    );
    assert_eq!(
        fs::read(day_path(root.path(), "other", "2026-08-28")).unwrap(),
        other_bytes,
        "other Sessions are untouched"
    );
}

#[test]
fn forcing_a_continuation_requires_current_earlier_digests() {
    let (root, archive) = digested_archive();
    fs::remove_file(day_path(root.path(), "mail", "2026-08-28")).unwrap();
    let error = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[]),
        &options(Some("codex:mail"), Some("2026-08-29"), true),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("2026-08-28"),
        "names the earlier Session Day: {error:#}"
    );

    // Restored Messages grow the first day, so its Digest is out of date.
    let mut grown = two_day_session();
    grown.messages.insert(
        1,
        message(Role::User, "restored request", "2026-08-28T10:00:30Z"),
    );
    let (root, archive) = digested_archive();
    archive.db().unwrap().upsert_conversation(&grown).unwrap();
    let error = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[]),
        &options(Some("codex:mail"), Some("2026-08-29"), true),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("2026-08-28"), "{error:#}");
    assert!(day_path(root.path(), "mail", "2026-08-29").exists());
}

#[test]
fn force_rejects_sessions_and_days_it_cannot_digest() {
    let (_root, archive) = digested_archive();
    for (session, day) in [
        ("codex:missing", "2026-08-28"),
        ("claude:mail", "2026-08-28"),
        ("codex:mail", "2026-08-27"),
        ("codex:mail", "2026-09-03"),
    ] {
        let result = agent_sessions::digest::run_with_client(
            &archive,
            &ScriptedClient::new(&[]),
            &options(Some(session), Some(day), true),
        );
        assert!(result.is_err(), "{session} {day} must fail");
    }
    let result = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[]),
        &options(None, Some("2026-08-28"), true),
    );
    assert!(result.is_err(), "force needs one Session");
}

#[test]
fn session_selection_restricts_ordinary_candidates() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), CONFIG).unwrap();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&two_day_session()).unwrap();
    db.upsert_conversation(&other_session()).unwrap();
    drop(db);
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[FIRST, SECOND]),
        &options(Some("codex:mail"), None, false),
    )
    .unwrap();
    assert_eq!((stats.candidates, stats.day_written), (2, 2));
    assert!(!day_path(root.path(), "other", "2026-08-28").exists());
}

#[test]
fn session_keys_keep_the_full_native_id() {
    let key = SessionKey::parse("codex:01a0b788-4ce9-7e92-bddb-73311ea8df88").unwrap();
    assert_eq!(
        (key.source.as_str(), key.native_id.as_str()),
        ("codex", "01a0b788-4ce9-7e92-bddb-73311ea8df88")
    );
    let key = SessionKey::parse("cursor:composer:abc").unwrap();
    assert_eq!(key.native_id, "composer:abc");
    assert!(SessionKey::parse("no-source").is_err());
    assert!(SessionKey::parse(":id").is_err());
    assert!(SessionKey::parse("codex:").is_err());
}

#[test]
fn same_count_dialog_corrections_invalidate_digests_but_action_changes_do_not() {
    let (root, archive) = digested_archive();
    let path = day_path(root.path(), "mail", "2026-08-28");
    let before = read_json(&path);
    let mut corrected = two_day_session();
    corrected.messages[0].text = "Catalog all photographed mail and opt out".into();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&corrected)
        .unwrap();
    let client = ScriptedClient::new(&[REDIGESTED_FIRST]);
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &options(Some("codex:mail"), Some("2026-08-28"), false),
    )
    .unwrap();
    assert_eq!(stats.day_written, 1);
    let after = read_json(&path);
    assert_eq!(before["lockedTitle"], after["lockedTitle"]);
    assert_ne!(before["dialogFingerprint"], after["dialogFingerprint"]);
    corrected.messages[0].actions_before += 1;
    archive
        .db()
        .unwrap()
        .upsert_conversation(&corrected)
        .unwrap();
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &ScriptedClient::new(&[]),
        &options(Some("codex:mail"), Some("2026-08-28"), false),
    )
    .unwrap();
    assert_eq!(stats.day_written, 0);
}
