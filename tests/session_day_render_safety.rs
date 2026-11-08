use std::fs;
use std::path::Path;

use agent_sessions::archive::Archive;
use agent_sessions::model::{
    Conversation, Message, MessageKind, Provenance, Role, SubagentParent, SubagentReference,
};
use agent_sessions::render::{RenderOptions, render_all, render_all_with_options};

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
fn unsafe_output_roots_fail_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "safe",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
        ),
    );
    let capture = archive.captures_dir().join("keep.capture");
    fs::write(&capture, "keep").unwrap();
    let native = root.path().join("native-store");
    fs::create_dir_all(&native).unwrap();
    let native_sentinel = native.join("keep.native");
    fs::write(&native_sentinel, "keep").unwrap();

    for config in [
        "[views]\noutput = '.'\n".to_string(),
        "[views]\noutput = 'captures'\n".to_string(),
        format!(
            "[[host]]\nname = 'local'\ncodex-root = {:?}\n[views]\noutput = {:?}\n",
            native, native
        ),
    ] {
        write_config(root.path(), &config);
        assert!(render_all(&archive).is_err(), "{config}");
        assert_eq!(fs::read_to_string(&capture).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&native_sentinel).unwrap(), "keep");
    }
}

#[test]
fn literal_collision_suffix_and_file_directory_transitions_are_safe() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    for (index, title) in ["Same", "Same", "Same (2)"].into_iter().enumerate() {
        let id = format!("collision-{index}");
        insert(
            &archive,
            &conversation(
                &id,
                &format!("2026-08-31T1{index}:00:00Z"),
                vec![message(
                    Role::User,
                    &id,
                    &format!("2026-08-31T1{index}:00:00Z"),
                )],
            ),
        );
        let digest_dir = agent_sessions::digest::digest_dir(root.path(), "codex", &id).unwrap();
        fs::create_dir_all(&digest_dir).unwrap();
        fs::write(
            digest_dir.join("2026-08-31.json"),
            format!(r#"{{"lockedTitle":"{title}"}}"#),
        )
        .unwrap();
    }
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "out/{{ title }}.md"
loose = "loose/{{ title }}.md"
subagent = "{{ parent_dir }}/children/{{ title }}.md"
"#,
    );
    let blocker = archive.views_dir().join("out");
    fs::write(&blocker, "old file").unwrap();
    render_all(&archive).unwrap();
    assert!(archive.views_dir().join("out/Same.md").is_file());
    assert!(archive.views_dir().join("out/Same (2).md").is_file());
    assert!(archive.views_dir().join("out/Same (2) (2).md").is_file());

    fs::remove_dir_all(&blocker).unwrap();
    fs::write(&blocker, "old file again").unwrap();
    render_all(&archive).unwrap();
    assert!(blocker.is_dir());
}

#[test]
fn untimestamped_assistant_and_trailing_actions_belong_to_one_session_day() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut session = conversation(
        "actions",
        "2026-08-30T23:50:00Z",
        vec![
            message(Role::User, "first", "2026-08-30T23:50:00Z"),
            message(Role::Assistant, "without timestamp", "2026-08-30T23:51:00Z"),
            message(Role::User, "second", "2026-08-31T00:01:00Z"),
        ],
    );
    session.messages[1].timestamp_ms = None;
    session.trailing_actions = 3;
    insert(&archive, &session);
    write_config(
        root.path(),
        "[views]\ntimezone = 'UTC'\nunit = 'session-day'\n",
    );
    render_all_with_options(
        &archive,
        RenderOptions {
            show_action_counts: true,
        },
    )
    .unwrap();
    let files = fs::read_dir(archive.views_dir().join("projects/raw-project"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != "INDEX.md")
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert_eq!(
        files
            .iter()
            .filter(|text| text.contains("without timestamp"))
            .count(),
        1
    );
    assert_eq!(
        files
            .iter()
            .filter(|text| text.contains("<tool-calls count=3 />"))
            .count(),
        1
    );
    assert!(
        files
            .iter()
            .find(|text| text.contains("second"))
            .unwrap()
            .contains("<tool-calls count=3 />")
    );
}

#[test]
fn subagent_without_title_follows_rendered_parent_but_not_skipped_parent() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "parent-title-gated",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "parent", "2026-08-31T10:00:00Z")],
    );
    let mut child = conversation(
        "child-without-digest",
        "2026-08-31T10:01:00Z",
        vec![message(Role::User, "child", "2026-08-31T10:01:00Z")],
    );
    let mut grandchild = conversation(
        "grandchild-without-digest",
        "2026-08-31T10:02:00Z",
        vec![message(Role::User, "grandchild", "2026-08-31T10:02:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child-without-digest".into()),
        child_native_key: None,
        link_key: Some("spawn".into()),
        description: Some("Child".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "parent-title-gated".into(),
        description: Some("Child".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("spawn".into()),
    });
    child.subagent_references.push(SubagentReference {
        child_native_id: Some("grandchild-without-digest".into()),
        child_native_key: None,
        link_key: Some("nested-spawn".into()),
        description: Some("Grandchild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:01:30Z")),
    });
    grandchild.subagent_parent = Some(SubagentParent {
        parent_native_id: "child-without-digest".into(),
        description: Some("Grandchild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(2),
        native_key: None,
        link_key: Some("nested-spawn".into()),
    });
    insert(&archive, &parent);
    insert(&archive, &child);
    insert(&archive, &grandchild);
    assert_eq!(archive.db().unwrap().resolve_subagent_links().unwrap(), 2);
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "root/{{ workday }} {{ title }}.md"
loose = "loose/{{ workday }} {{ title }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.children/{{ workday }}-{{ short_id }}.md"
"#,
    );

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 0);
    assert_eq!(stats.skipped.len(), 3, "{:?}", stats.skipped);
    assert!(!archive.views_dir().join("root").exists());

    let digest_dir =
        agent_sessions::digest::digest_dir(root.path(), "codex", "parent-title-gated").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(
        digest_dir.join("2026-08-31.json"),
        r#"{"schemaVersion":1,"source":"codex","nativeId":"parent-title-gated","workday":"2026-08-31","covered":{"firstIdx":0,"lastIdx":0},"topic":"Parent","title":"Parent","lockedTitle":"Parent","tldr":null,"summary":"Summary","classification":["build"],"truncated":false,"model":"fixture","promptVersion":1,"digestedAtMs":0}"#,
    )
    .unwrap();

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 3);
    assert!(stats.skipped.is_empty(), "{:?}", stats.skipped);
    let parent_path = archive.views_dir().join("root/2026-08-31 Parent.md");
    assert!(parent_path.is_file());
    let child_dir = archive.views_dir().join("root/2026-08-31 Parent.children");
    let child_entries = fs::read_dir(child_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(child_entries.len(), 2);
    assert_eq!(
        child_entries.iter().filter(|path| path.is_file()).count(),
        1
    );
    assert_eq!(child_entries.iter().filter(|path| path.is_dir()).count(), 1);
    assert!(fs::read_to_string(parent_path).unwrap().contains("Child"));
}

#[test]
fn multiday_subagents_nest_under_the_spawn_owning_parent_day_at_every_depth() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "parent-multi",
        "2026-08-30T10:00:00Z",
        vec![
            message(Role::User, "parent day one", "2026-08-30T10:00:00Z"),
            message(Role::User, "parent day two", "2026-08-31T10:00:00Z"),
        ],
    );
    let mut child = conversation(
        "child-multi",
        "2026-08-31T10:01:00Z",
        vec![
            message(Role::User, "child day one", "2026-08-31T10:01:00Z"),
            message(Role::User, "child day two", "2026-09-01T10:00:00Z"),
        ],
    );
    let mut grandchild = conversation(
        "grandchild",
        "2026-09-01T10:01:00Z",
        vec![message(Role::User, "grandchild", "2026-09-01T10:01:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child-multi".into()),
        child_native_key: None,
        link_key: Some("child-link".into()),
        description: Some("UniqueChild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "parent-multi".into(),
        description: Some("UniqueChild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("child-link".into()),
    });
    child.subagent_references.push(SubagentReference {
        child_native_id: Some("grandchild".into()),
        child_native_key: None,
        link_key: Some("grand-link".into()),
        description: Some("UniqueGrandchild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-09-01T10:00:30Z")),
    });
    grandchild.subagent_parent = Some(SubagentParent {
        parent_native_id: "child-multi".into(),
        description: Some("UniqueGrandchild".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(2),
        native_key: None,
        link_key: Some("grand-link".into()),
    });
    insert(&archive, &parent);
    insert(&archive, &child);
    insert(&archive, &grandchild);
    assert_eq!(archive.db().unwrap().resolve_subagent_links().unwrap(), 2);
    write_config(
        root.path(),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "root/{{ workday }}-{{ short_id }}.md"
loose = "loose/{{ workday }}-{{ short_id }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.children/{{ workday }}-{{ short_id }}.md"
"#,
    );
    render_all(&archive).unwrap();

    let root_dir = archive.views_dir().join("root");
    let parent_files = fs::read_dir(&root_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    assert_eq!(parent_files.len(), 2);
    let second_parent = parent_files
        .iter()
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("2026-08-31")
        })
        .unwrap();
    assert_eq!(
        parent_files
            .iter()
            .map(|path| fs::read_to_string(path).unwrap())
            .filter(|text| text.contains("UniqueChild"))
            .count(),
        1
    );
    assert!(
        fs::read_to_string(second_parent)
            .unwrap()
            .contains("UniqueChild")
    );
    let child_dir = root_dir.join(format!(
        "{}.children",
        second_parent.file_stem().unwrap().to_string_lossy()
    ));
    let child_files = fs::read_dir(&child_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    assert_eq!(child_files.len(), 2);
    let second_child = child_files
        .iter()
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("2026-09-01")
        })
        .unwrap();
    let grandchild_dir = child_dir.join(format!(
        "{}.children",
        second_child.file_stem().unwrap().to_string_lossy()
    ));
    assert_eq!(fs::read_dir(grandchild_dir).unwrap().count(), 1);
}

#[test]
fn user_template_inside_output_is_preserved_and_rejected() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "template-owner",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
        ),
    );
    let template = archive.views_dir().join("owned.j2");
    fs::write(&template, "{{ body }}").unwrap();
    write_config(
        root.path(),
        &format!("[views]\ntemplate = {:?}\n", template),
    );
    assert!(render_all(&archive).is_err());
    assert_eq!(fs::read_to_string(template).unwrap(), "{{ body }}");
}

#[test]
fn cyclic_subagent_parent_graph_fails_before_view_mutation() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut first = conversation(
        "cycle-a",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "a", "2026-08-31T10:00:00Z")],
    );
    let mut second = conversation(
        "cycle-b",
        "2026-08-31T10:01:00Z",
        vec![message(Role::User, "b", "2026-08-31T10:01:00Z")],
    );
    first.subagent_references.push(SubagentReference {
        child_native_id: Some("cycle-b".into()),
        child_native_key: None,
        link_key: Some("a-to-b".into()),
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    first.subagent_parent = Some(SubagentParent {
        parent_native_id: "cycle-b".into(),
        description: None,
        agent_type: None,
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("b-to-a".into()),
    });
    second.subagent_references.push(SubagentReference {
        child_native_id: Some("cycle-a".into()),
        child_native_key: None,
        link_key: Some("b-to-a".into()),
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:01:30Z")),
    });
    second.subagent_parent = Some(SubagentParent {
        parent_native_id: "cycle-a".into(),
        description: None,
        agent_type: None,
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("a-to-b".into()),
    });
    insert(&archive, &first);
    insert(&archive, &second);
    assert_eq!(archive.db().unwrap().resolve_subagent_links().unwrap(), 2);
    let sentinel = archive.views_dir().join("keep.txt");
    fs::write(&sentinel, "keep").unwrap();
    assert!(render_all(&archive).is_err());
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
}
