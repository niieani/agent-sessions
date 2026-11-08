mod common;
#[path = "push_titles/support.rs"]
mod support;

use std::fs;
use std::io::Write as _;

use agent_sessions::digest::digest_dir;
use agent_sessions::push_titles::{PushTitleOptions, run};
use common::{empty_roots, tree_bytes};
use rusqlite::Connection;
use support::*;

#[test]
fn dry_run_plans_owned_and_empty_titles_without_writing_any_file() {
    let fixture = Fixture::new();
    let stores_before = tree_bytes(fixture.stores_dir.path());
    let digests_before = tree_bytes(&fixture.archive_dir.path().join("digests"));
    let result = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: None,
            dry_run: true,
        },
    )
    .unwrap();

    assert_eq!(result.pushed, 4);
    assert_eq!(result.kept, 2);
    assert_eq!(tree_bytes(fixture.stores_dir.path()), stores_before);
    assert_eq!(
        tree_bytes(&fixture.archive_dir.path().join("digests")),
        digests_before
    );
}

#[test]
fn live_push_changes_only_owned_title_fields_and_records_ownership() {
    let fixture = Fixture::new();
    let foreign_claude_before =
        fs::read(claude_path(&fixture.claude_root, CLAUDE_FOREIGN)).unwrap();
    let result = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: None,
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(result.pushed, 4);
    assert_eq!(result.kept, 2);

    let state = Connection::open(fixture.codex_root.join("state_5.sqlite")).unwrap();
    let rows = state
        .prepare("SELECT id, name, title, marker FROM threads ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                CODEX_FRESH.into(),
                Some("Codex Fresh".into()),
                "raw".into(),
                "keep".into()
            ),
            (
                CODEX_OWN.into(),
                Some("Codex Updated".into()),
                "raw".into(),
                "keep".into()
            ),
            (
                CODEX_FOREIGN.into(),
                Some("Human Codex".into()),
                "raw".into(),
                "keep".into()
            ),
        ]
    );

    assert_eq!(
        last_custom_title(&claude_path(&fixture.claude_root, CLAUDE_FRESH)),
        Some("Claude Fresh".into())
    );
    assert_eq!(
        last_custom_title(&claude_path(&fixture.claude_root, CLAUDE_OWN)),
        Some("Claude Updated".into())
    );
    assert_eq!(
        read_claude_sidecar(&fixture.claude_root, CLAUDE_FRESH),
        "Claude Fresh"
    );
    assert_eq!(
        read_claude_sidecar(&fixture.claude_root, CLAUDE_OWN),
        "Claude Updated"
    );
    assert_eq!(
        fs::read(claude_path(&fixture.claude_root, CLAUDE_FOREIGN)).unwrap(),
        foreign_claude_before
    );
    assert!(
        !fixture
            .claude_root
            .join("-work-project")
            .join(CLAUDE_FOREIGN)
            .join("custom-title.json")
            .exists()
    );

    for (source, id, expected) in [
        ("codex", CODEX_FRESH, Some("Codex Fresh")),
        ("codex", CODEX_OWN, Some("Codex Updated")),
        ("codex", CODEX_FOREIGN, None),
        ("claude", CLAUDE_FRESH, Some("Claude Fresh")),
        ("claude", CLAUDE_OWN, Some("Claude Updated")),
        ("claude", CLAUDE_FOREIGN, None),
    ] {
        assert_eq!(
            read_pushed(fixture.archive_dir.path(), source, id),
            expected.map(str::to_string)
        );
    }
}

#[test]
fn pull_after_push_recaptures_the_native_titles() {
    let fixture = Fixture::new();
    run(
        &fixture.archive(),
        &PushTitleOptions {
            source: None,
            dry_run: false,
        },
    )
    .unwrap();
    let mut roots = empty_roots(fixture.stores_dir.path());
    roots.codex = fixture.codex_root.clone();
    roots.claude = fixture.claude_root.clone();
    agent_sessions::pull::pull_localhost(&fixture.archive(), &roots, HOST).unwrap();
    let sessions = fixture
        .archive()
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap();
    for (source, id, expected) in [
        ("codex", CODEX_FRESH, "Codex Fresh"),
        ("codex", CODEX_OWN, "Codex Updated"),
        ("claude", CLAUDE_FRESH, "Claude Fresh"),
        ("claude", CLAUDE_OWN, "Claude Updated"),
    ] {
        let session = sessions
            .iter()
            .find(|row| row.source == source && row.native_id == id)
            .unwrap();
        assert_eq!(session.title.as_deref(), Some(expected));
    }
}

#[test]
fn second_push_is_an_exact_no_op_for_our_titles() {
    let fixture = Fixture::new();
    let archive = fixture.archive();
    run(
        &archive,
        &PushTitleOptions {
            source: None,
            dry_run: false,
        },
    )
    .unwrap();
    let stores_before = tree_bytes(fixture.stores_dir.path());
    let digests_before = tree_bytes(&fixture.archive_dir.path().join("digests"));

    let second = run(
        &archive,
        &PushTitleOptions {
            source: None,
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(second.pushed, 0);
    assert_eq!(second.unchanged, 4);
    assert_eq!(second.kept, 2);
    assert_eq!(tree_bytes(fixture.stores_dir.path()), stores_before);
    assert_eq!(
        tree_bytes(&fixture.archive_dir.path().join("digests")),
        digests_before
    );
}

#[test]
fn unsupported_source_fails_without_writing() {
    let fixture = Fixture::new();
    let stores_before = tree_bytes(fixture.stores_dir.path());
    let error = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("cursor".into()),
            dry_run: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("no writable route"));
    assert_eq!(tree_bytes(fixture.stores_dir.path()), stores_before);
}

#[cfg(unix)]
#[test]
fn claude_sidecar_preflight_fails_before_appending() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    let before = fs::read(&transcript).unwrap();
    let project = transcript.parent().unwrap();
    let outside = fixture.stores_dir.path().join("outside-sidecar");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, project.join(CLAUDE_FRESH)).unwrap();

    let error = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("not a real directory"));
    assert_eq!(fs::read(transcript).unwrap(), before);
    assert!(
        !digest_dir(fixture.archive_dir.path(), "claude", CLAUDE_FRESH)
            .unwrap()
            .join("push-title.pending")
            .exists()
    );
}

#[test]
fn pending_claude_write_recovers_without_a_duplicate_append() {
    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    let checked_len = fs::metadata(&transcript).unwrap().len();
    let record = serde_json::json!({
        "type": "custom-title",
        "customTitle": "Claude Fresh",
        "sessionId": CLAUDE_FRESH,
    });
    fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(format!("{}\n", record).as_bytes())
        .unwrap();
    write_claude_pending_at(
        &fixture,
        CLAUDE_FRESH,
        "Claude Fresh",
        Some("Claude Fresh"),
        Some(checked_len),
    );
    let digest_dir = digest_dir(fixture.archive_dir.path(), "claude", CLAUDE_FRESH).unwrap();

    run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(custom_title_count(&transcript), 1);
    assert_eq!(
        read_pushed(fixture.archive_dir.path(), "claude", CLAUDE_FRESH).as_deref(),
        Some("Claude Fresh")
    );
    assert_eq!(
        read_claude_sidecar(&fixture.claude_root, CLAUDE_FRESH),
        "Claude Fresh"
    );
    assert!(!digest_dir.join("push-title.pending").exists());
}

#[test]
fn pending_claude_write_finishes_old_title_before_pushing_refreshed_digest() {
    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    let checked_len = fs::metadata(&transcript).unwrap().len();
    append_claude_title(&transcript, CLAUDE_FRESH, "Interrupted Claude");
    write_claude_pending_at(
        &fixture,
        CLAUDE_FRESH,
        "Interrupted Claude",
        Some("Interrupted Claude"),
        Some(checked_len),
    );
    write_session_digest(
        fixture.archive_dir.path(),
        "claude",
        CLAUDE_FRESH,
        "Refreshed Claude",
        None,
    );

    run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap();

    assert_eq!(custom_title_count(&transcript), 2);
    assert_eq!(
        last_custom_title(&transcript).as_deref(),
        Some("Refreshed Claude")
    );
    assert_eq!(
        read_claude_sidecar(&fixture.claude_root, CLAUDE_FRESH),
        "Refreshed Claude"
    );
    assert_eq!(
        read_pushed(fixture.archive_dir.path(), "claude", CLAUDE_FRESH).as_deref(),
        Some("Refreshed Claude")
    );
    assert!(
        !claude_pending_dir(&fixture, CLAUDE_FRESH)
            .join("push-title.pending")
            .exists()
    );
    assert!(!claude_sidecar_temp(&fixture, CLAUDE_FRESH).exists());
}

#[test]
fn foreign_claude_title_abandons_stale_pending_sidecar_temp() {
    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    append_claude_title(&transcript, CLAUDE_FRESH, "Human After Crash");
    write_claude_pending(&fixture, CLAUDE_FRESH, "Claude Fresh", Some("Claude Fresh"));

    let result = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap();

    assert_eq!(result.kept, 2);
    assert_eq!(custom_title_count(&transcript), 1);
    assert_eq!(
        last_custom_title(&transcript).as_deref(),
        Some("Human After Crash")
    );
    assert!(
        !claude_pending_dir(&fixture, CLAUDE_FRESH)
            .join("push-title.pending")
            .exists()
    );
    assert!(!claude_sidecar_temp(&fixture, CLAUDE_FRESH).exists());
}

#[test]
fn pending_claude_recovery_restores_foreign_title_appended_before_ours() {
    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    let checked_len = fs::metadata(&transcript).unwrap().len();
    append_claude_title(&transcript, CLAUDE_FRESH, "Human During Push");
    append_claude_title(&transcript, CLAUDE_FRESH, "Claude Fresh");
    write_claude_pending_at(
        &fixture,
        CLAUDE_FRESH,
        "Claude Fresh",
        Some("Claude Fresh"),
        Some(checked_len),
    );

    let error = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("restored the foreign title"));
    assert_eq!(
        last_custom_title(&transcript).as_deref(),
        Some("Human During Push")
    );
    assert_eq!(
        read_claude_sidecar(&fixture.claude_root, CLAUDE_FRESH),
        "Human During Push"
    );
    assert_eq!(
        read_pushed(fixture.archive_dir.path(), "claude", CLAUDE_FRESH),
        None
    );
    assert!(
        !claude_pending_dir(&fixture, CLAUDE_FRESH)
            .join("push-title.pending")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn pending_claude_recovery_rejects_a_prepared_sidecar_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let transcript = claude_path(&fixture.claude_root, CLAUDE_FRESH);
    let checked_len = fs::metadata(&transcript).unwrap().len();
    append_claude_title(&transcript, CLAUDE_FRESH, "Claude Fresh");
    write_claude_pending_at(
        &fixture,
        CLAUDE_FRESH,
        "Claude Fresh",
        None,
        Some(checked_len),
    );
    let temp = claude_sidecar_temp(&fixture, CLAUDE_FRESH);
    fs::create_dir(temp.parent().unwrap()).unwrap();
    let outside = fixture.stores_dir.path().join("outside-sidecar.json");
    fs::write(&outside, r#"{"customTitle":"Claude Fresh"}"#).unwrap();
    symlink(&outside, &temp).unwrap();
    let canonical_temp = fs::canonicalize(&transcript)
        .unwrap()
        .parent()
        .unwrap()
        .join(CLAUDE_FRESH)
        .join("custom-title.json.agent-sessions.tmp");
    assert!(
        fs::symlink_metadata(&canonical_temp)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let error = run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("claude".into()),
            dry_run: false,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("temp is not a regular file"));
    assert_eq!(
        fs::read_to_string(outside).unwrap(),
        r#"{"customTitle":"Claude Fresh"}"#
    );
    assert!(temp.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn pending_codex_write_recovers_ownership_without_rewriting_the_store() {
    let fixture = Fixture::new();
    let state_path = fixture.codex_root.join("state_5.sqlite");
    let state = Connection::open(&state_path).unwrap();
    state
        .execute(
            "UPDATE threads SET name = 'Codex Fresh' WHERE id = ?1",
            [CODEX_FRESH],
        )
        .unwrap();
    drop(state);
    let digest_dir = digest_dir(fixture.archive_dir.path(), "codex", CODEX_FRESH).unwrap();
    fs::write(
        digest_dir.join("push-title.pending"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "source": "codex",
            "nativeId": CODEX_FRESH,
            "previous": null,
            "title": "Codex Fresh",
        }))
        .unwrap(),
    )
    .unwrap();

    run(
        &fixture.archive(),
        &PushTitleOptions {
            source: Some("codex".into()),
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(
        read_pushed(fixture.archive_dir.path(), "codex", CODEX_FRESH).as_deref(),
        Some("Codex Fresh")
    );
    assert!(!digest_dir.join("push-title.pending").exists());
    let state = Connection::open(state_path).unwrap();
    assert_eq!(
        state
            .query_row(
                "SELECT name, title, marker FROM threads WHERE id = ?1",
                [CODEX_FRESH],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ))
            )
            .unwrap(),
        ("Codex Fresh".into(), "raw".into(), "keep".into())
    );
}
