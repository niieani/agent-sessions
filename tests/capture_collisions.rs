mod common;
use agent_sessions::{
    adapters::{Adapter, EnumerateGate, claude::ClaudeAdapter},
    archive::Archive,
};
use std::fs;

fn body(text: &str, cwd: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"user","cwd":cwd,"timestamp":"2026-10-30T05:24:03Z","message":{"role":"user","content":text}})
    )
}

#[test]
fn nineteen_copies_settle_once_and_winner_changes_revise() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    for i in 0..19 {
        let dir = store.path().join(format!("project-{i:02}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("same.jsonl"), body(&format!("copy {i}"), "/work")).unwrap();
    }
    let first = common::pull(store.path(), dest.path());
    assert_eq!(first.sessions_new, 1);
    assert_eq!(first.sessions_revised, 0);
    let before = common::tree_bytes(&dest.path().join("captures"));
    let second = common::pull(store.path(), dest.path());
    assert_eq!(second.sessions_revised, 0);
    assert_eq!(before, common::tree_bytes(&dest.path().join("captures")));
    fs::write(
        store.path().join("project-18/same.jsonl"),
        body("losing copy changed substantially", "/work"),
    )
    .unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_found, 0);
    fs::write(
        store.path().join("project-00/same.jsonl"),
        body("winning copy changed", "/work"),
    )
    .unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_revised, 1);
    assert_eq!(common::pull(store.path(), dest.path()).sessions_revised, 0);
}

#[test]
fn exclusion_precedes_election_and_preserves_human_worktrees() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    for (i, cwd) in [
        "/tmp/example-assistant-eval-workspaces/run/vault",
        "/tmp/temp.local/eval-workspaces/run/vault",
        "/home/u/.t3/worktrees/example-assistant/eval-fidelity-audit",
        "/home/u/.t3/worktrees/example-assistant/eval-claude-continuations",
        "/home/u/.t3/worktrees/example-assistant/eval-direction",
        "/home/u/.t3/worktrees/example-assistant/eval-codex-resume",
        "/home/u/.t3/worktrees/example-assistant/eval-direction/eval-workspaces/run/vault",
    ]
    .iter()
    .enumerate()
    {
        let dir = store.path().join(format!("project-{i}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("s{i}.jsonl")), body("hello", cwd)).unwrap();
    }
    fs::write(
        dest.path().join("config.toml"),
        "[pull]\nexclude-paths = ['**/*eval-workspaces/**/vault']\n",
    )
    .unwrap();
    let stats = common::pull(store.path(), dest.path());
    assert_eq!(stats.sessions_new, 4);
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(stats.sessions_unchanged, 0);
    let archive = Archive::open(dest.path()).unwrap();
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 4);
    assert_eq!(
        ClaudeAdapter
            .enumerate(store.path(), &EnumerateGate::default())
            .unwrap()
            .len(),
        7
    );
}

#[test]
fn archived_exclusions_are_reversible_without_deleting_evidence() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    fs::create_dir(store.path().join("project")).unwrap();
    fs::write(
        store.path().join("project/eval.jsonl"),
        body("eval", "/tmp/eval-workspaces/run/vault"),
    )
    .unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_new, 1);
    let evidence = common::tree_bytes(&dest.path().join("captures"));
    fs::write(
        dest.path().join("config.toml"),
        "[pull]\nexclude-paths = ['**/*eval-workspaces/**/vault']\n",
    )
    .unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_found, 0);
    let archive = Archive::open(dest.path()).unwrap();
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 0);
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 0);
    assert_eq!(evidence, common::tree_bytes(&dest.path().join("captures")));
    fs::write(
        dest.path().join("config.toml"),
        "[pull]\nexclude-paths = []\n",
    )
    .unwrap();
    common::pull(store.path(), dest.path());
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 1);
}

#[test]
fn returning_to_retained_bodies_does_not_duplicate_revisions() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let dir = store.path().join("project");
    fs::create_dir(&dir).unwrap();
    for text in [
        "body A",
        "body B longer",
        "body A",
        "body B longer",
        "body A",
    ] {
        fs::write(dir.join("same.jsonl"), body(text, "/work")).unwrap();
        let stats = common::pull(store.path(), dest.path());
        assert_eq!(stats.sessions_failed, 0);
    }
    let revisions = dest.path().join("captures/testhost/claude/same.revisions");
    assert_eq!(
        fs::read_dir(revisions).unwrap().count(),
        4,
        "two distinct bodies and two sidecars"
    );
}

#[test]
fn fleet_exports_filter_and_elect_before_bookmarks() {
    use agent_sessions::fleet::{FleetConfig, FleetPull, pull_hosts};
    let remote = common::fake_remote::FakeRemote::reachable();
    let dest = tempfile::tempdir().unwrap();
    let store = remote.home().join(".claude/projects");
    for i in 0..19 {
        let dir = store.join(format!("project-{i:02}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("same.jsonl"),
            body(
                &format!("copy {i}"),
                if i == 0 {
                    "/tmp/eval-workspaces/run/vault"
                } else {
                    "/work"
                },
            ),
        )
        .unwrap();
    }
    let config_text = "[pull]\nexclude-paths = ['**/*eval-workspaces/**/vault']\n[[host]]\nname = 'remote'\nssh = 'fake'\n";
    fs::write(dest.path().join("config.toml"), config_text).unwrap();
    let config = FleetConfig::parse(config_text).unwrap();
    let archive = Archive::open(dest.path()).unwrap();
    let options = FleetPull {
        transport: &remote,
        local_binary: std::path::Path::new(env!("CARGO_BIN_EXE_agent-sessions")),
        full_source: None,
    };
    let first = pull_hosts(&archive, &config.hosts, &options).unwrap();
    assert!(!first.failed(), "{first:?}");
    assert_eq!(first.totals.sessions_new, 1);
    let before = common::tree_bytes(&archive.captures_dir());
    let second = pull_hosts(&archive, &config.hosts, &options).unwrap();
    assert!(!second.failed(), "{second:?}");
    assert_eq!(second.totals.sessions_revised, 0);
    assert_eq!(before, common::tree_bytes(&archive.captures_dir()));
    assert!(
        remote
            .calls()
            .iter()
            .any(|call| call.contains("--exclude-path '**/*eval-workspaces/**/vault'"))
    );
}

#[test]
fn native_project_directory_is_used_only_without_a_recorded_working_path() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let dir = store.path().join("native-eval-workspaces");
    fs::create_dir(&dir).unwrap();
    let mut no_cwd: serde_json::Value =
        serde_json::from_str(&body("no recorded path", "/unused")).unwrap();
    no_cwd.as_object_mut().unwrap().remove("cwd");
    fs::write(dir.join("fallback.jsonl"), format!("{no_cwd}\n")).unwrap();
    fs::write(dir.join("human.jsonl"), body("human work", "/work/human")).unwrap();
    fs::write(
        dest.path().join("config.toml"),
        "[pull]\nexclude-paths = ['*eval-workspaces*']\n",
    )
    .unwrap();
    let stats = common::pull(store.path(), dest.path());
    assert_eq!(stats.sessions_new, 1);
    assert_eq!(stats.sessions_failed, 0);
    let archive = Archive::open(dest.path()).unwrap();
    let sessions = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id, "human");
}

#[test]
fn newly_elected_copy_with_same_size_and_mtime_is_compared_by_bytes() {
    let store = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let later = store.path().join("z-project/same.jsonl");
    fs::create_dir_all(later.parent().unwrap()).unwrap();
    fs::write(&later, body("copy A", "/work")).unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_new, 1);
    let earlier = store.path().join("a-project/same.jsonl");
    fs::create_dir_all(earlier.parent().unwrap()).unwrap();
    fs::write(&earlier, body("copy B", "/work")).unwrap();
    let mtime = fs::metadata(&later).unwrap().modified().unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&earlier)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(common::pull(store.path(), dest.path()).sessions_revised, 1);
    assert_eq!(common::pull(store.path(), dest.path()).sessions_found, 0);
}

#[test]
fn repeated_rebuild_logs_stable_origin_ambiguity_at_debug() {
    let store = tempfile::tempdir().unwrap();
    let project = store.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("same.jsonl"), body("hello", "/work")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let archive = Archive::open(dest.path()).unwrap();
    let session = ClaudeAdapter
        .enumerate(store.path(), &EnumerateGate::default())
        .unwrap()
        .remove(0);
    for host in ["studio", "laptop"] {
        ClaudeAdapter
            .capture(&session, &archive.captures_dir(), host)
            .unwrap();
    }
    for repeat in [false, true] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(["rebuild", "--archive"])
            .arg(dest.path())
            .env("RUST_LOG", "debug")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if repeat {
            assert!(!log.contains("origin decision requires attention"), "{log}");
            assert!(log.contains("origin ambiguity unchanged"), "{log}");
        } else {
            assert!(log.contains("origin decision requires attention"), "{log}");
        }
    }
}
