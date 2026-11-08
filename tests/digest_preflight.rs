//! `digest --dry-run` as a pre-flight check: planned work split into new and
//! redone Digests, an estimate from the last completed run's pace, a warning
//! when existing Digests are being invalidated, and safety against a live run.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};
use serde_json::Value;

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn message(text: &str, at: &str) -> Message {
    Message {
        role: Role::User,
        kind: MessageKind::Dialog,
        text: text.into(),
        timestamp_ms: Some(ms(at)),
        actions_before: 0,
    }
}

fn conversation(source: &str, id: &str, messages: Vec<Message>) -> Conversation {
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

/// Answers validly; optionally blocks the first request until released.
struct Client {
    hold: Option<(mpsc::Sender<()>, std::sync::Mutex<mpsc::Receiver<()>>)>,
}

impl ModelClient for Client {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        if let Some((started, release)) = &self.hold {
            let _ = started.send(());
            let _ = release.lock().unwrap().recv();
        }
        Ok(match request.kind {
            CompletionKind::SessionTitle => r#"{"title":"Preflight Work"}"#.into(),
            CompletionKind::FirstSessionDay => r#"{"topic":"Archive","title":"First pass","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#.into(),
            CompletionKind::ContinuationSessionDayWithoutIntent => r#"{"title":"Next pass","intent":"Do the work","tldr":null,"summary":"Built more.","classification":["build"]}"#.into(),
            CompletionKind::ContinuationSessionDay => r#"{"title":"Next pass","intent":null,"tldr":null,"summary":"Built more.","classification":["build"]}"#.into(),
        })
    }
}

const NOW: &str = "2026-09-03T10:00:00Z";

fn codex_one(changed: bool, extra_day: bool) -> Conversation {
    let mut messages = vec![
        message("codex work on 30th", "2026-08-30T10:00:00Z"),
        message(
            if changed {
                "codex work on 31st, revised"
            } else {
                "codex work on 31st"
            },
            "2026-08-31T10:00:00Z",
        ),
    ];
    if extra_day {
        messages.push(message("codex work on 1st", "2026-09-01T10:00:00Z"));
    }
    conversation("codex", "codex-one", messages)
}

fn claude_one(grown: bool) -> Conversation {
    let mut messages = vec![message("claude work", "2026-08-30T10:00:00Z")];
    if grown {
        messages.push(message("claude follow-up", "2026-08-30T11:00:00Z"));
    }
    conversation("claude", "claude-one", messages)
}

fn upsert(archive: &Archive, conversations: &[Conversation]) {
    let mut db = archive.db().unwrap();
    for conversation in conversations {
        db.upsert_conversation(conversation).unwrap();
    }
}

/// An Archive whose two Sessions were fully digested by a completed run.
fn digested() -> (tempfile::TempDir, Archive) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        "[views]\ntimezone = \"UTC\"\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url = \"http://127.0.0.1:1/v1\"\nmodel = \"fixture\"\napi-key-env = \"TEST_API_KEY\"\nconcurrency = 1\nretries = 1\n",
    )
    .unwrap();
    upsert(&archive, &[codex_one(false, false), claude_one(false)]);
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &Client { hold: None },
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms(NOW),
        },
    )
    .unwrap();
    assert_eq!((stats.day_written, stats.session_written), (3, 2));
    (root, archive)
}

/// After the digested run: one revised day, one grown day, two new days
/// (one in a brand-new Session).
fn mixed_changes(archive: &Archive) {
    upsert(
        archive,
        &[
            codex_one(true, true),
            claude_one(true),
            conversation(
                "claude",
                "claude-two",
                vec![message("claude second", "2026-09-01T10:00:00Z")],
            ),
        ],
    );
}

fn dry_run(root: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["digest", "--dry-run", "--archive"])
        .arg(root)
        .args(extra)
        .output()
        .unwrap()
}

fn dry_run_json(root: &Path, extra: &[&str]) -> (Value, String) {
    let mut args = vec!["--json"];
    args.extend_from_slice(extra);
    let output = dry_run(root, &args);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    (serde_json::from_slice(&output.stdout).unwrap(), stderr)
}

/// Rewrite the completed run's timing to a known pace.
fn set_last_run(root: &Path, started: &str, finished: &str, done: usize) {
    let path = root.join("digest-status.json");
    let mut status: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(status["state"], "completed");
    status["started_at"] = started.into();
    status["updated_at"] = finished.into();
    status["done"] = done.into();
    fs::write(&path, serde_json::to_vec(&status).unwrap()).unwrap();
}

#[test]
fn dry_run_splits_new_from_redo_by_reason_and_source() {
    let (root, archive) = digested();
    mixed_changes(&archive);
    let (summary, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "1"]);

    // New: codex 2026-09-01, claude-two's day and its title.
    assert_eq!(summary["new"], 3);
    assert_eq!(summary["new_days"], 2);
    assert_eq!(summary["new_titles"], 1);
    // Redo: codex's revised day, claude-one's grown day, and both existing
    // Sessions' titles.
    assert_eq!(summary["redo"], 4);
    assert_eq!(summary["redo_days"], 2);
    assert_eq!(
        summary["redo_reasons"],
        serde_json::json!({"dialog_changed": 1, "range_grew": 1, "title_refresh": 2})
    );
    assert_eq!(summary["existing_day_digests"], 3);
    assert_eq!(summary["fingerprints"], 0);
    assert!(summary["input_tokens"].as_u64().unwrap() > 0);

    let codex = &summary["sources"]["codex"];
    assert_eq!((&codex["new"], &codex["redo"]), (&1.into(), &2.into()));
    assert_eq!(
        codex["redo_reasons"],
        serde_json::json!({"dialog_changed": 1, "title_refresh": 1})
    );
    let claude = &summary["sources"]["claude"];
    assert_eq!((&claude["new"], &claude["redo"]), (&2.into(), &2.into()));
    assert_eq!(summary["warning"], Value::Null);
}

#[test]
fn estimate_uses_the_last_completed_runs_pace() {
    let (root, archive) = digested();
    mixed_changes(&archive);
    // Ten items in two hours: five per hour. Seven planned items take 1h24m.
    set_last_run(
        root.path(),
        "2026-09-03T08:00:00.000Z",
        "2026-09-03T10:00:00.000Z",
        10,
    );
    let (summary, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "1"]);
    let estimate = &summary["estimate"];
    assert_eq!(estimate["basis"], "last_run");
    assert_eq!(estimate["items"], 7);
    assert_eq!(estimate["rate_per_hour"], 5.0);
    assert_eq!(estimate["duration_seconds"], 7 * 3600 / 5);
    assert_eq!(estimate["run"]["command"], "digest");

    let output = dry_run(root.path(), &["--redo-warn-fraction", "1"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let last = stdout.lines().last().unwrap();
    assert_eq!(
        last,
        "digest dry-run: 3 new · 4 redo (range grew 1, dialog changed 1, title refresh 2) \
         · 0 fingerprints · ~1h24m at last run's pace (claude 2/2, codex 1/2)"
    );
    assert!(stdout.contains("dry-run codex: 1 new · 2 redo (dialog changed 1, title refresh 1)"));
}

#[test]
fn without_a_completed_run_the_estimate_falls_back_to_tokens() {
    let (root, archive) = digested();
    fs::remove_file(root.path().join("digest-status.json")).unwrap();
    mixed_changes(&archive);
    let (summary, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "1"]);
    let estimate = &summary["estimate"];
    assert_eq!(estimate["basis"], "token_count");
    assert_eq!(estimate["duration_seconds"], Value::Null);
    assert!(
        estimate["reason"]
            .as_str()
            .unwrap()
            .contains("digest-status.json")
    );

    let stdout = String::from_utf8(dry_run(root.path(), &[]).stdout).unwrap();
    let last = stdout.lines().last().unwrap();
    assert!(last.contains("no rate history"), "{last}");
    assert!(last.contains("input tokens"), "{last}");
}

#[test]
fn a_corrupt_status_file_is_a_hard_error() {
    let (root, _archive) = digested();
    fs::write(root.path().join("digest-status.json"), "{").unwrap();
    let output = dry_run(root.path(), &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("digest-status.json")
    );
}

#[test]
fn redo_beyond_new_work_warns_without_aborting() {
    let (root, archive) = digested();
    upsert(&archive, &[codex_one(true, false)]);
    let (summary, stderr) = dry_run_json(root.path(), &["--redo-warn-fraction", "1"]);
    assert_eq!(
        (&summary["new"], &summary["redo_days"]),
        (&0.into(), &1.into())
    );
    let warning = summary["warning"].as_str().unwrap();
    assert!(warning.contains("invalidat"), "{warning}");
    assert!(stderr.contains("WARN"), "{stderr}");
}

#[test]
fn redo_beyond_the_configured_fraction_of_existing_digests_warns() {
    let (root, archive) = digested();
    // Two new days, one redone day out of three existing Day Digests.
    upsert(
        &archive,
        &[
            codex_one(true, true),
            conversation(
                "claude",
                "claude-two",
                vec![message("claude second", "2026-09-01T10:00:00Z")],
            ),
        ],
    );
    let (quiet, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "0.5"]);
    assert_eq!(quiet["warning"], Value::Null);
    let (loud, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "0.25"]);
    assert!(loud["warning"].as_str().unwrap().contains("25%"));
    // The default fraction is strict enough to flag a third of the Archive.
    let (default, _) = dry_run_json(root.path(), &[]);
    assert!(default["warning"].is_string());
}

#[test]
fn an_out_of_range_warn_fraction_is_rejected() {
    let (root, _archive) = digested();
    let output = dry_run(root.path(), &["--redo-warn-fraction", "1.5"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("redo-warn-fraction")
    );
}

#[test]
fn dry_run_is_safe_beside_a_live_digest_run() {
    let (root, archive) = digested();
    mixed_changes(&archive);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let live_root = root.path().to_path_buf();
    let live = std::thread::spawn(move || {
        let archive = Archive::open(&live_root).unwrap();
        let client = Client {
            hold: Some((started_tx, std::sync::Mutex::new(release_rx))),
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
                now_ms: ms(NOW),
            },
        )
    });
    // The live run is mid-request: it holds the Archive writer lock and the
    // status lock, and its status file says running.
    started_rx.recv().unwrap();
    assert!(archive.acquire_writer_lock().is_err());
    let status_before = fs::read(root.path().join("digest-status.json")).unwrap();
    assert!(String::from_utf8_lossy(&status_before).contains("\"running\""));
    let digests_before = tree(&root.path().join("digests"));

    let (summary, _) = dry_run_json(root.path(), &["--redo-warn-fraction", "1"]);
    assert_eq!((&summary["new"], &summary["redo"]), (&3.into(), &4.into()));
    assert_eq!(summary["estimate"]["basis"], "token_count");
    assert!(
        summary["estimate"]["reason"]
            .as_str()
            .unwrap()
            .contains("running")
    );
    assert_eq!(
        fs::read(root.path().join("digest-status.json")).unwrap(),
        status_before
    );
    assert_eq!(tree(&root.path().join("digests")), digests_before);

    drop(release_tx);
    let stats = live.join().unwrap().unwrap();
    assert_eq!((stats.failed, stats.day_written), (0, 4));
}

fn tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(tree(&path));
        } else {
            let bytes = fs::read(&path).unwrap();
            found.push((path, bytes));
        }
    }
    found.sort();
    found
}

#[test]
fn title_tokens_reach_library_json_and_text_preflight_totals() {
    for mixed in [false, true] {
        let (root, archive) = digested();
        let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "codex-one").unwrap();
        fs::write(
            dir.join("session-title.pending"),
            r#"{"source":"codex","nativeId":"codex-one"}"#,
        )
        .unwrap();
        if mixed {
            upsert(&archive, &[codex_one(false, true)]);
        }
        let stats = agent_sessions::digest::run_with_client(
            &archive,
            &Client { hold: None },
            &DigestOptions {
                session: Some(
                    agent_sessions::digest::SessionKey::parse("codex:codex-one").unwrap(),
                ),
                force: false,
                day: None,
                from: None,
                through: None,
                dry_run: true,
                now_ms: ms(NOW),
            },
        )
        .unwrap();
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.previews.len(), usize::from(mixed));
        let day_tokens: usize = stats.previews.iter().map(|p| p.input_tokens).sum();
        assert!(stats.input_tokens > day_tokens);
        assert_eq!(stats.plan.sources["codex"].input_tokens, stats.input_tokens);
        let (summary, _) = dry_run_json(root.path(), &["--session", "codex:codex-one"]);
        assert_eq!(summary["input_tokens"], stats.input_tokens);
        assert_eq!(
            summary["sources"]["codex"]["input_tokens"],
            stats.input_tokens
        );
        let text = dry_run(root.path(), &["--session", "codex:codex-one"]);
        assert!(text.status.success());
        assert!(
            String::from_utf8(text.stdout)
                .unwrap()
                .contains(&format!("{} input tokens", stats.input_tokens))
        );
    }
}
