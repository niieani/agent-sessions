use agent_sessions::adapters::{
    Adapter, EnumerateGate, claude::ClaudeAdapter, codex::CodexAdapter, pi::PiAdapter,
};
use std::path::Path;

#[test]
fn native_and_capture_admission_paths_agree_on_file_fixtures() {
    for (adapter, root) in [
        (
            &ClaudeAdapter as &dyn Adapter,
            "tests/fixtures/claude/projects",
        ),
        (&CodexAdapter, "tests/fixtures/codex/store"),
        (&PiAdapter, "tests/fixtures/pi/store"),
    ] {
        let sessions = adapter
            .enumerate(Path::new(root), &EnumerateGate::default())
            .unwrap();
        assert!(
            !sessions.is_empty(),
            "{} fixtures missing",
            adapter.source()
        );
        let dest = tempfile::tempdir().unwrap();
        for session in sessions {
            let native = adapter.session_working_path(&session).unwrap();
            let capture = adapter.capture(&session, dest.path(), "fixture").unwrap();
            assert_eq!(
                adapter.capture_working_path(&capture).unwrap(),
                native,
                "{} {}",
                adapter.source(),
                session.native_id
            );
        }
    }
}

#[test]
fn codex_header_cwd_wins_over_turn_context_and_replayed_headers_for_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("sessions/2026/03/05");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("rollout-2026-03-05T09-00-00-aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/recorded\"}}\n",
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/ancestor\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/later\"}}\n"
        ),
    )
    .unwrap();
    let session = CodexAdapter
        .enumerate(root.path(), &EnumerateGate::default())
        .unwrap()
        .remove(0);
    assert_eq!(
        CodexAdapter
            .session_working_path(&session)
            .unwrap()
            .as_deref(),
        Some("/recorded")
    );
    let dest = tempfile::tempdir().unwrap();
    let capture = CodexAdapter
        .capture(&session, dest.path(), "fixture")
        .unwrap();
    assert_eq!(
        CodexAdapter
            .capture_working_path(&capture)
            .unwrap()
            .as_deref(),
        Some("/recorded")
    );
}
