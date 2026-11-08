use agent_sessions::{
    archive::Archive,
    digest::{self, CompletionKind, DigestOptions, ModelClient, ModelRequest},
    model::{Conversation, Message, MessageKind, Provenance, Role},
};
use std::{collections::VecDeque, fs, sync::Mutex};

fn ms(s: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(s).unwrap()
}
fn options() -> DigestOptions {
    DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: false,
        now_ms: ms("2026-09-03T12:00:00Z"),
    }
}
struct Client {
    responses: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Client {
    fn new(responses: &[&str]) -> Self {
        Self {
            responses: Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
            requests: Mutex::new(vec![]),
        }
    }
}
impl ModelClient for Client {
    fn complete(&self, r: &ModelRequest) -> anyhow::Result<String> {
        self.requests.lock().unwrap().push(r.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("intentional model failure"))
    }
}
fn fixture() -> (tempfile::TempDir, Archive) {
    let root = tempfile::tempdir().unwrap();
    let a = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        r#"[views]
timezone="UTC"
[views.kind.loose]
match.path.glob="/temporary/**"
loose=true
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url="http://127.0.0.1:1/v1"
model="fake"
api-key-env="FAKE_KEY"
concurrency=1
retries=0
"#,
    )
    .unwrap();
    let mut c = Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: "loose-1".into(),
            native_path: "/absent".into(),
        },
        title: None,
        project_path: Some("/temporary/can-you".into()),
        native_project: None,
        created_at_ms: Some(ms("2026-08-30T12:00:00Z")),
        updated_at_ms: Some(ms("2026-08-31T12:00:00Z")),
        messages: vec![],
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: vec![],
        branches: Vec::new(),
        subagent_links: vec![],
        trailing_actions: 0,
    };
    for day in ["2026-08-30", "2026-08-31"] {
        for (role, text) in [
            (Role::User, "Build a floor plan viewer"),
            (Role::Assistant, "Implemented the floor plan viewer"),
        ] {
            c.messages.push(Message {
                role,
                kind: MessageKind::Dialog,
                text: text.into(),
                timestamp_ms: Some(ms(&format!("{day}T12:00:00Z"))),
                actions_before: 0,
            });
        }
    }
    a.db().unwrap().upsert_conversation(&c).unwrap();
    let client = Client::new(&[
        r#"{"topic":"Can Yo","title":"First","intent":"Do the work","tldr":null,"summary":"Built viewer.","classification":["build"]}"#,
        r#"{"title":"Second","intent":null,"tldr":null,"summary":"Improved viewer.","classification":["build"]}"#,
        r#"{"title":"Can Yo Viewer"}"#,
    ]);
    assert_eq!(
        digest::run_with_client(&a, &client, &options())
            .unwrap()
            .failed,
        0
    );
    (root, a)
}
fn repair_options() -> digest::RepairOptions {
    digest::RepairOptions {
        mode: digest::RepairMode::Loose,
        dry_run: false,
        force: false,
        now_ms: options().now_ms,
    }
}
const RESPONSES: [&str; 3] = [
    r#"{"topic":"Floor Plan Viewer","title":"First","intent":"Do the work","tldr":null,"summary":"Built viewer.","classification":["build"]}"#,
    r#"{"title":"Second","intent":null,"tldr":null,"summary":"Improved viewer.","classification":["build"]}"#,
    r#"{"title":"Floor Plan Viewer"}"#,
];

#[test]
fn repairs_all_topics_and_locked_titles_preserving_writeback_then_skips_repeat() {
    let (_root, a) = fixture();
    let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
    let mut old: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    old["pushed"] = serde_json::json!({"codex":"Old owned title"});
    fs::write(dir.join("session.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    fs::write(
        dir.join("push-title.pending"),
        b"preserve exact pending bytes",
    )
    .unwrap();
    let client = Client::new(&RESPONSES);
    let result = digest::repair_with_client(&a, &client, &repair_options()).unwrap();
    assert_eq!((result.repaired, result.failed), (1, 0));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].kind, CompletionKind::FirstSessionDay);
    assert!(!requests[0].user.contains("Can Yo"));
    assert!(!requests[1].user.contains("Can Yo"));
    assert!(requests[1].user.contains("Floor Plan Viewer"));
    for day in ["2026-08-30", "2026-08-31"] {
        let d = digest::read_day(&dir.join(format!("{day}.json")))
            .unwrap()
            .unwrap();
        assert!(d.title.starts_with("Floor Plan Viewer - "));
        assert_eq!(d.title, d.locked_title);
    }
    let s: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(s["topic"], "Floor Plan Viewer");
    assert_eq!(s["pushed"], old["pushed"]);
    assert_eq!(
        fs::read(dir.join("push-title.pending")).unwrap(),
        b"preserve exact pending bytes"
    );
    let repeat = digest::repair_with_client(&a, &Client::new(&[]), &repair_options()).unwrap();
    assert_eq!((repeat.repaired, repeat.skipped, repeat.failed), (0, 1, 0));
}

#[test]
fn partial_model_failure_keeps_entire_old_digest_set_and_reports_failure() {
    let (_root, a) = fixture();
    let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
    let files = ["session.json", "2026-08-30.json", "2026-08-31.json"];
    let old: Vec<_> = files
        .iter()
        .map(|p| fs::read(dir.join(p)).unwrap())
        .collect();
    let result =
        digest::repair_with_client(&a, &Client::new(&RESPONSES[..1]), &repair_options()).unwrap();
    assert_eq!((result.repaired, result.failed), (0, 1));
    for (p, before) in files.iter().zip(old) {
        assert_eq!(fs::read(dir.join(p)).unwrap(), before);
    }
    assert!(dir.join("repair.failed").is_file());
    let retried =
        digest::repair_with_client(&a, &Client::new(&RESPONSES), &repair_options()).unwrap();
    assert_eq!((retried.repaired, retried.failed), (1, 0));
}

#[test]
fn interrupted_publication_blocks_readers_then_resumes_without_model_requests() {
    let (_root, a) = fixture();
    digest::repair_with_client(&a, &Client::new(&RESPONSES), &repair_options()).unwrap();
    let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
    let read = |name: &str| {
        serde_json::from_slice::<serde_json::Value>(&fs::read(dir.join(name)).unwrap()).unwrap()
    };
    let days = serde_json::json!({"2026-08-30.json":read("2026-08-30.json"),"2026-08-31.json":read("2026-08-31.json")});
    let journal = serde_json::json!({"schema_version":2,"fingerprint":read("repair.completed"),"session":read("session.json"),"days":days});
    fs::write(
        dir.join("repair.pending"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("2026-08-31.json"),
        b"partially published old generation",
    )
    .unwrap();
    assert!(
        digest::read_day(&dir.join("2026-08-30.json"))
            .unwrap_err()
            .to_string()
            .contains("pending Digest repair")
    );
    assert!(
        agent_sessions::render::render_all(&a)
            .unwrap_err()
            .to_string()
            .contains("pending Digest repair")
    );
    let result = digest::repair_with_client(&a, &Client::new(&[]), &repair_options()).unwrap();
    assert_eq!((result.repaired, result.skipped, result.failed), (0, 1, 0));
    assert_eq!(read("2026-08-31.json"), days["2026-08-31.json"]);
    assert!(!dir.join("repair.pending").exists());
}

#[test]
fn rejected_output_is_preserved_and_session_title_failure_never_publishes_fallback() {
    for responses in [&["not valid JSON"][..], &RESPONSES[..2]] {
        let (_root, a) = fixture();
        let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
        let old = fs::read(dir.join("session.json")).unwrap();
        let stats =
            digest::repair_with_client(&a, &Client::new(responses), &repair_options()).unwrap();
        assert_eq!((stats.repaired, stats.failed), (0, 1));
        assert_eq!(fs::read(dir.join("session.json")).unwrap(), old);
        if responses.len() == 1 {
            assert!(
                fs::read_to_string(dir.join("2026-08-30.rejected.jsonl"))
                    .unwrap()
                    .contains("not valid JSON")
            );
        }
    }
}

#[test]
fn dry_run_does_not_change_durable_archive_files() {
    fn tree(path: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
        let mut out = vec![];
        for entry in fs::read_dir(path).unwrap() {
            let p = entry.unwrap().path();
            // SQLite read-only WAL connections may create coordination files.
            if p.file_name()
                .is_some_and(|name| name == "archive.db-wal" || name == "archive.db-shm")
            {
                continue;
            }
            if p.is_dir() {
                out.extend(tree(&p))
            } else {
                use sha2::{Digest as _, Sha256};
                out.push((
                    p.clone(),
                    format!("{:x}", Sha256::digest(fs::read(p).unwrap())),
                ));
            }
        }
        out.sort();
        out
    }
    let (_root, a) = fixture();
    let before = tree(a.root());
    let mut options = repair_options();
    options.dry_run = true;
    let stats = digest::repair_with_client(&a, &Client::new(&[]), &options).unwrap();
    assert_eq!((stats.selected, stats.repaired, stats.failed), (1, 0, 0));
    assert!(stats.input_tokens > 0);
    assert_eq!(tree(a.root()), before);
}

#[test]
fn project_prefix_cleanup_keeps_collision_resolved_filenames_and_topics_coherent() {
    let (_root, a) = fixture();
    let config = fs::read_to_string(a.root().join("config.toml"))
        .unwrap()
        .replace(
            "[views.kind.loose]\nmatch.path.glob=\"/temporary/**\"\nloose=true\n",
            "",
        )
        .replace("timezone=\"UTC\"", "timezone=\"UTC\"\nunit=\"session-day\"");
    fs::write(a.root().join("config.toml"),format!("{config}\n[views.project-names]\n\"/temporary/can-you\"=\"Example Assistant Service\"\n[views.layout]\nsession=\"notes/{{{{ workday }}}} {{{{ project }}}} - {{{{ title }}}}.md\"\n")).unwrap();
    let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
    for name in ["session.json", "2026-08-30.json", "2026-08-31.json"] {
        let path = dir.join(name);
        let mut d: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if !d["topic"].is_null() {
            d["topic"] = serde_json::json!("Example Assistant Service Config");
        }
        d["title"] = serde_json::json!(
            "Example Assistant Service Config - Fixed reasoningEffort max rejection"
        );
        if name != "session.json" {
            d["lockedTitle"] = d["title"].clone();
        }
        fs::write(path, serde_json::to_vec(&d).unwrap()).unwrap();
    }
    // A second identity with the same day/title exercises normal collision resolution.
    let db = a.db().unwrap();
    let row = db.sessions_with_dialog().unwrap().remove(0);
    let messages = db.messages(row.id).unwrap();
    drop(db);
    let c = Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: "loose-2".into(),
            native_path: "/absent2".into(),
        },
        title: None,
        project_path: row.project_path,
        native_project: None,
        created_at_ms: row.created_at_ms.map(|t| t + 1),
        updated_at_ms: row.updated_at_ms,
        messages,
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: vec![],
        branches: Vec::new(),
        subagent_links: vec![],
        trailing_actions: 0,
    };
    a.db().unwrap().upsert_conversation(&c).unwrap();
    let dir2 = digest::digest_dir(a.root(), "codex", "loose-2").unwrap();
    fs::create_dir_all(&dir2).unwrap();
    for name in ["session.json", "2026-08-30.json", "2026-08-31.json"] {
        let mut d: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(name)).unwrap()).unwrap();
        if name != "session.json" {
            d["nativeId"] = serde_json::json!("loose-2");
        }
        fs::write(dir2.join(name), serde_json::to_vec(&d).unwrap()).unwrap();
    }
    let mut options = repair_options();
    options.mode = digest::RepairMode::ProjectPrefixes;
    let stats = digest::repair_with_client(&a, &Client::new(&[]), &options).unwrap();
    assert_eq!(stats.repaired, 2);
    agent_sessions::render::render_all(&a).unwrap();
    let expected =
        "2026-08-30 Example Assistant Service - Config - Fixed reasoningEffort max rejection";
    assert!(
        a.views_dir()
            .join("notes")
            .join(format!("{expected}.md"))
            .is_file()
    );
    assert!(
        a.views_dir()
            .join("notes")
            .join(format!("{expected} (2).md"))
            .is_file()
    );
    let first = digest::read_day(&dir.join("2026-08-30.json"))
        .unwrap()
        .unwrap();
    assert_eq!(first.topic.as_deref(), Some("Config"));
    assert_eq!(
        digest::repair_with_client(&a, &Client::new(&[]), &options)
            .unwrap()
            .repaired,
        0
    );
}

#[test]
fn grown_day_avoids_repeating_project_without_changing_its_existing_lock() {
    let (_root, a) = fixture();
    let config = fs::read_to_string(a.root().join("config.toml"))
        .unwrap()
        .replace(
            "[views.kind.loose]\nmatch.path.glob=\"/temporary/**\"\nloose=true\n",
            "",
        );
    fs::write(
        a.root().join("config.toml"),
        format!("{config}\n[views.project-names]\n\"/temporary/can-you\"=\"Can Yo\"\n"),
    )
    .unwrap();
    let dir = digest::digest_dir(a.root(), "codex", "loose-1").unwrap();
    let path = dir.join("2026-08-30.json");
    let mut day: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let old_lock = day["lockedTitle"].clone();
    day["covered"]["lastIdx"] = serde_json::json!(0);
    fs::write(&path, serde_json::to_vec(&day).unwrap()).unwrap();
    let client = Client::new(&[
        r#"{"title":"Improved viewer","intent":"Do the work","tldr":null,"summary":"Built it.","classification":["build"]}"#,
        r#"{"title":"Viewer"}"#,
    ]);
    assert_eq!(
        digest::run_with_client(&a, &client, &options())
            .unwrap()
            .failed,
        0
    );
    let day = digest::read_day(&path).unwrap().unwrap();
    assert_eq!(day.title, "Improved viewer");
    assert_eq!(day.topic.as_deref(), Some("Can Yo"));
    assert_eq!(day.locked_title, old_lock.as_str().unwrap());
}

#[test]
fn repair_reports_progress_in_the_shared_status_file() {
    let (root, a) = fixture();
    let client = Client::new(&RESPONSES);
    digest::repair_with_client(&a, &client, &repair_options()).unwrap();
    let status = digest::progress::inspect(root.path()).unwrap().status;
    assert_eq!(status.command, "repair-digests");
    assert_eq!(status.state, digest::progress::RunState::Completed);
    // Two Session Days plus the Session title.
    assert_eq!((status.total, status.done, status.failed), (3, 3, 0));

    // An already-completed repair settles its planned work without requests.
    digest::repair_with_client(&a, &Client::new(&[]), &repair_options()).unwrap();
    let status = digest::progress::inspect(root.path()).unwrap().status;
    assert_eq!((status.total, status.done, status.failed), (3, 3, 0));
}

#[test]
fn successful_repair_settles_a_failed_session_title_refresh() {
    let (_root, archive) = fixture();
    let mut forced = options();
    forced.session = Some(digest::SessionKey::parse("codex:loose-1").unwrap());
    forced.day = Some("2026-08-31".into());
    forced.force = true;
    let failed = digest::run_with_client(&archive, &Client::new(&[RESPONSES[1]]), &forced).unwrap();
    assert_eq!(failed.failed, 1);
    let repaired =
        digest::repair_with_client(&archive, &Client::new(&RESPONSES), &repair_options()).unwrap();
    assert_eq!((repaired.repaired, repaired.failed), (1, 0));
    let next = digest::run_with_client(&archive, &Client::new(&[]), &options()).unwrap();
    assert_eq!((next.requests, next.failed), (0, 0));
}
