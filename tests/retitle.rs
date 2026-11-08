use std::fs;
use std::path::Path;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{DayDigest, PromptSource, read_day};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};
use agent_sessions::render::render_all;
use agent_sessions::retitle::{RetitleOptions, run};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn conversation(id: &str) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "testhost".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}.jsonl"),
        },
        title: Some("Native title".into()),
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: Some(ms("2026-08-31T10:00:00Z")),
        updated_at_ms: Some(ms("2026-08-31T10:00:00Z")),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "hello".into(),
            timestamp_ms: Some(ms("2026-08-31T10:00:00Z")),
            actions_before: 0,
        }],
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

fn write_digest(root: &Path, source: &str, native_id: &str, title: &str, locked_title: &str) {
    let dir = agent_sessions::digest::digest_dir(root, source, native_id).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let digest = DayDigest {
        schema_version: 1,
        dialog_fingerprint: None,
        source: source.into(),
        native_id: native_id.into(),
        workday: "2026-08-31".into(),
        covered: agent_sessions::digest::CoveredRange {
            first_idx: 0,
            last_idx: 0,
        },
        topic: Some("Topic".into()),
        title: title.into(),
        locked_title: locked_title.into(),
        intent: None,
        tldr: None,
        summary: "Summary".into(),
        classification: vec!["build".into()],
        truncated: false,
        model: "fixture".into(),
        prompt_version: 1,
        prompt_source: PromptSource::Builtin,
        digested_at_ms: 0,
    };
    let mut bytes = serde_json::to_vec_pretty(&digest).unwrap();
    bytes.push(b'\n');
    fs::write(dir.join("2026-08-31.json"), bytes).unwrap();
}

#[test]
fn dry_run_plans_rename_and_live_run_only_relocks_the_digest() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("retitle-me"))
        .unwrap();
    fs::write(
        root.path().join("config.toml"),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "notes/{{ workday }} {{ title }}.md"
loose = "loose/{{ workday }} {{ title }}.md"
subagent = "{{ parent_dir }}/subagents/{{ workday }} {{ short_id }}.md"
"#,
    )
    .unwrap();
    write_digest(
        root.path(),
        "codex",
        "retitle-me",
        "Topic - Current title",
        "Topic - Locked title",
    );
    render_all(&archive).unwrap();
    fs::remove_file(root.path().join(".agent-sessions.lock")).unwrap();
    let view_root = archive.views_dir().canonicalize().unwrap();
    let old_path = view_root.join("notes/2026-08-31 Topic - Locked title.md");
    let new_path = view_root.join("notes/2026-08-31 Topic - Current title.md");
    assert!(old_path.is_file());
    let before_dry_run = common::tree_bytes(root.path());

    let dry = run(
        &archive,
        &RetitleOptions {
            native_id: Some("retitle-me".into()),
            day: Some("2026-08-31".into()),
            dry_run: true,
        },
    )
    .unwrap();
    assert_eq!(dry.relocked, 1);
    assert_eq!(dry.renames.len(), 1);
    assert_eq!(dry.renames[0].old_path, old_path);
    assert_eq!(dry.renames[0].new_path, new_path);
    let digest_path = agent_sessions::digest::digest_dir(root.path(), "codex", "retitle-me")
        .unwrap()
        .join("2026-08-31.json");
    assert_eq!(
        read_day(&digest_path).unwrap().unwrap().locked_title,
        "Topic - Locked title"
    );
    assert!(old_path.is_file());
    assert!(!new_path.exists());
    use sha2::{Digest as _, Sha256};
    let fingerprints = |files: Vec<(String, Vec<u8>)>| {
        files
            .into_iter()
            // SQLite may create its disposable read-lock/journal files on a read-only open.
            .filter(|(path, _)| !matches!(path.as_str(), "archive.db-shm" | "archive.db-wal"))
            .map(|(path, bytes)| (path, format!("{:x}", Sha256::digest(bytes))))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fingerprints(common::tree_bytes(root.path())),
        fingerprints(before_dry_run)
    );

    let live = run(
        &archive,
        &RetitleOptions {
            native_id: Some("retitle-me".into()),
            day: Some("2026-08-31".into()),
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(live.renames, dry.renames);
    assert_eq!(
        read_day(&digest_path).unwrap().unwrap().locked_title,
        "Topic - Current title"
    );
    assert!(old_path.is_file(), "retitle never mutates Views");
    assert!(!new_path.exists());

    render_all(&archive).unwrap();
    assert!(!old_path.exists());
    assert!(new_path.is_file());
}

#[test]
fn rejects_mismatched_and_ambiguous_digest_identities_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    write_digest(
        root.path(),
        "codex",
        "mismatch",
        "Topic - Current",
        "Topic - Locked",
    );
    let mismatch_path = agent_sessions::digest::digest_dir(root.path(), "codex", "mismatch")
        .unwrap()
        .join("2026-08-31.json");
    let mut mismatch: serde_json::Value =
        serde_json::from_slice(&fs::read(&mismatch_path).unwrap()).unwrap();
    mismatch["source"] = "claude".into();
    fs::write(
        &mismatch_path,
        serde_json::to_vec_pretty(&mismatch).unwrap(),
    )
    .unwrap();
    let before = common::tree_bytes(&root.path().join("digests"));
    assert!(run(&archive, &RetitleOptions::default()).is_err());
    assert_eq!(common::tree_bytes(&root.path().join("digests")), before);

    fs::write(
        &mismatch_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "source": "codex",
            "nativeId": "mismatch",
            "workday": "2026-08-31",
            "covered": {"firstIdx": 0, "lastIdx": 0},
            "topic": "Topic",
            "title": "Topic - Current",
            "lockedTitle": "Topic - Locked",
            "tldr": null,
            "summary": "Summary",
            "classification": ["build"],
            "truncated": false,
            "model": "fixture",
            "promptVersion": 1,
            "digestedAtMs": 0
        }))
        .unwrap(),
    )
    .unwrap();
    write_digest(
        root.path(),
        "claude",
        "mismatch",
        "Topic - Other current",
        "Topic - Other locked",
    );
    let before = common::tree_bytes(root.path());
    assert!(
        run(
            &archive,
            &RetitleOptions {
                native_id: Some("mismatch".into()),
                day: None,
                dry_run: true,
            },
        )
        .unwrap_err()
        .to_string()
        .contains("ambiguous across Sources")
    );
    assert_eq!(common::tree_bytes(root.path()), before);
}

#[test]
fn collision_planning_includes_nested_subagent_moves() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let mut first = conversation("first-parent");
    let second = conversation("second-parent");
    let mut child = conversation("nested-child");
    first
        .subagent_references
        .push(agent_sessions::model::SubagentReference {
            child_native_id: Some("nested-child".into()),
            child_native_key: None,
            link_key: Some("spawn".into()),
            description: Some("Nested".into()),
            agent_type: Some("worker".into()),
            instructions: None,
            spawned_at_ms: Some(ms("2026-08-31T10:00:00Z")),
        });
    child.subagent_parent = Some(agent_sessions::model::SubagentParent {
        parent_native_id: "first-parent".into(),
        description: Some("Nested".into()),
        agent_type: Some("worker".into()),
        instructions: None,
        depth: Some(1),
        native_key: None,
        link_key: Some("spawn".into()),
    });
    for session in [&first, &second, &child] {
        archive.db().unwrap().upsert_conversation(session).unwrap();
    }
    assert_eq!(archive.db().unwrap().resolve_subagent_links().unwrap(), 1);
    fs::write(
        root.path().join("config.toml"),
        r#"[views]
timezone = "UTC"
unit = "session-day"
[views.layout]
session = "notes/{{ workday }} {{ title }}.md"
loose = "loose/{{ workday }} {{ title }}.md"
subagent = "{{ parent_dir }}/{{ parent_stem }}.subagents/{{ workday }} {{ short_id }}.md"
"#,
    )
    .unwrap();
    write_digest(
        root.path(),
        "codex",
        "first-parent",
        "Topic - Shared",
        "Topic - Alpha",
    );
    write_digest(
        root.path(),
        "codex",
        "second-parent",
        "Topic - Shared",
        "Topic - Beta",
    );

    let result = run(
        &archive,
        &RetitleOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.relocked, 2);
    assert_eq!(result.renames.len(), 3);
    let by_id = result
        .renames
        .iter()
        .map(|rename| (rename.native_id.as_str(), rename))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(
        by_id["first-parent"]
            .new_path
            .ends_with("2026-08-31 Topic - Shared.md")
    );
    assert!(
        by_id["second-parent"]
            .new_path
            .ends_with("2026-08-31 Topic - Shared (2).md")
    );
    assert!(
        by_id["nested-child"]
            .new_path
            .to_string_lossy()
            .contains("2026-08-31 Topic - Shared.subagents")
    );
}
mod common;
