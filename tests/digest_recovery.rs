use std::fs;
use std::sync::Mutex;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn message(text: &str, timestamp: &str) -> Message {
    Message {
        role: Role::User,
        kind: MessageKind::Dialog,
        text: text.into(),
        timestamp_ms: Some(ms(timestamp)),
        actions_before: 0,
    }
}

fn conversation(messages: Vec<Message>) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: "recover".into(),
            native_path: "/store/recover".into(),
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

struct Client {
    title: Mutex<String>,
}

impl ModelClient for Client {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        Ok(match request.kind {
            CompletionKind::FirstSessionDay => {
                r#"{"topic":"Repair","title":"Generated later","intent":"Do the work","tldr":null,"summary":"Generated later.","classification":["fix"]}"#.into()
            }
            CompletionKind::ContinuationSessionDay => {
                r#"{"title":"Added earlier","intent":null,"tldr":null,"summary":"Added earlier.","classification":["fix"]}"#.into()
            }
            CompletionKind::ContinuationSessionDayWithoutIntent => {
                r#"{"title":"Added earlier","intent":"Do the work","tldr":null,"summary":"Added earlier.","classification":["fix"]}"#.into()
            }
            CompletionKind::SessionTitle => self.title.lock().unwrap().clone(),
        })
    }
}

fn setup() -> (tempfile::TempDir, Archive, Client) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        "[views]\ntimezone='UTC'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='http://127.0.0.1:1/v1'\nmodel='fixture'\napi-key-env='TEST_API_KEY'\nconcurrency=1\nmax-input-tokens=60000\nrequest-timeout='1s'\n",
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(vec![
            message("earlier", "2026-08-30T10:00:00Z"),
            message("later", "2026-08-31T10:00:00Z"),
        ]))
        .unwrap();
    (
        root,
        archive,
        Client {
            title: Mutex::new(r#"{"title":"Repair Session Digest Work"}"#.into()),
        },
    )
}

fn run_day(archive: &Archive, client: &Client, day: &str) -> agent_sessions::digest::DigestStats {
    agent_sessions::digest::run_with_client(
        archive,
        client,
        &DigestOptions {
            session: None,
            force: false,
            day: Some(day.into()),
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap()
}

#[test]
fn missing_session_digest_is_repaired_from_current_day_digest() {
    let (root, archive, client) = setup();
    run_day(&archive, &client, "2026-08-31");
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "recover").unwrap();
    fs::remove_file(dir.join("session.json")).unwrap();

    let stats = run_day(&archive, &client, "2026-08-31");

    assert_eq!(
        (stats.day_written, stats.session_written, stats.requests),
        (0, 1, 1)
    );
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(session["topic"], "Repair");
}

#[test]
fn invalid_existing_session_title_is_refreshed_without_redigesting_the_day() {
    let (root, archive, client) = setup();
    run_day(&archive, &client, "2026-08-31");
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "recover").unwrap();
    let path = dir.join("session.json");
    let mut session: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    session["title"] = "x".repeat(81).into();
    fs::write(&path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();

    let stats = run_day(&archive, &client, "2026-08-31");

    assert_eq!(
        (stats.day_written, stats.session_written, stats.requests),
        (0, 1, 1)
    );
    let repaired: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(repaired["title"], "Repair Session Digest Work");
}

#[test]
fn failed_session_title_is_retried_without_regenerating_fresh_days() {
    let (root, archive, client) = setup();
    client
        .title
        .lock()
        .unwrap()
        .clone_from(&r#"{"title":""}"#.into());

    let later = run_day(&archive, &client, "2026-08-31");
    assert_eq!(later.failed, 1);
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "recover").unwrap();
    assert!(!dir.join("session.json").exists());
    let day_before = fs::read(dir.join("2026-08-31.json")).unwrap();
    *client.title.lock().unwrap() = r#"{"title":"Recovered title"}"#.into();
    let retried = run_day(&archive, &client, "2026-08-31");
    assert_eq!(
        (
            retried.day_written,
            retried.session_written,
            retried.requests,
            retried.failed
        ),
        (0, 1, 1, 0)
    );
    assert_eq!(fs::read(dir.join("2026-08-31.json")).unwrap(), day_before);
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(session["title"], "Recovered title");
    assert_eq!(run_day(&archive, &client, "2026-08-31").requests, 0);
}

#[test]
fn failed_title_refresh_preserves_existing_title_and_remains_retryable() {
    let (root, archive, client) = setup();
    run_day(&archive, &client, "2026-08-30");
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "recover").unwrap();
    let before = fs::read(dir.join("session.json")).unwrap();
    *client.title.lock().unwrap() = r#"{"title":""}"#.into();
    assert_eq!(run_day(&archive, &client, "2026-08-31").failed, 1);
    assert_eq!(fs::read(dir.join("session.json")).unwrap(), before);
    let day = fs::read(dir.join("2026-08-31.json")).unwrap();
    *client.title.lock().unwrap() = r#"{"title":"Successful refresh"}"#.into();
    let retried = run_day(&archive, &client, "2026-08-31");
    assert_eq!(
        (
            retried.day_written,
            retried.session_written,
            retried.requests,
            retried.failed
        ),
        (0, 1, 1, 0)
    );
    assert_eq!(fs::read(dir.join("2026-08-31.json")).unwrap(), day);
    assert_eq!(run_day(&archive, &client, "2026-08-31").requests, 0);
}

#[cfg(unix)]
#[test]
fn digest_write_rejects_symlinked_directory_ancestors() {
    let (root, archive, client) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("digests")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("digests/codex")).unwrap();

    let stats = run_day(&archive, &client, "2026-08-31");

    assert_eq!(stats.failed, 1);
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn digest_write_rejects_a_symlinked_digest_root() {
    let (root, archive, client) = setup();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("digests")).unwrap();

    let stats = run_day(&archive, &client, "2026-08-31");

    assert_eq!(stats.failed, 1);
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}
