//! View names must be accepted by Windows and Obsidian Sync on every platform:
//! unsafe characters, trailing dots/spaces, and device names never reach disk.
mod common;
use agent_sessions::{
    archive::Archive,
    model::{Conversation, Message, MessageKind, Provenance, Role, SubagentReference},
    render::render_all,
};
use std::fs;
use std::path::Path;

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn conversation(native_id: &str, project_path: &str) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "fixture".into(),
            native_id: native_id.into(),
            native_path: "/absent".into(),
        },
        title: None,
        project_path: Some(project_path.into()),
        native_project: None,
        created_at_ms: Some(ms("2026-10-26T12:00:00Z")),
        updated_at_ms: Some(ms("2026-10-26T12:00:00Z")),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "Refine the workflow".into(),
            timestamp_ms: Some(ms("2026-10-26T12:00:00Z")),
            actions_before: 0,
        }],
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: vec![],
        branches: Vec::new(),
        subagent_links: vec![],
        trailing_actions: 0,
    }
}

fn write_day_digest(root: &Path, native_id: &str, title: &str) {
    let dir = agent_sessions::digest::digest_dir(root, "codex", native_id).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("2026-10-26.json"),
        serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"source":"codex","nativeId":native_id,"workday":"2026-10-26","covered":{"firstIdx":0,"lastIdx":0},"topic":"Vault","title":title,"lockedTitle":title,"tldr":null,"summary":"Refined.","classification":["plan"],"truncated":false,"model":"fixture","promptVersion":1,"promptSource":"builtin","digestedAtMs":0})).unwrap(),
    )
    .unwrap();
}

fn relative_files(root: &Path) -> Vec<String> {
    common::tree_bytes(root)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

#[test]
fn locked_titles_render_sync_safe_names_and_old_spellings_are_swept() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut parent = conversation("parent", "/work/Vault");
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some("child".into()),
        child_native_key: None,
        link_key: Some("child".into()),
        description: None,
        agent_type: None,
        instructions: None,
        spawned_at_ms: Some(ms("2026-10-26T12:01:00Z")),
    });
    let child = conversation("child", "/work/Vault");
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
[views.layout]
session="projects/{{ project }}/{{ workday }} {{ project }} - {{ title }}.md"
subagent="{{ parent_dir }}/{{ parent_stem }}.subagents/{{ short_id }}.md"
"#,
    )
    .unwrap();
    write_day_digest(
        dir.path(),
        "parent",
        r#"Vault - Workflow Refinement: Daily Log, "Inbox" #1 ^b [x] <y> a|b *? v2."#,
    );
    // A View written by an earlier release with the old, Windows-illegal spelling.
    let views = archive.views_dir();
    let old_parent = views.join(
        r#"projects/Vault/2026-10-26 Vault - Vault - Workflow Refinement: Daily Log, "Inbox" #1 ^b [x] <y> a|b *? v2..md"#,
    );
    let old_child_dir = views.join(
        r#"projects/Vault/2026-10-26 Vault - Vault - Workflow Refinement: Daily Log, "Inbox" #1 ^b [x] <y> a|b *? v2..subagents"#,
    );
    fs::create_dir_all(&old_child_dir).unwrap();
    fs::write(&old_parent, "old").unwrap();
    fs::write(old_child_dir.join("old.md"), "old").unwrap();

    render_all(&archive).unwrap();

    let stem = "2026-10-26 Vault - Vault - Workflow Refinement： Daily Log, ＂Inbox＂ ＃1 ＾b ［x］ ＜y＞ a｜b ＊？ v2.";
    let files = relative_files(&views);
    assert!(
        files.contains(&format!("projects/Vault/{stem}.md")),
        "{files:#?}"
    );
    assert!(
        files
            .iter()
            .any(|path| path.starts_with(&format!("projects/Vault/{stem}.subagents/"))),
        "{files:#?}"
    );
    assert!(!old_parent.exists());
    assert!(!old_child_dir.exists());
    for path in &files {
        for component in Path::new(path).components() {
            let name = component.as_os_str().to_string_lossy();
            assert!(
                !name.contains([':', '*', '?', '"', '<', '>', '|', '#', '^', '[', ']', '\\']),
                "{path}"
            );
            assert!(!name.ends_with(['.', ' ']), "{path}");
        }
    }
    let output = common::tree_bytes(&views);
    render_all(&archive).unwrap();
    assert_eq!(common::tree_bytes(&views), output);
}

#[test]
fn project_directories_from_paths_name_tables_and_assignments_are_sync_safe() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&conversation("hash", "/work/optimizing for #1"))
        .unwrap();
    db.upsert_conversation(&conversation("table", "/work/table"))
        .unwrap();
    db.upsert_conversation(&conversation("device", "/work/aux"))
        .unwrap();
    db.upsert_conversation(&conversation("dots", "/work/release v1."))
        .unwrap();
    db.upsert_conversation(&conversation("assigned", "/work/elsewhere"))
        .unwrap();
    drop(db);
    agent_sessions::assignments::assign(&archive, "codex", "assigned", "Ops | Infra", None)
        .unwrap();
    fs::write(
        dir.path().join("config.toml"),
        r#"[views]
timezone="UTC"
[views.project-names]
"/work/table"="Clients: Acme?"
"#,
    )
    .unwrap();

    render_all(&archive).unwrap();

    let projects = fs::read_dir(archive.views_dir().join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        projects,
        [
            "optimizing for ＃1",
            "Clients： Acme？",
            "ａｕｘ",
            "release v1．",
            "Ops ｜ Infra"
        ]
        .map(String::from)
        .into()
    );
    let index = fs::read_to_string(
        archive
            .views_dir()
            .join("projects/Clients： Acme？/INDEX.md"),
    )
    .unwrap();
    assert!(index.contains("Clients: Acme?"), "{index}");
}

#[test]
fn titles_that_fill_a_whole_component_avoid_device_names_and_trailing_dots() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    db.upsert_conversation(&conversation("device", "/work/Vault"))
        .unwrap();
    db.upsert_conversation(&conversation("dots", "/work/Vault"))
        .unwrap();
    drop(db);
    fs::write(
        dir.path().join("config.toml"),
        r#"[views]
timezone="UTC"
unit="session-day"
[views.layout]
session="notes/{{ title }}/{{ workday }}.md"
"#,
    )
    .unwrap();
    write_day_digest(dir.path(), "device", "con");
    write_day_digest(dir.path(), "dots", "Wait. ");

    render_all(&archive).unwrap();

    assert_eq!(
        relative_files(&archive.views_dir()),
        ["notes/Wait．/2026-10-26.md", "notes/ｃｏｎ/2026-10-26.md"]
    );
}

#[test]
fn titles_that_become_equal_after_sanitizing_take_collision_suffixes() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    let mut later = conversation("ascii", "/work/Vault");
    for message in &mut later.messages {
        message.timestamp_ms = Some(ms("2026-10-26T13:00:00Z"));
    }
    db.upsert_conversation(&conversation("fullwidth", "/work/Vault"))
        .unwrap();
    db.upsert_conversation(&later).unwrap();
    drop(db);
    fs::write(
        dir.path().join("config.toml"),
        r#"[views]
timezone="UTC"
unit="session-day"
[views.layout]
session="notes/{{ title }}.md"
"#,
    )
    .unwrap();
    write_day_digest(dir.path(), "fullwidth", "Plan： next");
    write_day_digest(dir.path(), "ascii", "Plan: next");

    render_all(&archive).unwrap();

    assert_eq!(
        relative_files(&archive.views_dir()),
        ["notes/Plan： next (2).md", "notes/Plan： next.md"]
    );
}
