use agent_sessions::{
    archive::Archive,
    model::{Conversation, Message, MessageKind, Provenance, Role, SubagentReference},
    render::render_all,
};
use std::{fs, path::Path};
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

fn conversation(id: &str, created: &str, messages: Vec<Message>) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "raw-host".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}.jsonl"),
        },
        title: Some(format!("Native {id}")),
        project_path: Some("/work/raw-project".into()),
        native_project: None,
        created_at_ms: Some(ms(created)),
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

fn insert(archive: &Archive, conversation: &Conversation) {
    archive
        .db()
        .unwrap()
        .upsert_conversation(conversation)
        .unwrap();
}

fn write_config(root: &Path, text: &str) {
    fs::write(root.join("config.toml"), text).unwrap();
}

#[test]
fn hierarchy_resolves_parent_evidence_and_crossing_turn_without_inventing_unknown_day() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "parent",
        "2026-08-31T03:00:00Z",
        vec![
            message(Role::User, "first", "2026-08-31T03:00:00Z"),
            message(Role::Assistant, "reply", "2026-08-31T05:00:00Z"),
            message(Role::User, "next", "2026-09-01T10:00:00Z"),
        ],
    );
    for (id, at) in [
        ("child", Some(ms("2026-08-31T05:01:00Z"))),
        ("unknown", None),
    ] {
        parent.subagent_references.push(SubagentReference {
            child_native_id: Some(id.into()),
            child_native_key: None,
            link_key: Some(id.into()),
            description: Some("Worker".into()),
            agent_type: Some("worker".into()),
            instructions: None,
            spawned_at_ms: at,
        });
    }
    insert(
        &archive,
        &conversation(
            "aaa",
            "2026-08-30T10:00:00Z",
            vec![message(Role::User, "collision", "2026-08-30T10:00:00Z")],
        ),
    );
    insert(&archive, &parent);
    for id in ["child", "unknown"] {
        insert(
            &archive,
            &conversation(
                id,
                "2026-08-31T05:01:00Z",
                vec![
                    message(Role::Assistant, "work", "2026-08-31T05:01:00Z"),
                    message(Role::User, "continue", "2026-09-01T11:00:00Z"),
                ],
            ),
        );
    }
    let mut grandparent = conversation(
        "child",
        "2026-08-31T05:01:00Z",
        vec![
            message(Role::Assistant, "work", "2026-08-31T05:01:00Z"),
            message(Role::User, "continue", "2026-09-01T11:00:00Z"),
        ],
    );
    grandparent.subagent_references.push(SubagentReference {
        child_native_id: Some("nested".into()),
        child_native_key: None,
        link_key: None,
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-09-01T11:01:00Z")),
    });
    insert(&archive, &grandparent);
    insert(
        &archive,
        &conversation(
            "nested",
            "2026-09-01T11:02:00Z",
            vec![message(
                Role::Assistant,
                "nested work",
                "2026-09-01T11:02:00Z",
            )],
        ),
    );
    archive.db().unwrap().resolve_subagent_links().unwrap();
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
day-start = "04:00"
unit = "session-day"
[views.layout]
session = "notes/{{ workday }}.md"
loose = "loose/{{ workday }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.children/{{ workday }} {{ short_id }}.md"
json = "json/{{ short_id }} {{ workday }}.json"
"#,
    );
    render_all(&archive).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/templates/vault-agent-session.md.j2");
    let config = fs::read_to_string(root.path().join("config.toml"))
        .unwrap()
        .replace("[views]", &format!("[views]\ntemplate = {:?}", template));
    write_config(root.path(), &config);
    render_all(&archive).unwrap();
    let values = fs::read_dir(archive.views_dir().join("json"))
        .unwrap()
        .map(|e| {
            serde_json::from_str::<serde_json::Value>(
                &fs::read_to_string(e.unwrap().path()).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let nested = values
        .iter()
        .find(|v| v["session"]["native_id"] == "nested")
        .unwrap();
    assert_eq!(nested["session"]["subagent"]["parent_id"], "child");
    assert_eq!(
        nested["session"]["subagent"]["parent_day"]["workday"],
        "2026-09-01"
    );
    let mut pending = vec![archive.views_dir().join("notes")];
    let mut child_count = 0;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let text = fs::read_to_string(path).unwrap();
                if text.contains("session-subagent: true") {
                    child_count += 1;
                    assert!(text.contains("update-policy: generated"));
                    assert!(text.contains("authorship: agent"));
                    assert!(!text.contains("[[the operator]]"));
                    assert!(!text.contains("tags:"));
                    assert!(text.contains("session-parent-id:"));
                    assert!(text.contains("session-day-index:"));
                    assert!(text.contains("session-host:"));
                }
            }
        }
    }
    assert_eq!(child_count, 5);
    let next = values
        .iter()
        .find(|v| v["session"]["native_id"] == "parent" && v["day"]["workday"] == "2026-09-01")
        .unwrap();
    assert_eq!(next["day"]["index"], 2);
    assert_eq!(next["day"]["first"]["workday"], "2026-08-30");
    assert_eq!(next["day"]["first"]["stem"], "2026-08-30 (2)");
    for v in values
        .iter()
        .filter(|v| v["session"]["native_id"] == "child" || v["session"]["native_id"] == "unknown")
    {
        let sub = &v["session"]["subagent"];
        assert_eq!(sub["parent_id"], "parent");
        assert_eq!(sub["name"], "Worker");
        if v["session"]["native_id"] == "child" {
            assert_eq!(sub["spawn_day_known"], true);
            assert_eq!(sub["parent_day"]["workday"], "2026-08-30");
            assert_eq!(sub["parent_day"]["stem"], "2026-08-30 (2)");
        } else {
            assert_eq!(sub["spawn_day_known"], false);
            assert!(sub["parent_day"].is_null());
        }
    }
}

#[test]
fn missing_first_day_digest_never_promotes_a_later_day_to_first() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "multi",
            "2026-08-30T10:00:00Z",
            vec![
                message(Role::User, "first", "2026-08-30T10:00:00Z"),
                message(Role::User, "second", "2026-08-31T10:00:00Z"),
            ],
        ),
    );
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "multi").unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("2026-08-31.json"),
        r#"{"title":"Second","lockedTitle":"Second"}"#,
    )
    .unwrap();
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "notes/{{ workday }} {{ title }}.md"
loose = "loose/{{ workday }} {{ title }}.md"
subagent = "subagents/{{ short_id }}.md"
json = "json/{{ short_id }} {{ workday }}.json"
"#,
    );
    render_all(&archive).unwrap();
    let file = fs::read_dir(archive.views_dir().join("json"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(v["day"]["index"], 2);
    assert!(v["day"]["first"].is_null());
}

#[test]
fn resolved_parent_only_evidence_renders_parent_instructions_in_both_profiles() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "parent",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "delegate", "2026-08-31T10:00:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child".into()),
        child_native_key: None,
        link_key: None,
        description: Some("Review".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:01:00Z")),
    });
    let child = conversation(
        "child",
        "2026-08-31T10:02:00Z",
        vec![
            message(
                Role::User,
                "Inspect exactly 🦀\nsecond line",
                "2026-08-31T10:02:00Z",
            ),
            message(Role::Assistant, "Found the issue.", "2026-08-31T10:03:00Z"),
            message(Role::User, "Confirm the fix.", "2026-08-31T10:04:00Z"),
            message(Role::Assistant, "Confirmed.", "2026-08-31T10:05:00Z"),
        ],
    );
    insert(&archive, &parent);
    insert(&archive, &child);
    archive.db().unwrap().resolve_subagent_links().unwrap();
    let config = r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "notes/{{ workday }}.md"
loose = "loose/{{ workday }}.md"
subagent = "children/{{ workday }}.md"
json = "json/{{ short_id }} {{ workday }}.json"
"#;
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/templates/vault-agent-session.md.j2");
    let mut bodies = Vec::new();
    for config in [
        config.to_owned(),
        config.replace("[views]", &format!("[views]\ntemplate = {:?}", template)),
    ] {
        write_config(root.path(), &config);
        render_all(&archive).unwrap();
        let context = fs::read_dir(archive.views_dir().join("json"))
            .unwrap()
            .map(|entry| {
                serde_json::from_str::<serde_json::Value>(
                    &fs::read_to_string(entry.unwrap().path()).unwrap(),
                )
                .unwrap()
            })
            .find(|value| value["session"]["native_id"] == "child")
            .unwrap();
        let body = context["body"].as_str().unwrap();
        assert!(!body.contains("<user>"));
        assert!(body.contains("\n\nInspect exactly 🦀\nsecond line\n\n</parent>"));
        let ordered = [
            "Inspect exactly 🦀\nsecond line",
            "Found the issue.",
            "Confirm the fix.",
            "Confirmed.",
        ];
        let positions = ordered.map(|text| body.find(text).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            fs::read_to_string(archive.views_dir().join("children/2026-08-31.md"))
                .unwrap()
                .trim_end()
                .ends_with(body.trim_end())
        );
        bodies.push(body.to_owned());
    }
    assert_eq!(bodies[0], bodies[1]);
    let db = archive.db().unwrap();
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|row| row.native_id == "child")
        .unwrap();
    assert_eq!(session.subagent_parent.unwrap().parent_native_id, "parent");
    let connection = rusqlite::Connection::open_with_flags(
        archive.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let native_parent_facts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM subagent_parent_facts WHERE child_native_id='child'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(native_parent_facts, 0);
    assert_eq!(
        serde_json::to_value(db.messages(session.id).unwrap()).unwrap(),
        serde_json::to_value(child.messages).unwrap()
    );
}
