mod common;
use agent_sessions::{
    archive::Archive,
    model::{Conversation, Message, MessageKind, Provenance, Role, SubagentReference},
    render::render_all,
};
use std::fs;
fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}
#[test]
fn scalar_slashes_stay_in_filenames_and_resolved_links_follow_the_same_paths() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let messages = ["2026-08-30", "2026-08-31"]
        .into_iter()
        .map(|day| Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "Inspect spec/PRD".into(),
            timestamp_ms: Some(ms(&format!("{day}T12:00:00Z"))),
            actions_before: 0,
        })
        .collect();
    let mut parent = Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "fixture".into(),
            native_id: "parent".into(),
            native_path: "/absent".into(),
        },
        title: None,
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: Some(ms("2026-08-30T12:00:00Z")),
        updated_at_ms: Some(ms("2026-08-31T12:00:00Z")),
        messages,
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: vec![],
        branches: Vec::new(),
        subagent_links: vec![],
        trailing_actions: 0,
    };
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child".into()),
        child_native_key: None,
        link_key: Some("child".into()),
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-30T12:01:00Z")),
    });
    let mut child = parent.clone();
    child.provenance.native_id = "child".into();
    child.messages.truncate(1);
    child.subagent_references.clear();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&parent).unwrap();
    db.upsert_conversation(&child).unwrap();
    db.resolve_subagent_links().unwrap();
    drop(db);
    fs::write(
        dir.path().join("config.toml"),
        r#"[views]
timezone="UTC"
unit="session-day"
[views.project-names]
"/work/project"="Design/Research"
[views.layout]
session="notes/{{ workday }} {{ project }} - {{ title }}.md"
subagent="{{ parent_dir }}/{{ parent_stem }}.children/{{ short_id }}.md"
json="json/{{ short_id }} {{ workday }}.json"
"#,
    )
    .unwrap();
    let digest_dir = agent_sessions::digest::digest_dir(dir.path(), "codex", "parent").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    for (index, day) in ["2026-08-30", "2026-08-31"].into_iter().enumerate() {
        fs::write(digest_dir.join(format!("{day}.json")), serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"source":"codex","nativeId":"parent","workday":day,"covered":{"firstIdx":index,"lastIdx":index},"topic":"Skills","title":"Skills - spec/PRD and x\\y","lockedTitle":"Skills - spec/PRD and x\\y","tldr":null,"summary":"Checked paths.","classification":["research"],"truncated":false,"model":"fixture","promptVersion":1,"promptSource":"builtin","digestedAtMs":0})).unwrap()).unwrap();
    }
    let before = common::tree_bytes(&dir.path().join("digests"));
    render_all(&archive).unwrap();
    let first = "notes/2026-08-30 Design／Research - Skills - spec／PRD and x＼y.md";
    let second = "notes/2026-08-31 Design／Research - Skills - spec／PRD and x＼y.md";
    assert!(archive.views_dir().join(first).is_file());
    assert!(archive.views_dir().join(second).is_file());
    let values = fs::read_dir(archive.views_dir().join("json"))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(entry.unwrap().path()).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let later = values
        .iter()
        .find(|v| v["session"]["native_id"] == "parent" && v["day"]["workday"] == "2026-08-31")
        .unwrap();
    assert_eq!(later["session"]["project"]["name"], "Design/Research");
    assert_eq!(
        fs::canonicalize(archive.views_dir().join("notes").join(decode_href(
            later["day"]["previous"]["path"].as_str().unwrap()
        )))
        .unwrap(),
        fs::canonicalize(archive.views_dir().join(first)).unwrap()
    );
    let child = values
        .iter()
        .find(|v| v["session"]["native_id"] == "child")
        .unwrap();
    let child_dir = archive
        .views_dir()
        .join(first.trim_end_matches(".md").to_owned() + ".children");
    assert_eq!(fs::read_dir(&child_dir).unwrap().count(), 1);
    assert_eq!(
        fs::canonicalize(
            child_dir.join(decode_href(
                child["session"]["subagent"]["parent_day"]["path"]
                    .as_str()
                    .unwrap()
            ))
        )
        .unwrap(),
        fs::canonicalize(archive.views_dir().join(first)).unwrap()
    );
    assert_eq!(common::tree_bytes(&dir.path().join("digests")), before);
    let output = common::tree_bytes(&archive.views_dir());
    render_all(&archive).unwrap();
    assert_eq!(common::tree_bytes(&archive.views_dir()), output);
}

fn decode_href(value: &str) -> String {
    let mut bytes = Vec::new();
    let mut input = value.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let pair = [input.next().unwrap(), input.next().unwrap()];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&pair).unwrap(), 16).unwrap());
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn native_title_layout_is_source_neutral_and_does_not_require_or_create_a_digest() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let conversation = Conversation {
        provenance: Provenance {
            source: "pi".into(),
            host: "fixture".into(),
            native_id: "native-name".into(),
            native_path: "fixture".into(),
        },
        title: Some("Native/name".into()),
        project_path: None,
        native_project: None,
        created_at_ms: Some(ms("2026-08-30T12:00:00Z")),
        updated_at_ms: Some(ms("2026-08-30T12:00:00Z")),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "Preserved request".into(),
            timestamp_ms: Some(ms("2026-08-30T12:00:00Z")),
            actions_before: 0,
        }],
        branches: vec![],
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: vec![],
        subagent_links: vec![],
        trailing_actions: 0,
    };
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    fs::write(dir.path().join("config.toml"),"[views]\ntimezone='UTC'\nunit='session-day'\n[views.layout]\nloose='notes/{{ workday }} {{ native_title }} {{ short_id }}.md'\n").unwrap();
    let stats = render_all(&archive).unwrap();
    assert!(stats.skipped.is_empty());
    assert_eq!(stats.transcripts, 1);
    let path = fs::read_dir(archive.views_dir().join("notes"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("Native／name")
        })
        .unwrap();
    assert!(
        fs::read_to_string(path)
            .unwrap()
            .contains("Preserved request")
    );
    assert!(
        !archive.root().join("digests").exists()
            || fs::read_dir(archive.root().join("digests"))
                .unwrap()
                .next()
                .is_none()
    );
}
