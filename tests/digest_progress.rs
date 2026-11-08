//! Live progress of long Digest runs: planned work by Source, the
//! `digest-status.json` status file, and stale-run detection.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_sessions::archive::Archive;
use agent_sessions::digest::progress::{self, Liveness, RunState};
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn conversation(source: &str, id: &str, days: &[&str], text: &str) -> Conversation {
    let messages = days
        .iter()
        .map(|day| Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: format!("{text} on {day}"),
            timestamp_ms: Some(ms(&format!("{day}T10:00:00Z"))),
            actions_before: 0,
        })
        .collect::<Vec<_>>();
    Conversation {
        provenance: Provenance {
            source: source.into(),
            host: "test".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}"),
        },
        title: None,
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: messages.first().and_then(|m| m.timestamp_ms),
        updated_at_ms: messages.last().and_then(|m| m.timestamp_ms),
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

/// Answers every request validly unless `respond` overrides it, after
/// running `observe` with the request's ordinal.
struct HookClient<O, R> {
    calls: AtomicUsize,
    observe: O,
    respond: R,
}

fn valid_output(request: &ModelRequest) -> String {
    match request.kind {
        CompletionKind::SessionTitle => r#"{"title":"Progress Work"}"#.into(),
        CompletionKind::FirstSessionDay => r#"{"topic":"Archive","title":"First pass","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#.into(),
        _ => r#"{"title":"Next pass","intent":null,"tldr":null,"summary":"Built more.","classification":["build"]}"#.into(),
    }
}

impl<O, R> ModelClient for HookClient<O, R>
where
    O: Fn(usize) + Send + Sync,
    R: Fn(usize, &ModelRequest) -> Option<anyhow::Result<String>> + Send + Sync,
{
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        (self.observe)(call);
        (self.respond)(call, request).unwrap_or_else(|| Ok(valid_output(request)))
    }
}

fn client<O, R>(observe: O, respond: R) -> HookClient<O, R>
where
    O: Fn(usize) + Send + Sync,
    R: Fn(usize, &ModelRequest) -> Option<anyhow::Result<String>> + Send + Sync,
{
    HookClient {
        calls: AtomicUsize::new(0),
        observe,
        respond,
    }
}

fn fixture(concurrency: usize, interval: &str) -> (tempfile::TempDir, Archive) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        format!(
            "[views]\ntimezone = \"UTC\"\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url = \"http://127.0.0.1:1/v1\"\nmodel = \"fixture\"\napi-key-env = \"TEST_API_KEY\"\nconcurrency = {concurrency}\nretries = 1\nprogress-interval = \"{interval}\"\n"
        ),
    )
    .unwrap();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&conversation(
        "codex",
        "codex-one",
        &["2026-08-30", "2026-08-31"],
        "codex work",
    ))
    .unwrap();
    db.upsert_conversation(&conversation(
        "claude",
        "claude-one",
        &["2026-08-30"],
        "claude work",
    ))
    .unwrap();
    (root, archive)
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

fn status_json(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join("digest-status.json")).unwrap()).unwrap()
}

#[test]
fn completed_run_leaves_final_status_split_by_source() {
    let (root, archive) = fixture(2, "5m");
    let client = client(|_| {}, |_, _| None);
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!(stats.failed, 0);

    let report = progress::inspect(root.path()).unwrap();
    assert_eq!(report.liveness, Liveness::Finished);
    let status = report.status;
    assert_eq!(status.command, "digest");
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.state, RunState::Completed);
    // Three Session Days plus one Session title per Session.
    assert_eq!((status.total, status.done, status.failed), (5, 5, 0));
    assert_eq!(
        (status.sources["codex"].total, status.sources["codex"].done),
        (3, 3)
    );
    assert_eq!(
        (
            status.sources["claude"].total,
            status.sources["claude"].done
        ),
        (2, 2)
    );
    assert!(status.current.is_none());
    assert!(status.error.is_none());
}

#[test]
fn running_status_reports_live_progress_with_current_item() {
    let (root, archive) = fixture(1, "5m");
    let seen = Mutex::new(Vec::new());
    let client = client(
        |call| {
            if call == 3 {
                seen.lock()
                    .unwrap()
                    .push(progress::inspect(root.path()).unwrap());
            }
        },
        |_, _| None,
    );
    agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    let seen = seen.into_inner().unwrap();
    let report = &seen[0];
    assert_eq!(report.liveness, Liveness::Live);
    assert_eq!(report.status.state, RunState::Running);
    assert_eq!(report.status.total, 5);
    assert_eq!(report.status.done, 2);
    let current = report.status.current.as_ref().unwrap();
    assert!(["codex", "claude"].contains(&current.source.as_str()));
    assert!(current.workday.starts_with("2026-08-3"));
}

#[test]
fn rejections_and_abandoned_session_items_are_counted() {
    let (root, archive) = fixture(1, "5m");
    let rejected_once = AtomicUsize::new(0);
    let client = client(
        |_| {},
        |_, request| {
            if request.user.contains("codex work") {
                return Some(Err(anyhow::anyhow!("model unavailable")));
            }
            (request.user.contains("claude work")
                && rejected_once.fetch_add(1, Ordering::SeqCst) == 0)
                .then(|| Ok("not json".to_string()))
        },
    );
    let stats = agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!(stats.failed, 1);
    let status = progress::inspect(root.path()).unwrap().status;
    assert_eq!(status.state, RunState::Completed);
    assert_eq!((status.total, status.done), (5, 5));
    // The failing codex Session abandons both days and its title.
    assert_eq!(status.failed, 3);
    assert_eq!(status.sources["codex"].failed, 3);
    assert_eq!(status.sources["claude"].failed, 0);
    assert_eq!(status.rejected, 1);
}

#[test]
fn heartbeat_refreshes_status_while_a_request_is_slow() {
    let (root, archive) = fixture(1, "20ms");
    let samples = Mutex::new(Vec::new());
    let client = client(
        |call| {
            if call == 1 {
                for _ in 0..2 {
                    samples
                        .lock()
                        .unwrap()
                        .push(status_json(root.path())["updated_at"].clone());
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }
        },
        |_, _| None,
    );
    agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    let samples = samples.into_inner().unwrap();
    assert_ne!(samples[0], samples[1]);
}

#[test]
fn dry_run_writes_no_status() {
    let (root, archive) = fixture(1, "5m");
    let options = DigestOptions {
        dry_run: true,
        ..options()
    };
    agent_sessions::digest::run_with_client(&archive, &client(|_| {}, |_, _| None), &options)
        .unwrap();
    assert!(!root.path().join("digest-status.json").exists());
}

#[test]
fn zero_progress_interval_is_rejected() {
    let (_root, archive) = fixture(1, "0s");
    let error =
        agent_sessions::digest::run_with_client(&archive, &client(|_| {}, |_, _| None), &options())
            .unwrap_err();
    assert!(format!("{error:#}").contains("progress-interval"));
}

#[test]
fn status_left_running_by_a_dead_writer_is_stale() {
    let (root, archive) = fixture(2, "5m");
    agent_sessions::digest::run_with_client(&archive, &client(|_| {}, |_, _| None), &options())
        .unwrap();
    let mut status = status_json(root.path());
    status["state"] = "running".into();
    status["pid"] = 999_999.into();
    fs::write(
        root.path().join("digest-status.json"),
        serde_json::to_vec(&status).unwrap(),
    )
    .unwrap();
    assert_eq!(
        progress::inspect(root.path()).unwrap().liveness,
        Liveness::Dead
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["digest", "status", "--archive"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("999999"));
}

#[test]
fn status_command_reports_a_finished_run_and_requires_a_status_file() {
    let (root, archive) = fixture(2, "5m");
    let status = || {
        Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(["digest", "status", "--archive"])
            .arg(root.path())
            .output()
            .unwrap()
    };
    assert!(!status().status.success());
    agent_sessions::digest::run_with_client(&archive, &client(|_| {}, |_, _| None), &options())
        .unwrap();
    let output = status();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("5/5"));
}
