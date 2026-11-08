use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use agent_sessions::archive::Archive;
use agent_sessions::digest::{PromptSource, SessionDigest, digest_dir};
use agent_sessions::model::{
    Conversation, Message, MessageKind, Provenance, Role, TranscriptFacts,
};
use rusqlite::Connection;

pub const HOST: &str = "fixture-host";
pub const CODEX_FRESH: &str = "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa";
pub const CODEX_OWN: &str = "bbbbbbbb-bbbb-7bbb-8bbb-bbbbbbbbbbbb";
pub const CODEX_FOREIGN: &str = "cccccccc-cccc-7ccc-8ccc-cccccccccccc";
pub const CLAUDE_FRESH: &str = "11111111-1111-4111-8111-111111111111";
pub const CLAUDE_OWN: &str = "22222222-2222-4222-8222-222222222222";
pub const CLAUDE_FOREIGN: &str = "33333333-3333-4333-8333-333333333333";

pub struct Fixture {
    pub archive_dir: tempfile::TempDir,
    pub stores_dir: tempfile::TempDir,
    pub codex_root: PathBuf,
    pub claude_root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let archive_dir = tempfile::tempdir().unwrap();
        let stores_dir = tempfile::tempdir().unwrap();
        let codex_root = stores_dir.path().join("codex");
        let claude_root = stores_dir.path().join("claude-projects");
        fs::create_dir_all(codex_root.join("sessions/2026/03/05")).unwrap();
        fs::create_dir_all(claude_root.join("-work-project")).unwrap();
        let state = Connection::open(codex_root.join("state_5.sqlite")).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, name TEXT, title TEXT, marker TEXT);",
            )
            .unwrap();
        for (id, name) in [
            (CODEX_FRESH, None),
            (CODEX_OWN, Some("Old Codex")),
            (CODEX_FOREIGN, Some("Human Codex")),
        ] {
            state
                .execute(
                    "INSERT INTO threads (id, name, title, marker) VALUES (?1, ?2, 'raw', 'keep')",
                    (id, name),
                )
                .unwrap();
            write_codex_rollout(&codex_root, id);
        }
        drop(state);

        write_claude_session(&claude_root, CLAUDE_FRESH, None);
        write_claude_session(&claude_root, CLAUDE_OWN, Some("Old Claude"));
        write_claude_session(&claude_root, CLAUDE_FOREIGN, Some("Human Claude"));
        fs::write(
            archive_dir.path().join("config.toml"),
            format!(
                "[[host]]\nname = {HOST:?}\nclaude-root = {claude:?}\ncodex-root = {codex:?}\n[views]\ntimezone = 'UTC'\n",
                claude = claude_root,
                codex = codex_root,
            ),
        )
        .unwrap();

        let archive = Archive::open(archive_dir.path()).unwrap();
        let mut db = archive.db().unwrap();
        for (source, id, native_path, desired, pushed) in [
            (
                "codex",
                CODEX_FRESH,
                codex_path(&codex_root, CODEX_FRESH),
                "Codex Fresh",
                None,
            ),
            (
                "codex",
                CODEX_OWN,
                codex_path(&codex_root, CODEX_OWN),
                "Codex Updated",
                Some("Old Codex"),
            ),
            (
                "codex",
                CODEX_FOREIGN,
                codex_path(&codex_root, CODEX_FOREIGN),
                "Codex Rejected",
                None,
            ),
            (
                "claude",
                CLAUDE_FRESH,
                claude_path(&claude_root, CLAUDE_FRESH),
                "Claude Fresh",
                None,
            ),
            (
                "claude",
                CLAUDE_OWN,
                claude_path(&claude_root, CLAUDE_OWN),
                "Claude Updated",
                Some("Old Claude"),
            ),
            (
                "claude",
                CLAUDE_FOREIGN,
                claude_path(&claude_root, CLAUDE_FOREIGN),
                "Claude Rejected",
                None,
            ),
        ] {
            db.upsert_conversation(&conversation(source, id, &native_path))
                .unwrap();
            write_session_digest(archive_dir.path(), source, id, desired, pushed);
        }
        drop(db);
        Self {
            archive_dir,
            stores_dir,
            codex_root,
            claude_root,
        }
    }

    pub fn archive(&self) -> Archive {
        Archive::open_existing(self.archive_dir.path()).unwrap()
    }
}

fn conversation(source: &str, id: &str, path: &Path) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: source.into(),
            host: HOST.into(),
            native_id: id.into(),
            native_path: path.to_string_lossy().into_owned(),
        },
        title: None,
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: Some(1),
        updated_at_ms: Some(2),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "fixture".into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        }],
        transcript_facts: TranscriptFacts::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

pub fn write_session_digest(
    root: &Path,
    source: &str,
    id: &str,
    title: &str,
    pushed: Option<&str>,
) {
    let dir = digest_dir(root, source, id).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let mut pushed_values = BTreeMap::new();
    if let Some(value) = pushed {
        pushed_values.insert(source.to_string(), value.to_string());
    }
    let digest = SessionDigest {
        schema_version: 1,
        topic: "Fixture".into(),
        title: title.into(),
        updated_at_ms: 1,
        model: "fixture".into(),
        prompt_version: 1,
        prompt_source: PromptSource::Builtin,
        pushed: pushed_values,
    };
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec_pretty(&digest).unwrap(),
    )
    .unwrap();
}

pub fn read_pushed(root: &Path, source: &str, id: &str) -> Option<String> {
    let path = digest_dir(root, source, id).unwrap().join("session.json");
    let digest: SessionDigest = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    digest.pushed.get(source).cloned()
}

fn codex_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!(
        "sessions/2026/03/05/rollout-2026-03-05T09-00-00-{id}.jsonl"
    ))
}

fn write_codex_rollout(root: &Path, id: &str) {
    let path = codex_path(root, id);
    fs::write(
        path,
        format!(
            "{{\"timestamp\":\"2026-03-05T09:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":{id:?},\"timestamp\":\"2026-03-05T09:00:00.000Z\",\"cwd\":\"/work/project\",\"source\":\"cli\",\"thread_source\":\"user\"}}}}\n{{\"timestamp\":\"2026-03-05T09:00:01.000Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"fixture\"}}}}\n"
        ),
    )
    .unwrap();
}

pub fn claude_path(root: &Path, id: &str) -> PathBuf {
    root.join("-work-project").join(format!("{id}.jsonl"))
}

fn write_claude_session(root: &Path, id: &str, custom_title: Option<&str>) {
    let path = claude_path(root, id);
    let mut records = format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"fixture\"}},\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"timestamp\":\"2026-03-05T09:00:00.000Z\",\"cwd\":\"/work/project\",\"sessionId\":{id:?}}}\n"
    );
    if let Some(title) = custom_title {
        records.push_str(&format!(
            "{{\"type\":\"custom-title\",\"customTitle\":{title:?},\"sessionId\":{id:?}}}\n"
        ));
    }
    fs::write(path, records).unwrap();
}

pub fn append_claude_title(path: &Path, id: &str, title: &str) {
    let record = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": id,
    });
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(format!("{record}\n").as_bytes())
        .unwrap();
}

pub fn write_claude_pending(fixture: &Fixture, id: &str, title: &str, temp: Option<&str>) {
    write_claude_pending_at(fixture, id, title, temp, None);
}

pub fn write_claude_pending_at(
    fixture: &Fixture,
    id: &str,
    title: &str,
    temp: Option<&str>,
    checked_len: Option<u64>,
) {
    let digest_dir = claude_pending_dir(fixture, id);
    fs::write(
        digest_dir.join("push-title.pending"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "source": "claude",
            "nativeId": id,
            "previous": null,
            "title": title,
            "claudeCheckedLen": checked_len,
        }))
        .unwrap(),
    )
    .unwrap();
    if let Some(temp) = temp {
        let sidecar_dir = fixture.claude_root.join("-work-project").join(id);
        fs::create_dir(&sidecar_dir).unwrap();
        fs::write(
            sidecar_dir.join("custom-title.json.agent-sessions.tmp"),
            serde_json::to_vec(&serde_json::json!({"customTitle": temp})).unwrap(),
        )
        .unwrap();
    }
}

pub fn claude_pending_dir(fixture: &Fixture, id: &str) -> PathBuf {
    digest_dir(fixture.archive_dir.path(), "claude", id).unwrap()
}

pub fn claude_sidecar_temp(fixture: &Fixture, id: &str) -> PathBuf {
    fixture
        .claude_root
        .join("-work-project")
        .join(id)
        .join("custom-title.json.agent-sessions.tmp")
}

pub fn last_custom_title(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["type"] == "custom-title")
        .filter_map(|record| record["customTitle"].as_str().map(str::to_string))
        .next_back()
}

pub fn custom_title_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["type"] == "custom-title")
        .count()
}

pub fn read_claude_sidecar(root: &Path, id: &str) -> String {
    let path = root
        .join("-work-project")
        .join(id)
        .join("custom-title.json");
    serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap()["customTitle"]
        .as_str()
        .unwrap()
        .to_string()
}
