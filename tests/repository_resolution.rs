use agent_sessions::repository_resolution::{GitResolver, Lookup, Resolver};
use std::process::Command;

fn git(path: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn real_worktree_and_deleted_directory_have_distinct_outcomes() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init"]);
    git(
        tmp.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-org/infrastructure.git",
        ],
    );
    let worktree = tmp.path().join("worktree");
    git(
        tmp.path(),
        &["worktree", "add", "-b", "test", worktree.to_str().unwrap()],
    );
    assert_eq!(
        GitResolver.lookup(worktree.to_str().unwrap()).unwrap(),
        Lookup::Resolved("https://github.com/example-org/infrastructure.git".into())
    );
    git(
        tmp.path(),
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    assert_eq!(
        GitResolver.lookup(worktree.to_str().unwrap()).unwrap(),
        Lookup::Missing
    );
    git(tmp.path(), &["remote", "remove", "origin"]);
    assert_eq!(
        GitResolver.lookup(tmp.path().to_str().unwrap()).unwrap(),
        Lookup::Path
    );
}

mod common;
use agent_sessions::{
    archive::{Archive, rebuild},
    pull::pull_localhost,
    render::resolved_projects,
};
use serde_json::json;
use std::fs;

fn session(store: &std::path::Path, path: &std::path::Path, branch: bool, id: &str) {
    let folder = store.join("project");
    fs::create_dir_all(&folder).unwrap();
    let record = json!({"type":"user","sessionId":id,"cwd":path,"gitBranch":branch.then_some("main"),"timestamp":"2026-03-05T09:12:00Z","message":{"role":"user","content":"Fix the bug"}});
    fs::write(folder.join(format!("{id}.jsonl")), format!("{record}\n")).unwrap();
}

#[test]
fn capture_backfill_sticks_and_rebuild_preserves_repository_and_native_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    let worktree = tmp.path().join("branch-folder");
    git(
        &repo,
        &["worktree", "add", "-b", "test", worktree.to_str().unwrap()],
    );
    let store = tmp.path().join("store");
    session(
        &store,
        &worktree,
        true,
        "11111111-1111-4111-8111-111111111111",
    );
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store.clone();
    pull_localhost(&archive, &roots, "test").unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("branch-folder")
    );
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-org/infrastructure.git",
        ],
    );
    // A changed Capture revision permits a fresh negative-path lookup.
    use std::io::Write;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(store.join("project/11111111-1111-4111-8111-111111111111.jsonl"))
            .unwrap(),
        "{{\"type\":\"system\"}}"
    )
    .unwrap();
    pull_localhost(&archive, &roots, "test").unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("infrastructure")
    );
    git(&repo, &["worktree", "remove", worktree.to_str().unwrap()]);
    session(
        &store,
        &worktree,
        true,
        "22222222-2222-4222-8222-222222222222",
    );
    pull_localhost(&archive, &roots, "test").unwrap();
    rebuild(&archive).unwrap();
    assert!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .all(|v| v.as_deref() == Some("infrastructure"))
    );
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .iter()
            .all(|s| s
                .transcript_facts
                .git
                .as_ref()
                .unwrap()
                .repository_url
                .is_none())
    );
}

struct Failed;
impl Resolver for Failed {
    fn lookup(&self, _: &str) -> anyhow::Result<Lookup> {
        anyhow::bail!("offline")
    }
}

#[test]
fn missing_branch_is_unresolved_but_lookup_failure_is_retryable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    session(
        &store,
        &tmp.path().join("gone"),
        true,
        "11111111-1111-4111-8111-111111111111",
    );
    session(
        &store,
        &tmp.path().join("gone2"),
        false,
        "22222222-2222-4222-8222-222222222222",
    );
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "test").unwrap();
    let rows = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert!(rows[0].repository_resolution.as_ref().unwrap().unresolved);
    assert!(!rows[1].repository_resolution.as_ref().unwrap().unresolved);
    let before = fs::read(
        archive
            .captures_dir()
            .join("test/claude/11111111-1111-4111-8111-111111111111.meta.json"),
    )
    .unwrap();
    agent_sessions::repository_resolution::refresh(&archive, Some("test"), &Failed).unwrap();
    assert_eq!(
        before,
        fs::read(
            archive
                .captures_dir()
                .join("test/claude/11111111-1111-4111-8111-111111111111.meta.json")
        )
        .unwrap()
    );
    let projects = resolved_projects(&archive).unwrap();
    assert!(projects[&rows[0].id].is_none());
    assert_eq!(projects[&rows[1].id].as_deref(), Some("gone2"));
}

#[test]
fn exact_name_table_and_manual_assignment_win_for_unresolved_session() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let path = tmp.path().join("gone");
    session(&store, &path, true, "11111111-1111-4111-8111-111111111111");
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views.project-names]\n{} = \"Named project\"\n",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("Named project")
    );
    agent_sessions::assignments::assign(
        &archive,
        "claude",
        "11111111-1111-4111-8111-111111111111",
        "Manual project",
        None,
    )
    .unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("Manual project")
    );
}

#[test]
fn ssh_pull_backfills_expired_native_sessions_after_handshake() {
    use agent_sessions::fleet::{FleetPull, HostConfig, pull_hosts};
    let tmp = tempfile::tempdir().unwrap();
    let remote = common::fake_remote::FakeRemote::reachable();
    let repo = tmp.path().join("repo with 'quotes'");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init"]);
    let store = tmp.path().join("store");
    session(&store, &repo, true, "11111111-1111-4111-8111-111111111111");
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store.clone();
    session(
        &store,
        &tmp.path().join("missing-second"),
        true,
        "22222222-2222-4222-8222-222222222222",
    );
    pull_localhost(&archive, &roots, "remote").unwrap();
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example-org/infrastructure.git",
        ],
    );
    let sql = rusqlite::Connection::open(archive.db_path()).unwrap();
    sql.execute("DELETE FROM repository_resolutions", [])
        .unwrap();
    drop(sql);
    fs::remove_dir_all(store).unwrap();
    let mut host = HostConfig::localhost("remote".into(), None, None, None, None, None, None);
    host.ssh = Some("fake".into());
    host.remote_bin = Some(env!("CARGO_BIN_EXE_agent-sessions").into());
    let stats = pull_hosts(
        &archive,
        &[host.clone()],
        &FleetPull {
            transport: &remote,
            local_binary: std::path::Path::new(env!("CARGO_BIN_EXE_agent-sessions")),
            full_source: None,
        },
    )
    .unwrap();
    assert_eq!(stats.hosts_failed, 0);
    assert_eq!(
        remote
            .calls()
            .iter()
            .filter(|c| c.contains(" resolve-repository "))
            .count(),
        1
    );
    pull_hosts(
        &archive,
        &[host],
        &FleetPull {
            transport: &remote,
            local_binary: std::path::Path::new(env!("CARGO_BIN_EXE_agent-sessions")),
            full_source: None,
        },
    )
    .unwrap();
    assert_eq!(
        remote
            .calls()
            .iter()
            .filter(|c| c.contains(" resolve-repository "))
            .count(),
        1,
        "settled Missing and Path answers must not cause more SSH lookups"
    );
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("infrastructure")
    );
    rebuild(&archive).unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("infrastructure")
    );
}

#[test]
fn claude_git_fields_can_arrive_on_independent_records() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let id = "11111111-1111-4111-8111-111111111111";
    session(&store, tmp.path(), true, id);
    writeln!(fs::OpenOptions::new().append(true).open(store.join(format!("project/{id}.jsonl"))).unwrap(), "{}",json!({"type":"system","gitRepositoryUrl":"https://github.com/example-org/infrastructure.git","gitCommit":"abcdef"})).unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    let rows = archive.db().unwrap().sessions_with_dialog().unwrap();
    let git = rows[0].transcript_facts.git.as_ref().unwrap();
    assert_eq!(git.branch.as_deref(), Some("main"));
    assert_eq!(git.commit.as_deref(), Some("abcdef"));
    assert_eq!(
        git.repository_url.as_deref(),
        Some("https://github.com/example-org/infrastructure.git")
    );
}

#[test]
fn bundle_carries_resolution_after_worktree_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example-org/infrastructure.git",
        ],
    );
    let store = tmp.path().join("store");
    session(&store, &repo, true, "11111111-1111-4111-8111-111111111111");
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    let mut bytes = Vec::new();
    agent_sessions::bundle::export(&mut bytes, &roots, "host", Some("claude"), None).unwrap();
    fs::remove_dir_all(repo).unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    agent_sessions::bundle::ingest_bundle(bytes.as_slice(), &archive, &mut archive.db().unwrap())
        .unwrap();
    rebuild(&archive).unwrap();
    assert_eq!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_deref(),
        Some("infrastructure")
    );
}

#[test]
fn failed_first_lookup_creates_no_final_fact_and_retries_normally() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let id = "11111111-1111-4111-8111-111111111111";
    session(&store, &tmp.path().join("gone"), true, id);
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    let meta = archive
        .captures_dir()
        .join(format!("host/claude/{id}.meta.json"));
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&meta).unwrap()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("repositoryResolution");
    fs::write(&meta, serde_json::to_vec(&value).unwrap()).unwrap();
    rebuild(&archive).unwrap();
    agent_sessions::repository_resolution::refresh(&archive, Some("host"), &Failed).unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .repository_resolution
            .is_none()
    );
    pull_localhost(&archive, &roots, "host").unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .repository_resolution
            .as_ref()
            .unwrap()
            .unresolved
    );
}

#[derive(Default)]
struct Counting {
    lookups: std::cell::Cell<usize>,
    extractions: std::cell::Cell<usize>,
}
impl Resolver for Counting {
    fn lookup(&self, path: &str) -> anyhow::Result<Lookup> {
        self.lookups.set(self.lookups.get() + 1);
        GitResolver.lookup(path)
    }
    fn extracted(&self) {
        self.extractions.set(self.extractions.get() + 1);
    }
}

#[test]
fn unchanged_pull_extracts_and_looks_up_nothing_including_no_fact_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    session(
        &store,
        tmp.path(),
        false,
        "11111111-1111-4111-8111-111111111111",
    );
    session(
        &store,
        &tmp.path().join("gone"),
        true,
        "22222222-2222-4222-8222-222222222222",
    );
    session(
        &store,
        tmp.path(),
        false,
        "33333333-3333-4333-8333-333333333333",
    );
    use std::io::Write;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(store.join("project/33333333-3333-4333-8333-333333333333.jsonl"))
            .unwrap(),
        "{}",
        json!({"type":"system","gitRepositoryUrl":"https://github.com/owner/recorded"})
    )
    .unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    // Native Project and no-path Sessions are also indexed but need no fact.
    let mut conversation: agent_sessions::model::Conversation =
        serde_json::from_slice::<Vec<agent_sessions::model::Conversation>>(
            &fs::read("tests/fixtures/digest-upgrade/dialog.json").unwrap(),
        )
        .unwrap()
        .remove(0);
    conversation.provenance.native_id = "no-path".into();
    conversation.provenance.host = "host".into();
    conversation.project_path = None;
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    conversation.provenance.native_id = "native-project".into();
    conversation.project_path = Some("/native/project".into());
    conversation.native_project = Some(agent_sessions::model::NativeProject {
        source: "test".into(),
        native_id: "project".into(),
        name: None,
    });
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    let counter = Counting::default();
    let result =
        agent_sessions::pull::pull_localhost_with_resolver(&archive, &roots, "host", &counter)
            .unwrap();
    assert_eq!(result.conversations_extracted, 0);
    assert_eq!(counter.extractions.get(), 0);
    assert_eq!(counter.lookups.get(), 0);
    // Another Host must not inspect any of these paths either.
    agent_sessions::repository_resolution::refresh(&archive, Some("other-host"), &counter).unwrap();
    assert_eq!((counter.extractions.get(), counter.lookups.get()), (0, 0));
}

#[test]
fn batch_command_isolates_one_path_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    fs::write(&file, "not a directory").unwrap();
    let paths = vec![
        file.to_str().unwrap(),
        tmp.path().to_str().unwrap(),
        "/nonexistent-for-repository-test",
    ];
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args([
            "resolve-repository",
            "--paths-json",
            &serde_json::to_string(&paths).unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answers: std::collections::BTreeMap<String, Result<Lookup, String>> =
        serde_json::from_slice(&output.stdout).unwrap();
    assert!(answers[paths[0]].is_err());
    assert_eq!(answers[paths[1]], Ok(Lookup::Path));
    assert_eq!(answers[paths[2]], Ok(Lookup::Missing));
}

#[test]
fn corrupt_selected_sidecar_is_counted_and_other_repository_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    for id in [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ] {
        session(&store, tmp.path(), false, id);
    }
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    rusqlite::Connection::open(archive.db_path())
        .unwrap()
        .execute("DELETE FROM repository_resolutions", [])
        .unwrap();
    fs::write(
        archive
            .captures_dir()
            .join("host/claude/11111111-1111-4111-8111-111111111111.meta.json"),
        "invalid JSON",
    )
    .unwrap();
    let stats =
        agent_sessions::repository_resolution::refresh(&archive, Some("host"), &GitResolver)
            .unwrap();
    assert_eq!(stats.units_failed, 1);
    let rows = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert!(rows[1].repository_resolution.is_some());
}

#[test]
fn unresolved_summary_counts_only_sessions_actually_rendered_loose() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let id = "11111111-1111-4111-8111-111111111111";
    let path = tmp.path().join("gone");
    session(&store, &path, true, id);
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    assert_eq!(
        agent_sessions::render::render_all(&archive)
            .unwrap()
            .unresolved_repositories,
        1
    );
    fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views.project-names]\n{}=\"Named\"\n",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        agent_sessions::render::render_all(&archive)
            .unwrap()
            .unresolved_repositories,
        0
    );
    fs::remove_file(archive.root().join("config.toml")).unwrap();
    agent_sessions::assignments::assign(&archive, "claude", id, "Manual", None).unwrap();
    assert_eq!(
        agent_sessions::render::render_all(&archive)
            .unwrap()
            .unresolved_repositories,
        0
    );
}

#[test]
fn per_unit_sidecar_open_path_failure_is_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let id = "11111111-1111-4111-8111-111111111111";
    session(&store, tmp.path(), false, id);
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    pull_localhost(&archive, &roots, "host").unwrap();
    rusqlite::Connection::open(archive.db_path())
        .unwrap()
        .execute("DELETE FROM repository_resolutions", [])
        .unwrap();
    let meta = archive
        .captures_dir()
        .join(format!("host/claude/{id}.meta.json"));
    fs::remove_file(&meta).unwrap();
    fs::create_dir(&meta).unwrap();
    assert_eq!(
        agent_sessions::repository_resolution::refresh(&archive, Some("host"), &GitResolver)
            .unwrap()
            .units_failed,
        1
    );
}

#[test]
fn claude_subagent_candidate_preserves_nested_capture_path() {
    let store = common::temp_store();
    let tmp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    common::pull(store.path(), archive.root());
    let id = "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef";
    let paths = agent_sessions::adapters::capture_paths(
        &archive.captures_dir(),
        common::HOST,
        "claude",
        id,
    )
    .unwrap();
    let meta: agent_sessions::adapters::CaptureMeta =
        serde_json::from_slice(&fs::read(&paths.meta).unwrap()).unwrap();
    assert!(meta.repository_resolution.is_some());
    let session = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == id)
        .unwrap();
    assert!(session.repository_resolution.is_some());
    assert!(paths.meta.ends_with(format!("{id}.meta.json")));
    rebuild(&archive).unwrap();
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .into_iter()
            .find(|s| s.native_id == id)
            .unwrap()
            .repository_resolution
            .is_some()
    );
}

#[test]
fn real_pull_keeps_source_stats_when_repository_database_write_fails() {
    struct FailIndex {
        database: std::path::PathBuf,
    }
    impl Resolver for FailIndex {
        fn lookup(&self, _: &str) -> anyhow::Result<Lookup> {
            rusqlite::Connection::open(&self.database)?
                .execute("DROP TABLE repository_resolutions", [])?;
            Ok(Lookup::Resolved(
                "https://github.com/example-org/infrastructure.git".into(),
            ))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    session(
        &store,
        tmp.path(),
        true,
        "11111111-1111-4111-8111-111111111111",
    );
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    let stats = agent_sessions::pull::pull_localhost_with_resolver(
        &archive,
        &roots,
        "studio",
        &FailIndex {
            database: archive.db_path(),
        },
    )
    .unwrap();
    assert_eq!(stats.sessions_found, 1);
    assert_eq!(stats.sessions_new, 1);
    assert_eq!(stats.conversations_extracted, 1);
    assert_eq!(stats.sessions_failed, 0);
    let claude = stats.sources.iter().find(|s| s.source == "claude").unwrap();
    assert_eq!(claude.sessions_found, 1);
    assert_eq!(claude.sessions_new, 1);
    assert!(claude.error.is_none());
    assert_eq!(stats.repository_refresh.len(), 1);
    assert!(
        stats.repository_refresh[0]
            .error
            .as_ref()
            .unwrap()
            .contains("repository_resolutions")
    );
    let fleet = agent_sessions::fleet::FleetStats {
        hosts_pulled: 1,
        totals: stats,
        ..Default::default()
    };
    assert!(fleet.failed());
    let report = agent_sessions::fleet::summary_lines(&fleet, 1);
    assert!(
        report
            .iter()
            .any(|line| line.starts_with("studio/claude") && line.contains("found     1")),
        "{report:?}"
    );
    assert!(
        report
            .iter()
            .any(|line| line.contains("studio/repository-resolution") && line.contains("error:"))
    );
    assert!(report.last().unwrap().contains("1/1 hosts"));
}

#[test]
fn real_pull_isolates_a_sidecar_open_failure_after_ingest() {
    struct RemoveOne {
        meta: std::path::PathBuf,
    }
    impl Resolver for RemoveOne {
        fn lookup(&self, _: &str) -> anyhow::Result<Lookup> {
            if self.meta.exists() {
                fs::remove_file(&self.meta)?;
            }
            Ok(Lookup::Resolved(
                "https://github.com/example-org/infrastructure.git".into(),
            ))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    for id in [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ] {
        session(&store, tmp.path(), false, id);
    }
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = store;
    let meta = agent_sessions::adapters::capture_paths(
        &archive.captures_dir(),
        "studio",
        "claude",
        "11111111-1111-4111-8111-111111111111",
    )
    .unwrap()
    .meta;
    let stats = agent_sessions::pull::pull_localhost_with_resolver(
        &archive,
        &roots,
        "studio",
        &RemoveOne { meta },
    )
    .unwrap();
    assert_eq!(stats.sessions_found, 2);
    assert_eq!(stats.sessions_new, 2);
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(stats.repository_refresh[0].units_failed, 1);
    assert!(stats.repository_refresh[0].error.is_none());
    let rows = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert!(
        rows.iter()
            .find(|s| s.native_id.starts_with("2222"))
            .unwrap()
            .repository_resolution
            .is_some()
    );
}

#[test]
fn inaccessible_git_directory_is_retryable_not_a_path_answer() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init"]);
    let git_dir = tmp.path().join(".git");
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read_dir(&git_dir).is_ok() {
        fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!(
            "skipping permission regression: chmod 000 did not prevent reading .git (privileged user or ineffective permissions)"
        );
        return;
    }
    let result = GitResolver.lookup(tmp.path().to_str().unwrap());
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_err(),
        "denied Git discovery must not settle Path: {result:?}"
    );
    fs::remove_dir_all(git_dir).unwrap();
    assert_eq!(
        GitResolver.lookup(tmp.path().to_str().unwrap()).unwrap(),
        Lookup::Path
    );
}
