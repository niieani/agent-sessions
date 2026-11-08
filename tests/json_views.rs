use std::fs;
use std::path::Path;

use agent_sessions::archive::Archive;
use agent_sessions::model::{
    Conversation, Message, MessageKind, Provenance, Role, SubagentParent, SubagentReference,
};
use agent_sessions::render::render_all;

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
fn serializes_the_stable_render_context() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "json-unit",
            "2026-08-31T10:00:00Z",
            vec![
                message(Role::User, "question", "2026-08-31T10:00:00Z"),
                message(Role::Assistant, "answer", "2026-08-31T10:01:00Z"),
            ],
        ),
    );
    let digest_dir = agent_sessions::digest::digest_dir(root.path(), "codex", "json-unit").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(
        digest_dir.join("2026-08-31.json"),
        r#"{"schemaVersion":1,"source":"codex","nativeId":"json-unit","workday":"2026-08-31","covered":{"firstIdx":0,"lastIdx":1},"topic":"JSON Views","title":"JSON Views - Stable context","lockedTitle":"JSON Views - Stable context","intent":"Keep render data stable","tldr":"One line","summary":"Stable render data.","classification":["build","test"],"truncated":false,"model":"fixture","promptVersion":1,"digestedAtMs":0}"#,
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
subagent = "{{ parent_dir }}/subagents/{{ workday }} {{ short_id }}.md"
json = "json/{{ source }}/{{ short_id }}/{{ workday }}.json"
"#,
    );

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 1);
    assert_eq!(stats.json_views, 1);
    let json_dir = archive.views_dir().join("json/codex");
    let json_path = fs::read_dir(json_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("2026-08-31.json");
    let actual = fs::read_to_string(json_path).unwrap();
    assert_eq!(actual, include_str!("goldens/session_day.json"));
}

#[test]
fn layout_receives_top_level_fallback_and_resolved_subagent_parent_paths() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "json-parent",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "parent", "2026-08-31T10:00:00Z")],
    );
    let mut child = conversation(
        "json-child",
        "2026-08-31T10:01:00Z",
        vec![message(Role::User, "child", "2026-08-31T10:01:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("json-child".into()),
        child_native_key: None,
        link_key: Some("spawn".into()),
        description: Some("Child".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "json-parent".into(),
        description: Some("Child".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("spawn".into()),
    });
    insert(&archive, &parent);
    insert(&archive, &child);
    assert_eq!(archive.db().unwrap().resolve_subagent_links().unwrap(), 1);
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "root/{{ default_stem }}.md"
loose = "loose/{{ default_stem }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.children/{{ default_stem }}.md"
json = "{{ parent_dir }}/{{ parent_stem }}.json"
"#,
    );

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 2);
    assert_eq!(stats.json_views, 2);
    assert_eq!(
        fs::read_dir(archive.views_dir().join("projects/raw-project"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|value| value == "json"))
            .count(),
        1,
        "top-level JSON uses the normal group/default-stem fallback"
    );
    let parent = fs::read_dir(archive.views_dir().join("root"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "md"))
        .unwrap();
    let parent_stem = parent.file_stem().unwrap().to_string_lossy();
    let child_json = archive
        .views_dir()
        .join("root")
        .join(format!("{parent_stem}.json"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(child_json).unwrap())
            .unwrap()["session"]["native_id"],
        "json-child"
    );
}
