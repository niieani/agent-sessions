use agent_sessions::{
    archive::Archive,
    leads::{self, SessionLead},
    model::{Conversation, Message, MessageKind, Provenance, Role},
    render::render_all,
};
use serde_json::{Value, json};
use std::fs;

pub(super) fn temp_dir() -> tempfile::TempDir {
    let staging = std::path::PathBuf::from("temp.local")
        .join(time::OffsetDateTime::now_utc().date().to_string());
    fs::create_dir_all(&staging).unwrap();
    tempfile::tempdir_in(staging).unwrap()
}

pub(super) fn lead(requested_by: &str) -> SessionLead {
    serde_json::from_value(json!({
        "cursor":"delegation-42", "source":"codex", "sessionId":"worker",
        "ledBy":{"kind":"agent","name":"Example Assistant"},
        "origin":{"kind":"t3","machine":"workstation","t3ThreadId":"thread-42","requestedBy":requested_by,"runState":"running"},
        "recordedAt":"2026-09-01T03:00:00Z"
    })).unwrap()
}

pub(super) fn insert(archive: &Archive, source: &str, id: &str, timestamps: &[&str]) {
    let messages: Vec<_> = timestamps
        .iter()
        .map(|at| Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "request".into(),
            timestamp_ms: Some(agent_sessions::timefmt::parse_iso_ms(at).unwrap()),
            actions_before: 0,
        })
        .collect();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&Conversation {
            provenance: Provenance {
                source: source.into(),
                host: "fixture".into(),
                native_id: id.into(),
                native_path: format!("/fixture/{id}"),
            },
            title: None,
            project_path: None,
            native_project: None,
            created_at_ms: messages[0].timestamp_ms,
            updated_at_ms: messages.last().unwrap().timestamp_ms,
            messages,
            transcript_facts: Default::default(),
            subagent_parent: None,
            inherited_context: None,
            subagent_references: vec![],
            branches: Vec::new(),
            subagent_links: vec![],
            trailing_actions: 0,
        })
        .unwrap();
}

pub(super) const CONFIG: &str = r#"[views]
timezone = "UTC"
day-start = "04:00"
unit = "session-day"
[views.layout]
session = "notes/{{ source }} {{ workday }}.md"
loose = "notes/{{ source }} {{ workday }}.md"
json = "json/{{ short_id }} {{ workday }}.json"
"#;

pub(super) struct Fixture {
    pub(super) root: tempfile::TempDir,
    pub(super) archive: Archive,
}

impl Fixture {
    pub(super) fn new(requested_by: &str) -> Self {
        let root = temp_dir();
        let archive = Archive::open(root.path()).unwrap();
        insert(&archive, "codex", "worker", &["2026-09-01T12:00:00Z"]);
        let lead = lead(requested_by);
        leads::store(root.path(), &lead).unwrap();
        archive.db().unwrap().upsert_session_leads(&[lead]).unwrap();
        fs::write(root.path().join("config.toml"), CONFIG).unwrap();
        Self { root, archive }
    }

    pub(super) fn requester(&self) {
        insert(
            &self.archive,
            "claude",
            "requester",
            &["2026-08-31T12:00:00Z", "2026-09-01T12:00:00Z"],
        );
    }

    pub(super) fn context(&self) -> Value {
        self.context_matching(None)
    }

    pub(super) fn context_on(&self, day: &str) -> Value {
        self.context_matching(Some(day))
    }

    fn context_matching(&self, day: Option<&str>) -> Value {
        fs::read_dir(self.archive.views_dir().join("json"))
            .unwrap()
            .map(|entry| {
                serde_json::from_str::<Value>(&fs::read_to_string(entry.unwrap().path()).unwrap())
                    .unwrap()
            })
            .find(|v| {
                v["session"]["native_id"] == "worker"
                    && day.is_none_or(|day| v["day"]["workday"] == day)
            })
            .unwrap()
    }

    pub(super) fn note(&self) -> String {
        fs::read_to_string(self.archive.views_dir().join("notes/codex 2026-09-01.md")).unwrap()
    }

    pub(super) fn assert_unresolved(&self) {
        render_all(&self.archive).unwrap();
        assert!(self.context()["session"]["requested_by"].is_null());
        assert!(!self.note().contains("session-requested-by:"));
    }

    pub(super) fn digest(&self, source: &str, id: &str, day: &str, title: &str) {
        let dir = agent_sessions::digest::digest_dir(self.root.path(), source, id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{day}.json")),
            json!({"title":title,"lockedTitle":title}).to_string(),
        )
        .unwrap();
    }
}
