mod common;

use std::fs;
use std::path::Path;

use agent_sessions::archive::Archive;
use agent_sessions::model::{
    Conversation, GitSnapshot, Message, MessageKind, Provenance, Role, SubagentParent,
    SubagentReference,
};
use agent_sessions::render::render_all;

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

#[test]
fn vault_template_fixture_has_compact_plain_scalar_frontmatter() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut session = conversation(
        "vault-fixture",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
    );
    session.transcript_facts.starting_model = Some("model:beta".into());
    session.transcript_facts.git = Some(GitSnapshot {
        branch: Some("feature: fix".into()),
        ..Default::default()
    });
    session.transcript_facts.patched_files = vec!["src/a: b.rs".into()];
    session.transcript_facts.referenced_files = vec!["docs/one.md".into()];
    insert(&archive, &session);
    let digest_dir =
        agent_sessions::digest::digest_dir(root.path(), "codex", "vault-fixture").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(
        digest_dir.join("2026-08-31.json"),
        r#"{"schemaVersion":1,"source":"codex","nativeId":"vault-fixture","workday":"2026-08-31","covered":{"firstIdx":0,"lastIdx":0},"topic":"Topic","title":"Topic - Work","lockedTitle":"Topic - Work","tldr":null,"summary":"Summary","classification":["build"],"truncated":false,"model":"fixture","promptVersion":1,"promptSource":"builtin","digestedAtMs":0}"#,
    )
    .unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/templates/vault-agent-session.md.j2");
    write_config(
        root.path(),
        &format!(
            r#"[views]
timezone = "UTC"
unit = "session-day"
template = "{}"
[views.layout]
session = "notes/{{{{ workday }}}} {{{{ title }}}}.md"
loose = "loose/{{{{ workday }}}} {{{{ title }}}}.md"
subagent = "{{{{ parent_dir }}}}/subagents/{{{{ short_id }}}}.md"
"#,
            template.display()
        ),
    );

    render_all(&archive).unwrap();
    let rendered =
        fs::read_to_string(archive.views_dir().join("notes/2026-08-31 Topic - Work.md")).unwrap();
    let frontmatter = rendered.split("---\n").nth(1).unwrap();
    assert!(!frontmatter.lines().any(str::is_empty));
    assert!(!frontmatter.contains(">-"));
    assert!(frontmatter.contains("session-id: \"codex:vault-fixture\""));
    assert!(frontmatter.contains("session-starting-model: \"model:beta\""));
    assert!(frontmatter.contains("session-git-branch: \"feature: fix\""));
    assert!(frontmatter.contains("  - \"src/a: b.rs\""));
    assert!(frontmatter.contains("  - docs/one.md"));
    // A callout must end with a blank line, or the Dialog that follows lazily
    // continues the blockquote in Obsidian.
    // The pre-rendered body begins with a newline, so exactly one blank line
    // separates the callout from the Dialog.
    assert!(rendered.contains("> Summary\n\n<user>"));
    assert!(!rendered.contains("> Summary\n\n\n"));
}

#[test]
fn decomposed_unicode_view_name_is_not_swept_as_an_orphan() {
    // A synced Vault can contain NFD names on either platform. Reconcile
    // them before writing; do not create duplicates or sweep a live View.
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "nfd",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
        ),
    );
    let digest_dir = agent_sessions::digest::digest_dir(root.path(), "codex", "nfd").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(
        digest_dir.join("2026-08-31.json"),
        r#"{"schemaVersion":1,"source":"codex","nativeId":"nfd","workday":"2026-08-31","covered":{"firstIdx":0,"lastIdx":0},"topic":"Café","title":"Café - Work","lockedTitle":"Café - Work","tldr":null,"summary":"Summary","classification":["build"],"truncated":false,"model":"fixture","promptVersion":1,"promptSource":"builtin","digestedAtMs":0}"#,
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
subagent = "{{ parent_dir }}/subagents/{{ short_id }}.md"
"#,
    );
    render_all(&archive).unwrap();
    let notes = archive.views_dir().join("notes");
    let composed = notes.join("2026-08-31 Café - Work.md");
    assert!(composed.is_file());
    let decomposed = notes.join("2026-08-31 Cafe\u{301} - Work.md");
    fs::rename(&composed, &decomposed).unwrap();

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.removed, 0);
    assert_eq!(stats.changed, 0);
    assert!(composed.is_file());
    assert_eq!(fs::read_dir(notes).unwrap().count(), 1);
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
fn session_day_split_keeps_crossing_assistant_with_its_turn_and_links_days() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "multi",
            "2026-08-31T03:30:00Z",
            vec![
                message(Role::User, "first user", "2026-08-31T03:30:00Z"),
                message(
                    Role::Assistant,
                    "crossing assistant",
                    "2026-08-31T05:00:00Z",
                ),
                message(Role::User, "second user", "2026-08-31T05:01:00Z"),
                message(Role::Assistant, "second assistant", "2026-08-31T05:02:00Z"),
            ],
        ),
    );
    let template = root.path().join("unit.j2");
    fs::write(
        &template,
        "{{ day.workday }}|{% if day.previous %}{{ day.previous.workday }}:{{ day.previous.path }}{% endif %}|{% if day.next %}{{ day.next.workday }}:{{ day.next.path }}{% endif %}\n{{ body }}",
    )
    .unwrap();
    write_config(
        root.path(),
        &format!(
            r#"[views]
timezone = "UTC"
day-start = "04:00"
unit = "session-day"
template = "{}"

[views.layout]
session = "projects/{{{{ project }}}}/{{{{ workday }}}}.{{{{ short_id }}}}.md"
loose = "loose/{{{{ month }}}}/{{{{ workday }}}}.{{{{ short_id }}}}.md"
subagent = "{{{{ parent_dir }}}}/subagents/{{{{ workday }}}}.{{{{ short_id }}}}.md"

[views.project-names]
"/work/raw-project" = "Mapped Project"
"#,
            template.display()
        ),
    );

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 2);
    assert_eq!(stats.indexes, 0, "layout table omitted index keys");
    let dir = archive.views_dir().join("projects/Mapped Project");
    let mut files = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 2);
    let first = fs::read_to_string(&files[0]).unwrap();
    let second = fs::read_to_string(&files[1]).unwrap();
    assert!(first.contains("2026-08-30||2026-08-31:"));
    assert!(first.contains("first user"));
    assert!(first.contains("crossing assistant"));
    assert!(!first.contains("second user"));
    assert!(second.contains("2026-08-31|2026-08-30:"));
    assert!(second.contains("second user"));
    assert!(!second.contains("first user"));
}

#[test]
fn custom_output_template_name_tables_and_collision_suffixes_are_deterministic() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    for (id, timestamp) in [
        ("earlier", "2026-08-31T10:00:00Z"),
        ("later", "2026-08-31T11:00:00Z"),
    ] {
        insert(
            &archive,
            &conversation(id, timestamp, vec![message(Role::User, id, timestamp)]),
        );
        let digest_dir = agent_sessions::digest::digest_dir(root.path(), "codex", id).unwrap();
        fs::create_dir_all(&digest_dir).unwrap();
        fs::write(
            digest_dir.join("2026-08-31.json"),
            format!(
                r#"{{"schemaVersion":1,"source":"codex","nativeId":"{id}","workday":"2026-08-31","covered":{{"firstIdx":0,"lastIdx":0}},"topic":"Shared","title":"Shared","lockedTitle":"Shared","tldr":null,"summary":"Summary","classification":["build"],"truncated":false,"model":"fixture","promptVersion":1,"digestedAtMs":0}}"#
            ),
        )
        .unwrap();
    }
    let output = root.path().join("custom-output");
    let template = root.path().join("unit.j2");
    fs::write(
        &template,
        "{{ session.native_id }}|{{ session.source_name }}|{{ session.host_name }}|{{ session.project.name }}|{{ day.workday }}\n{{ body }}",
    )
    .unwrap();
    write_config(
        root.path(),
        &format!(
            r#"[views]
timezone = "UTC"
unit = "session-day"
output = "{}"
template = "{}"

[views.layout]
session = "projects/{{{{ project }}}}/{{{{ workday }}}} {{{{ title }}}}.md"
loose = "loose/{{{{ month }}}}/{{{{ workday }}}} {{{{ title }}}}.md"
subagent = "{{{{ parent_dir }}}}/subagents/{{{{ workday }}}} {{{{ title }}}}.md"

[views.source-names]
codex = "Codex App"
[views.host-names]
raw-host = "Mapped Host"
[views.project-names]
"/work/raw-project" = "Mapped Project"
"#,
            output.display(),
            template.display()
        ),
    );

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 2);
    assert_eq!(stats.indexes, 0);
    assert!(!archive.views_dir().join("projects").exists());
    let first = output.join("projects/Mapped Project/2026-08-31 Shared.md");
    let second = output.join("projects/Mapped Project/2026-08-31 Shared (2).md");
    assert!(
        fs::read_to_string(first)
            .unwrap()
            .starts_with("earlier|Codex App||Mapped Project|2026-08-31")
    );
    assert!(
        fs::read_to_string(second)
            .unwrap()
            .starts_with("later|Codex App||Mapped Project|2026-08-31")
    );
}

#[test]
fn title_layout_skips_undigested_unit_then_uses_locked_title() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "digest-me",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "digest me", "2026-08-31T10:00:00Z")],
        ),
    );
    let template = root.path().join("digest.j2");
    fs::write(
        &template,
        "{{ day.digest.locked_title }}|{{ day.digest.classification | join(',') }}\n{{ body }}",
    )
    .unwrap();
    write_config(
        root.path(),
        &format!(
            r#"[views]
timezone = "UTC"
unit = "session-day"
template = "{}"
[views.layout]
session = "projects/{{{{ project }}}}/{{{{ workday }}}} {{{{ title }}}}.md"
loose = "loose/{{{{ month }}}}/{{{{ workday }}}} {{{{ title }}}}.md"
subagent = "{{{{ parent_dir }}}}/subagents/{{{{ workday }}}} {{{{ title }}}}.md"
"#,
            template.display()
        ),
    );
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 0);
    assert_eq!(stats.skipped.len(), 1, "{:?}", stats.skipped);
    assert!(stats.skipped[0].contains("digest-me/2026-08-31"));

    let digest_dir = agent_sessions::digest::digest_dir(root.path(), "codex", "digest-me").unwrap();
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(
        digest_dir.join("2026-08-31.json"),
        r#"{"schemaVersion":1,"source":"codex","nativeId":"digest-me","workday":"2026-08-31","covered":{"firstIdx":0,"lastIdx":0},"topic":"Topic","title":"Changed title","lockedTitle":"Locked: title/name","tldr":null,"summary":"Summary","classification":["build"],"truncated":false,"model":"fixture","promptVersion":1,"digestedAtMs":0}"#,
    )
    .unwrap();
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 1);
    assert!(stats.skipped.is_empty());
    assert!(
        archive
            .views_dir()
            .join("projects/raw-project/2026-08-31 Locked： title／name.md")
            .is_file()
    );
    let rendered = fs::read_to_string(
        archive
            .views_dir()
            .join("projects/raw-project/2026-08-31 Locked： title／name.md"),
    )
    .unwrap();
    assert!(rendered.starts_with("Locked: title/name|build"));
}

#[test]
fn session_unit_has_null_day_and_custom_index_template() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "one-session",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
        ),
    );
    let template = root.path().join("session.j2");
    let index_template = root.path().join("index.j2");
    fs::write(
        &template,
        "{% if day is none %}session-unit{% else %}wrong{% endif %}\n{{ body }}",
    )
    .unwrap();
    fs::write(
        &index_template,
        "{{ title }}:{% for entry in entries %}{{ entry.filename }}{% endfor %}",
    )
    .unwrap();
    write_config(
        root.path(),
        &format!(
            "[views]\ntimezone = 'UTC'\ntemplate = {:?}\nindex-template = {:?}\n",
            template, index_template
        ),
    );
    render_all(&archive).unwrap();
    let transcript = fs::read_to_string(
        fs::read_dir(archive.views_dir().join("projects/raw-project"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap() != "INDEX.md")
            .unwrap(),
    )
    .unwrap();
    assert!(transcript.starts_with("session-unit"));
    let index =
        fs::read_to_string(archive.views_dir().join("projects/raw-project/INDEX.md")).unwrap();
    assert!(index.starts_with("raw-project:2026-08-31T100000Z."));
}

#[test]
fn subagent_layout_uses_resolved_parent_directory_and_stem() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "parent",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "parent", "2026-08-31T10:00:00Z")],
    );
    let mut child = conversation(
        "child",
        "2026-08-31T10:01:00Z",
        vec![message(Role::User, "child", "2026-08-31T10:01:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child".into()),
        child_native_key: None,
        link_key: Some("spawn".into()),
        description: Some("Child".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "parent".into(),
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
session = "root/{{ workday }} {{ short_id }}.md"
loose = "loose/{{ workday }} {{ short_id }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.children/{{ workday }} {{ short_id }}.md"
"#,
    );
    render_all(&archive).unwrap();
    let root_dir = archive.views_dir().join("root");
    let parent_path = fs::read_dir(&root_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .unwrap();
    let parent_stem = parent_path.file_stem().unwrap().to_string_lossy();
    let child_dir = root_dir.join(format!("{parent_stem}.children"));
    assert_eq!(
        fs::read_dir(child_dir)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        1
    );
}

#[test]
fn template_and_layout_errors_fail_before_view_mutation() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    insert(
        &archive,
        &conversation(
            "bad-config",
            "2026-08-31T10:00:00Z",
            vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
        ),
    );
    let sentinel = archive.views_dir().join("keep.txt");
    fs::write(&sentinel, "keep").unwrap();
    let bad_template = root.path().join("bad.j2");
    fs::write(&bad_template, "{% if broken %}").unwrap();

    for invalid in [
        format!("[views]\ntemplate = {:?}\n", bad_template),
        "[views.layout]\nsession = '{{ broken'\n".into(),
        "[views]\nunit = 'session-day'\n[views.layout]\nsession = '{{ unknown }}.md'\n".into(),
        "[views]\nunknown = true\n".into(),
    ] {
        write_config(root.path(), &invalid);
        assert!(render_all(&archive).is_err(), "{invalid}");
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    }
}

#[test]
fn title_dependent_index_layout_skips_undigested_unit() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut parent = conversation(
        "index-title",
        "2026-08-31T10:00:00Z",
        vec![message(Role::User, "hello", "2026-08-31T10:00:00Z")],
    );
    let mut child = conversation(
        "index-child",
        "2026-08-31T10:01:00Z",
        vec![message(Role::User, "child", "2026-08-31T10:01:00Z")],
    );
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("index-child".into()),
        child_native_key: None,
        link_key: Some("index-link".into()),
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-08-31T10:00:30Z")),
    });
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "index-title".into(),
        description: None,
        agent_type: None,
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("index-link".into()),
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
session = "projects/{{ project }}/{{ default_stem }}.md"
loose = "loose/{{ default_stem }}.md"
subagent = "{{ parent_dir }}/subagents/{{ default_stem }}.md"
project-index = "projects/{{ project }}/{{ title }}.index.md"
"#,
    );
    let stats = render_all(&archive).unwrap();
    assert_eq!(
        stats.transcripts, 0,
        "resolved child follows skipped parent"
    );
    assert_eq!(stats.indexes, 0);
    assert_eq!(stats.skipped.len(), 2, "{:?}", stats.skipped);
}
