//! End-to-end slice 1 test: pull from a temp Store built from fixtures into
//! a temp Archive, then assert captures layout, database rows, and rendered
//! Views (golden files).

mod common;

use std::fs;
use std::io::Write as _;
use std::thread::sleep;
use std::time::Duration;

use agent_sessions::archive::Archive;
use agent_sessions::model::Role;
use agent_sessions::render::render_all;

use common::{HOST, fixtures_dir, pin_utc_workday, pull, temp_store, tree_bytes};

/// Build a temp Store from fixtures, pull it into a temp Archive, render.
fn pulled_archive() -> (tempfile::TempDir, tempfile::TempDir) {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pin_utc_workday(archive_dir.path());
    // These archival/workday fixtures deliberately use nonexistent workspaces;
    // pin their Project membership independently of repository lookup.
    use std::io::Write as _;
    writeln!(fs::OpenOptions::new().append(true).open(archive_dir.path().join("config.toml")).unwrap(), "[views.project-names]\n\"/workspace/Projects/example\" = \"example\"\n\"/workspace/Projects/webapp\" = \"webapp\"").unwrap();

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 4, "3 main + 1 subagent");
    assert_eq!(stats.sessions_captured, 4);
    assert_eq!(stats.conversations_extracted, 4);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let rstats = render_all(&archive).unwrap();
    assert_eq!(rstats.transcripts, 4);
    assert_eq!(rstats.indexes, 2);
    (store, archive_dir)
}

#[test]
fn captures_layout_matches_spec() {
    let (_store, archive_dir) = pulled_archive();
    let captures = archive_dir
        .path()
        .join("captures")
        .join(HOST)
        .join("claude");

    for id in [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
        "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef",
    ] {
        assert!(
            captures.join(format!("{id}.jsonl.zst")).is_file(),
            "{id} body"
        );
        assert!(
            captures.join(format!("{id}.meta.json")).is_file(),
            "{id} sidecar"
        );
    }

    // Captures are verbatim: decompressing returns the original bytes.
    let original =
        fs::read(fixtures_dir().join(
            "projects/-workspace-Projects-example/11111111-1111-4111-8111-111111111111.jsonl",
        ))
        .unwrap();
    let compressed =
        fs::File::open(captures.join("11111111-1111-4111-8111-111111111111.jsonl.zst")).unwrap();
    let decompressed = zstd::stream::decode_all(compressed).unwrap();
    assert_eq!(decompressed, original);

    // The subagent sidecar embeds the native meta.json verbatim.
    let sidecar: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(captures.join(
            "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef.meta.json",
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["kind"], "subagent");
    assert_eq!(sidecar["nativeMeta"]["agentType"], "Explore");
    assert_eq!(sidecar["host"], HOST);
    assert!(sidecar["nativePath"].as_str().unwrap().ends_with(".jsonl"));
}

#[test]
fn database_rows_match_fixtures() {
    let (_store, archive_dir) = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();

    assert_eq!(db.session_count().unwrap(), 4);
    let sessions = db.sessions_with_dialog().unwrap();
    assert_eq!(sessions.len(), 4);

    let parent = sessions
        .iter()
        .find(|s| s.native_id == "33333333-3333-4333-8333-333333333333")
        .unwrap();
    let links = db
        .subagent_links(&parent.source, &parent.native_id)
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link.description.as_deref(), Some("Map codebase"));
    assert_eq!(links[0].link.agent_type.as_deref(), Some("Explore"));
    let child = sessions
        .iter()
        .find(|s| s.id == links[0].child_session_id)
        .unwrap();
    assert_eq!(
        child.subagent_parent.as_ref().unwrap().parent_native_id,
        parent.native_id
    );

    let s1 = sessions
        .iter()
        .find(|s| s.native_id == "11111111-1111-4111-8111-111111111111")
        .unwrap();
    assert_eq!(s1.title.as_deref(), Some("TOML config loader"));
    assert_eq!(s1.project.as_deref(), Some("example"));
    assert_eq!(
        s1.project_path.as_deref(),
        Some("/workspace/Projects/example")
    );
    assert_eq!(s1.message_count, 6);
    assert_eq!(s1.hosts, vec![HOST.to_string()]);
    assert_eq!(
        s1.transcript_facts.starting_model.as_deref(),
        Some("claude-opus-5")
    );
    assert_eq!(
        s1.transcript_facts
            .git
            .as_ref()
            .and_then(|git| git.branch.as_deref()),
        Some("main")
    );

    let messages = db.messages(s1.id).unwrap();
    assert_eq!(messages.len(), 6);
    // Noise excluded: no isMeta caveat, no synthetic assistant text.
    assert!(!messages.iter().any(|m| m.text.contains("Caveat")));
    assert!(!messages.iter().any(|m| m.text.contains("API Error")));
    // Seam counts: 2 tool_uses before "Tests pass.", 1 before "Docs updated."
    assert_eq!(messages[2].actions_before, 2);
    assert_eq!(messages[5].actions_before, 1);
}

#[test]
fn queued_prompt_attachments_are_user_dialog_in_native_order() {
    let store = tempfile::tempdir().unwrap();
    let project = store.path().join("-work-queued");
    fs::create_dir_all(&project).unwrap();
    let native_id = "44444444-4444-4444-8444-444444444444";
    fs::write(
        project.join(format!("{native_id}.jsonl")),
        concat!(
            r#"{"type":"assistant","cwd":"/work/queued","timestamp":"2026-08-28T10:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Ready."}]}}"#,
            "\n",
            r#"{"type":"attachment","timestamp":"2026-08-28T10:00:01Z","attachment":{"type":"queued_command","commandMode":"prompt","prompt":"repeat me"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-28T10:00:02Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{}}]}}"#,
            "\n",
            r#"{"type":"attachment","timestamp":"2026-08-28T10:00:03Z","attachment":{"type":"queued_command","commandMode":"prompt","prompt":[{"type":"text","text":"repeat me"}]}}"#,
            "\n",
            r#"{"type":"attachment","timestamp":"2026-08-28T10:00:04Z","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"exclude me"}}"#,
            "\n",
            r#"{"type":"attachment","timestamp":"2026-08-28T10:00:05Z","attachment":{"type":"queued_command","commandMode":"prompt","prompt":[]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-28T10:00:06Z","message":{"role":"assistant","content":"Done."}}"#,
            "\n",
        ),
    )
    .unwrap();
    let archive_dir = tempfile::tempdir().unwrap();
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_failed, 0);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == native_id)
        .unwrap();
    let messages = db.messages(session.id).unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.text.as_str(), message.timestamp_ms))
            .collect::<Vec<_>>(),
        vec![
            (Role::Assistant, "Ready.", Some(1_787_911_200_000)),
            (Role::User, "repeat me", Some(1_787_911_201_000)),
            (Role::User, "repeat me", Some(1_787_911_203_000)),
            (Role::Assistant, "Done.", Some(1_787_911_206_000)),
        ]
    );
    assert_eq!(messages[2].actions_before, 1);
    let dump_before = db.content_dump().unwrap();
    drop(db);

    render_all(&archive).unwrap();
    let views_before = tree_bytes(&archive.views_dir());
    let transcript = views_before
        .iter()
        .find(|(path, _)| path.ends_with(".claude.md"))
        .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        .unwrap();
    assert_eq!(transcript.matches("repeat me").count(), 2);
    assert!(!transcript.contains("exclude me"));

    agent_sessions::archive::rebuild(&archive).unwrap();
    render_all(&archive).unwrap();
    assert_eq!(archive.db().unwrap().content_dump().unwrap(), dump_before);
    assert_eq!(tree_bytes(&archive.views_dir()), views_before);
}

#[test]
fn pull_is_idempotent() {
    let (store, archive_dir) = pulled_archive();

    // Second pull: the Bookmark makes it a no-op at enumerate time.
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0, "bookmark skips everything");
    assert_eq!(stats.conversations_extracted, 0);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    assert_eq!(db.session_count().unwrap(), 4);
    let s1 = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == "11111111-1111-4111-8111-111111111111")
        .unwrap();
    assert_eq!(
        db.messages(s1.id).unwrap().len(),
        6,
        "no duplicated messages"
    );
}

#[test]
fn one_bad_session_does_not_abort_source_and_bookmark_waits_for_retry() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let bookmark_before = archive.db().unwrap().bookmark(HOST, "claude").unwrap();

    sleep(Duration::from_millis(25));
    let project = store.path().join("-workspace-Projects-example");
    let main = project.join("11111111-1111-4111-8111-111111111111.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(main)
        .unwrap()
        .write_all(
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"still succeeds"},"timestamp":"2026-03-08T00:00:00Z","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let subagent_dir = project.join("33333333-3333-4333-8333-333333333333/subagents");
    let subagent = subagent_dir.join("agent-a0123456789abcdef.jsonl");
    writeln!(
        fs::OpenOptions::new().append(true).open(&subagent).unwrap(),
        "{{}}"
    )
    .unwrap();
    let meta = subagent_dir.join("agent-a0123456789abcdef.meta.json");
    let valid_meta = fs::read(&meta).unwrap();
    fs::write(&meta, "not json").unwrap();

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_failed, 1);
    assert_eq!(
        stats.conversations_extracted, 1,
        "good Session still ingested"
    );
    assert_eq!(
        archive.db().unwrap().bookmark(HOST, "claude").unwrap(),
        bookmark_before
    );

    fs::write(meta, valid_meta).unwrap();
    let retry = pull(store.path(), archive_dir.path());
    assert_eq!(retry.sessions_failed, 0);
    assert!(archive.db().unwrap().bookmark(HOST, "claude").unwrap() > bookmark_before);
}

#[test]
fn rendered_views_match_goldens() {
    let (_store, archive_dir) = pulled_archive();
    let views = archive_dir.path().join("views");
    let expected_root = fixtures_dir().join("expected");

    let actual = tree_bytes(&views.join("projects"));
    let expected = tree_bytes(&expected_root);
    assert_eq!(actual.len(), expected.len());
    for ((path, actual), (expected_path, expected)) in actual.iter().zip(expected.iter()) {
        assert_eq!(path, expected_path);
        assert_eq!(
            String::from_utf8_lossy(actual),
            String::from_utf8_lossy(expected),
            "{path}"
        );
    }
}
