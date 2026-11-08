use std::fs;

use agent_sessions::archive::Archive;
use agent_sessions::model::{
    Conversation, Message, MessageKind, Provenance, Role, SubagentParent, SubagentReference,
};
use agent_sessions::render::render_all;

fn conversation(id: &str, project_path: Option<&str>, timestamp_ms: i64) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "testhost".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}.jsonl"),
        },
        title: Some(format!("Session {id}")),
        project_path: project_path.map(str::to_owned),
        native_project: None,
        created_at_ms: Some(timestamp_ms),
        updated_at_ms: Some(timestamp_ms),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "hello".into(),
            timestamp_ms: Some(timestamp_ms),
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

fn with_source_and_message(mut value: Conversation, source: &str, message: &str) -> Conversation {
    value.provenance.source = source.into();
    value.messages[0].text = message.into();
    value
}

#[test]
fn ordered_kinds_match_and_apply_every_placement() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    {
        let mut db = archive.db().unwrap();
        for conv in [
            with_source_and_message(
                conversation("omit", Some("/tmp/a"), 1_786_080_000_000),
                "codex",
                "action: capture backup",
            ),
            with_source_and_message(
                conversation("group", Some("/eval/one/vault"), 1_786_080_000_000),
                "claude",
                "evaluate",
            ),
            with_source_and_message(
                conversation("subfolder", Some("/work/app"), 1_786_080_000_000),
                "codex",
                "<workflow-marker>run",
            ),
            with_source_and_message(
                conversation("loose", Some("/scratch/x"), 1_786_080_000_000),
                "pi",
                "scratch",
            ),
            conversation("default", Some("/work/default"), 1_786_080_000_000),
            conversation("missing", None, 1_786_080_000_000),
        ] {
            db.upsert_conversation(&conv).unwrap();
        }
    }
    fs::write(
        dir.path().join("config.toml"),
        r#"
[views.kind.mechanical]
match.message.contains = ["action: capture", "backup"]
omit = true

[views.kind.evals]
match.path.glob = "/eval/**/vault"
match.source = "claude"
output = "example-assistant-evals/automations"

[views.kind.workflow]
match.message.starts-with = "<workflow-marker>"
output = "./automations"

[views.kind.scratch]
loose = true
[[views.kind.scratch.match]]
path.is = "/does/not/match"
[[views.kind.scratch.match]]
message.matches = "^scratch$"
"#,
    )
    .unwrap();

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 5);
    assert_eq!(
        stats
            .claims
            .iter()
            .map(|c| (&*c.name, c.sessions))
            .collect::<Vec<_>>(),
        vec![
            ("mechanical", 1),
            ("evals", 1),
            ("workflow", 1),
            ("scratch", 1),
            ("default", 2)
        ]
    );
    assert!(
        dir.path()
            .join("views/projects/example-assistant-evals/automations/INDEX.md")
            .is_file()
    );
    assert!(
        dir.path()
            .join("views/projects/app/automations/INDEX.md")
            .is_file()
    );
    assert!(dir.path().join("views/loose/2026-08/INDEX.md").is_file());
    assert!(dir.path().join("views/projects/default/INDEX.md").is_file());
    assert!(!dir.path().join("views/projects/a").exists());

    // Classification is a render-only choice: removing the pattern creates
    // the Project View without rebuilding or re-ingesting anything.
    fs::write(dir.path().join("config.toml"), "[views]\n").unwrap();
    let rerender = render_all(&archive).unwrap();
    assert!(dir.path().join("views/projects/a/INDEX.md").is_file());
    assert!(rerender.removed > 0);
    let old_loose = dir.path().join("views/loose/2026-08");
    assert!(
        !old_loose.exists()
            || fs::read_dir(old_loose).unwrap().all(|entry| {
                !fs::read_to_string(entry.unwrap().path())
                    .unwrap_or_default()
                    .contains("Session loose")
            }),
        "reclassified Transcript is swept from its old View group"
    );

    fs::write(
        dir.path().join("config.toml"),
        "[views.kind.mechanical]\nmatch.message.contains = ['action: capture']\nomit = true\n",
    )
    .unwrap();
    let omitted_again = render_all(&archive).unwrap();
    assert!(omitted_again.removed > 0);
    assert!(!dir.path().join("views/projects/a").exists());
}

#[test]
fn declaration_order_not_alphabetical_order_is_priority() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "priority",
            Some("/work/p"),
            1_786_080_000_000,
        ))
        .unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "[views.kind.z-first]\nmatch.message.is = 'hello'\noutput = 'winner'\n\n[views.kind.a-second]\nmatch.message.is = 'hello'\nomit = true\n",
    )
    .unwrap();
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 1);
    assert!(dir.path().join("views/projects/winner/INDEX.md").is_file());
    assert_eq!(stats.claims[0].name, "z-first");
    assert_eq!(stats.claims[0].sessions, 1);
}

#[test]
fn combined_matchers_and_no_user_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let matching = with_source_and_message(
        conversation("matching", Some("/exact/path"), 1_786_080_000_000),
        "vscode",
        "prefix alpha beta suffix",
    );
    let mut no_user = matching.clone();
    no_user.provenance.native_id = "no-user".into();
    no_user.messages[0].role = Role::Assistant;
    {
        let mut db = archive.db().unwrap();
        db.upsert_conversation(&matching).unwrap();
        db.upsert_conversation(&no_user).unwrap();
    }
    fs::write(
        dir.path().join("config.toml"),
        r#"
[views.kind.combined]
match.path.is = "/exact/path"
match.message.starts-with = "prefix"
match.message.ends-with = "suffix"
match.message.contains = ["alpha", "beta"]
match.source = "vscode"
output = "matched"
"#,
    )
    .unwrap();
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.claims[0].sessions, 1);
    assert_eq!(stats.claims[1].sessions, 1);
    assert!(dir.path().join("views/projects/matched/INDEX.md").is_file());
}

#[test]
fn every_kind_config_error_preflights_before_view_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let views = dir.path().join("views");
    let sentinel = views.join("keep.txt");
    fs::write(&sentinel, "untouched").unwrap();
    for invalid in [
        "[views]\nunfiled-patterns = ['old']\n",
        "[views.kind.Bad_Name]\nmatch.source = 'codex'\nomit = true\n",
        "[views.kind.x]\nomit = true\n",
        "[views.kind.x]\nmatch = {}\nomit = true\n",
        "[views.kind.x]\nmatch.path.glob = '['\nomit = true\n",
        "[views.kind.x]\nmatch.message.matches = '['\nomit = true\n",
        "[views.kind.x]\nmatch.source = 't3'\nomit = true\n",
        "[views.kind.x]\nmatch.source = ['codex']\nomit = true\n",
        "[views.kind.x]\nmatch.path.is = ['/a']\nomit = true\n",
        "[views.kind.x]\nmatch.source = 'codex'\nomit = true\nloose = true\n",
        "[views.kind.x]\nmatch.source = 'codex'\nomit = false\n",
        "[views.kind.x]\nmatch.source = 'codex'\noutput = '../escape'\n",
        "[views.kind.x]\nmatch.source = 'codex'\noutput = './'\n",
        "[views.kind.x]\nmatch.unknown = 'x'\nomit = true\n",
    ] {
        fs::write(dir.path().join("config.toml"), invalid).unwrap();
        let error = render_all(&archive).unwrap_err();
        assert!(!format!("{error:#}").is_empty(), "{invalid}");
        assert_eq!(
            fs::read_to_string(&sentinel).unwrap(),
            "untouched",
            "{invalid}"
        );
    }
}

#[test]
fn orphan_subagent_is_rendered_for_recovery_but_not_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut child = conversation("orphan", Some("/work/project"), 1_786_080_000_000);
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "missing-parent".into(),
        description: Some("Recover work".into()),
        agent_type: Some("Explore".into()),
        instructions: Some("hello".into()),
        depth: Some(1),
        native_key: Some("a1".into()),
        link_key: None,
    });
    {
        let mut db = archive.db().unwrap();
        db.upsert_conversation(&child).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 0);
    }

    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.transcripts, 1);
    assert_eq!(stats.indexes, 0);
    let files = fs::read_dir(dir.path().join("views/projects/project/subagents"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let transcript = fs::read_to_string(&files[0]).unwrap();
    assert!(transcript.contains("\n\nhello\n\n</parent>"));
    assert!(transcript.contains("parentId: missing-parent"));
}

#[test]
fn subagent_transcripts_are_nested_and_parent_links_remain_relative() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let parent = conversation("parent", Some("/work/project"), 1_786_080_000_000);
    let mut child = conversation("child", Some("/other/child"), 1_786_080_001_000);
    child.messages[0].text = "child-only".into();
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: parent.provenance.native_id.clone(),
        description: Some("Map project".into()),
        agent_type: Some("Explore".into()),
        instructions: Some("Inspect the project.".into()),
        depth: Some(1),
        native_key: None,
        link_key: Some("task-1".into()),
    });
    let mut parent = parent;
    parent.subagent_references.push(SubagentReference {
        child_native_id: Some(child.provenance.native_id.clone()),
        child_native_key: None,
        link_key: Some("task-1".into()),
        description: Some("Map project".into()),
        agent_type: Some("Explore".into()),
        instructions: Some("Inspect the project.".into()),
        spawned_at_ms: Some(1_786_080_000_500),
    });
    {
        let mut db = archive.db().unwrap();
        db.upsert_conversation(&parent).unwrap();
        db.upsert_conversation(&child).unwrap();
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
    }

    fs::write(
        dir.path().join("config.toml"),
        r#"
[views.kind.child-direct]
match.message.is = "child-only"
omit = true

[views.kind.parent-claim]
match.message.is = "hello"
output = "claimed-together"
"#,
    )
    .unwrap();

    assert_eq!(render_all(&archive).unwrap().transcripts, 2);
    let group = dir.path().join("views/projects/claimed-together");
    let child_dir = group.join("subagents");
    let child_path = fs::read_dir(&child_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("md"))
        .expect("nested child Transcript");
    let child_name = child_path.file_name().unwrap().to_string_lossy();
    assert!(
        fs::read_to_string(&child_path)
            .unwrap()
            .contains("\n\nchild-only\n\n</parent>")
    );

    let parent_path = fs::read_dir(&group)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension().and_then(|extension| extension.to_str()) == Some("md")
                && path.file_name().unwrap() != "INDEX.md"
        })
        .expect("parent Transcript");
    let parent_text = fs::read_to_string(parent_path).unwrap();
    assert!(parent_text.contains(&format!("[Session transcript](subagents/{child_name})")));
    assert!(!group.join(child_name.as_ref()).exists());
    assert!(
        !fs::read_to_string(group.join("INDEX.md"))
            .unwrap()
            .contains("Map project")
    );
}

#[test]
fn own_project_output_without_claimed_root_path_fails_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation("pathless", None, 1_786_080_000_000))
        .unwrap();
    let sentinel = archive.views_dir().join("keep.txt");
    fs::write(&sentinel, "keep").unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "[views.kind.path-required]\nmatch.message.is = 'hello'\noutput = './automation'\n",
    )
    .unwrap();
    let error = render_all(&archive).unwrap_err();
    assert!(format!("{error:#}").contains("requires claimed top-level Session"));
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
}

#[test]
fn child_parent_fact_overrides_cloned_parent_side_references() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let mut child = conversation("shared-child", Some("/work/p"), 1_786_080_002_000);
    child.subagent_parent = Some(SubagentParent {
        parent_native_id: "parent-b".into(),
        description: Some("Actual parent".into()),
        agent_type: None,
        instructions: None,
        depth: None,
        native_key: None,
        link_key: Some("tool-call".into()),
    });
    let mut parents = [
        conversation("parent-a", Some("/work/p"), 1_786_080_000_000),
        conversation("parent-b", Some("/work/p"), 1_786_080_001_000),
    ];
    for parent in &mut parents {
        parent.subagent_references.push(SubagentReference {
            child_native_id: Some(child.provenance.native_id.clone()),
            child_native_key: None,
            link_key: None,
            description: None,
            agent_type: None,
            instructions: None,
            spawned_at_ms: None,
        });
    }
    {
        let mut db = archive.db().unwrap();
        db.upsert_conversation(&child).unwrap();
        for parent in &parents {
            db.upsert_conversation(parent).unwrap();
        }
        assert_eq!(db.resolve_subagent_links().unwrap(), 1);
        let links = db.subagent_links("codex", "parent-b").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link.child_native_id, "shared-child");
        assert!(db.subagent_links("codex", "parent-a").unwrap().is_empty());
    }
    assert_eq!(render_all(&archive).unwrap().transcripts, 3);
}

#[test]
fn conflicting_parent_side_references_without_a_child_parent_remain_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let child = conversation("shared-child", Some("/work/p"), 1_786_080_002_000);
    let mut parents = [
        conversation("parent-a", Some("/work/p"), 1_786_080_000_000),
        conversation("parent-b", Some("/work/p"), 1_786_080_001_000),
    ];
    for parent in &mut parents {
        parent.subagent_references.push(SubagentReference {
            child_native_id: Some(child.provenance.native_id.clone()),
            child_native_key: None,
            link_key: None,
            description: None,
            agent_type: None,
            instructions: None,
            spawned_at_ms: None,
        });
    }
    {
        let mut db = archive.db().unwrap();
        db.upsert_conversation(&child).unwrap();
        for parent in &parents {
            db.upsert_conversation(parent).unwrap();
        }
        assert_eq!(db.resolve_subagent_links().unwrap(), 0);
        assert!(db.subagent_links("codex", "parent-a").unwrap().is_empty());
        assert!(db.subagent_links("codex", "parent-b").unwrap().is_empty());
    }
    assert_eq!(render_all(&archive).unwrap().transcripts, 3);
}

#[cfg(unix)]
#[test]
fn render_never_follows_view_symlinks_outside_the_archive() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, "keep me").unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(
            "safe",
            Some("/work/project"),
            1_786_080_000_000,
        ))
        .unwrap();

    symlink(external.path(), archive.views_dir().join("projects")).unwrap();
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.removed, 1, "nested symlink itself was swept");
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep me");
    assert!(
        archive
            .views_dir()
            .join("projects/project/INDEX.md")
            .is_file()
    );

    fs::remove_dir_all(archive.views_dir()).unwrap();
    symlink(external.path(), archive.views_dir()).unwrap();
    let error = render_all(&archive).unwrap_err();
    assert!(error.to_string().contains("symlinked Views root"));
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep me");
}
