//! Run fake executables in a separate test process so PATH never changes in
//! parallel tests or leaks into a real Store operation.
mod common;

use agent_sessions::repository_resolution::{GitResolver, Lookup, Resolver};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

fn fake_git(dir: &std::path::Path, body: &str) {
    let path = dir.join("git");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn hanging_git_is_killed_with_its_helper_budgeted_and_retryable() {
    if std::env::var_os("AS_TEST_DEADLINE_CHILD").is_some() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let path = format!(
        "{}:{}",
        temp.path().display(),
        std::env::var("PATH").unwrap()
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "deadline_scenario_child", "--nocapture"])
        .env("AS_TEST_DEADLINE_CHILD", temp.path())
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn deadline_scenario_child() {
    let Some(dir) = std::env::var_os("AS_TEST_DEADLINE_CHILD") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let pids = dir.join("pids");
    fake_git(
        &dir,
        "echo $$ >> \"$AS_TEST_DEADLINE_CHILD/pids\"\nsleep 60 &\necho $! >> \"$AS_TEST_DEADLINE_CHILD/pids\"\nwait",
    );
    let resolver = GitResolver::with_limits(Duration::from_millis(150), Duration::from_millis(360));
    let start = Instant::now();
    let paths = (0..10)
        .map(|i| format!("/recorded/path/{i}"))
        .collect::<Vec<_>>();
    let results = resolver.lookup_many(&paths).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(results.len(), 10);
    assert!(results.values().all(Result::is_err));
    let ids = fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(!ids.is_empty());
    assert!(ids.len() < 20, "budget must stop starting commands");
    for (index, id) in ids.iter().enumerate() {
        let output = Command::new("ps")
            .args(["-p", id, "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        assert!(
            state.trim().is_empty() || (index % 2 == 1 && state.trim_start().starts_with('Z')),
            "process {id} still running: {state}"
        );
    }
    assert!(
        results
            .values()
            .any(|v| v.as_ref().unwrap_err().contains("budget"))
    );
    // A helper retaining stdout after its parent exits cannot delay output
    // collection and must be killed even when the leader already succeeded.
    fake_git(
        &dir,
        "sleep 60 &\necho $! > \"$AS_TEST_DEADLINE_CHILD/orphan\"\ncase \"$3\" in rev-parse) echo .git;; config) echo https://github.com/example-org/infrastructure.git;; esac",
    );
    assert!(
        GitResolver::with_limits(Duration::from_millis(200), Duration::from_secs(1))
            .lookup("/helper")
            .is_ok()
    );
    let id = fs::read_to_string(dir.join("orphan")).unwrap();
    let state = Command::new("ps")
        .args(["-p", id.trim(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout);
    assert!(
        state.trim().is_empty() || state.trim_start().starts_with('Z'),
        "helper is still running: {state}"
    );

    // The second Git child (origin lookup) has the same hard deadline.
    fake_git(
        &dir,
        "if [ \"$3\" = rev-parse ]; then echo .git; else sleep 60; fi",
    );
    let start = Instant::now();
    assert!(
        GitResolver::with_limits(Duration::from_millis(100), Duration::from_secs(1))
            .lookup("/second-child")
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(3));

    // The real SSH transport uses the same group controller, independently
    // of whether the remote starts its resolver or sends a reply.
    fs::copy(dir.join("git"), dir.join("ssh")).unwrap();
    use agent_sessions::fleet::RemoteTransport;
    let start = Instant::now();
    let error = agent_sessions::fleet::SystemSsh
        .run_bounded("unused", "unused", Duration::from_millis(100))
        .unwrap_err();
    assert!(format!("{error:#}").contains("timed out"));
    assert!(start.elapsed() < Duration::from_secs(3));

    // The hidden batch command enforces its aggregate budget with real Git,
    // returns one retryable error per path, and exits successfully.
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args([
            "resolve-repository",
            "--budget-ms",
            "80",
            "--paths-json",
            "[\"/one\",\"/two\",\"/three\"]",
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
    assert_eq!(answers.len(), 3);
    assert!(answers.values().all(Result::is_err));
    assert!(
        answers
            .values()
            .any(|r| r.as_ref().unwrap_err().contains("budget"))
    );
    assert!(start.elapsed() < Duration::from_secs(3));

    facts_survive_changed_capture_and_retry(&dir);

    // An EPERM diagnostic accompanying generic discovery failure is not Path,
    // even if no marker is visible. Opaque failures with a marker also retry.
    let denied = dir.join("denied");
    fs::create_dir_all(denied.join(".git")).unwrap();
    fake_git(&dir, "echo 'fatal: not a git repository' >&2; exit 128");
    assert!(GitResolver.lookup(denied.to_str().unwrap()).is_err());
    fs::remove_dir(denied.join(".git")).unwrap();
    assert_eq!(
        GitResolver.lookup(denied.to_str().unwrap()).unwrap(),
        Lookup::Path
    );
    fake_git(
        &dir,
        "echo 'Operation not permitted: not a git repository' >&2; exit 128",
    );
    assert!(GitResolver.lookup(denied.to_str().unwrap()).is_err());

    // A recorded path is never stat'ed by the parent. Even this impossible
    // path reaches fake git; real git supplies the Missing observation.
    fake_git(
        &dir,
        "case \"$3\" in rev-parse) echo .git;; config) echo https://github.com/example-org/infrastructure.git;; esac",
    );
    let resolver = GitResolver::with_limits(Duration::from_secs(1), Duration::from_secs(2));
    assert!(matches!(
        resolver
            .lookup(&format!("/does-not-exist/{}", "x".repeat(1000)))
            .unwrap(),
        Lookup::Resolved(_)
    ));
}

fn facts_survive_changed_capture_and_retry(dir: &std::path::Path) {
    use agent_sessions::{
        adapters::CaptureMeta, archive::Archive, pull::pull_localhost_with_resolver,
    };
    use serde_json::json;
    let archive = Archive::open(&dir.join("archive")).unwrap();
    let mut roots = common::empty_roots(dir);
    roots.claude = dir.join("store");
    let folder = roots.claude.join("project");
    fs::create_dir_all(&folder).unwrap();
    let ids = [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ];
    let recorded = dir.join("recorded");
    fs::create_dir_all(recorded.join("existing")).unwrap();
    for (id, cwd) in ids
        .iter()
        .zip([recorded.join("existing"), recorded.join("missing")])
    {
        let record = json!({"type":"user","sessionId":id,"cwd":cwd,"gitBranch":"main","timestamp":"2026-03-05T09:12:00Z","message":{"role":"user","content":"Fix it"}});
        fs::write(folder.join(format!("{id}.jsonl")), format!("{record}\n")).unwrap();
    }
    let read_fact = |id: &str| {
        let paths = agent_sessions::adapters::capture_paths(
            &archive.captures_dir(),
            "studio",
            "claude",
            id,
        )
        .unwrap();
        let meta: CaptureMeta =
            serde_json::from_reader(fs::File::open(paths.meta).unwrap()).unwrap();
        meta.repository_resolution
    };
    fake_git(dir, "sleep 60");
    let stats = pull_localhost_with_resolver(
        &archive,
        &roots,
        "studio",
        &GitResolver::with_limits(Duration::from_millis(80), Duration::from_millis(120)),
    )
    .unwrap();
    assert!(!stats.repository_refresh_failed());
    assert!(
        ids.iter().all(|id| read_fact(id).is_none()),
        "timeout is never a negative answer"
    );

    fake_git(
        dir,
        "case \"$2\" in */recorded/missing) echo \"fatal: cannot change to '$2': No such file or directory\" >&2;; *) echo 'fatal: not a git repository' >&2;; esac; exit 128",
    );
    pull_localhost_with_resolver(&archive, &roots, "studio", &GitResolver).unwrap();
    let before = ids.map(|id| read_fact(id).unwrap());
    assert_eq!(before[0].lookup, Lookup::Path);
    assert_eq!(before[1].lookup, Lookup::Missing);
    let placements = agent_sessions::render::resolved_projects(&archive).unwrap();
    // A changed native revision must keep the prior observation and placement
    // when Git hangs, rather than settling a new Path or Missing answer.
    use std::io::Write;
    for id in ids {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(folder.join(format!("{id}.jsonl")))
                .unwrap(),
            "{{\"type\":\"progress\"}}"
        )
        .unwrap();
    }
    // Fleet Export defers lookup until all Sources are ingested, under one
    // shared Host budget. Ingest must not stamp an old negative answer with
    // this new revision (which would suppress the final lookup forever).
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args([
            "export",
            "--skip-repository-lookup",
            "--source",
            "claude",
            "--host",
            "studio",
            "--claude-root",
        ])
        .arg(&roots.claude)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    agent_sessions::bundle::ingest_bundle(
        output.stdout.as_slice(),
        &archive,
        &mut archive.db().unwrap(),
    )
    .unwrap();
    assert_eq!(ids.map(|id| read_fact(id).unwrap()), before);

    fake_git(dir, "sleep 60");
    let resolver = GitResolver::with_limits(Duration::from_millis(100), Duration::from_millis(150));
    let stats = pull_localhost_with_resolver(&archive, &roots, "studio", &resolver).unwrap();
    assert_eq!(stats.sessions_failed, 0);
    assert!(!stats.repository_refresh_failed());
    let report = &stats.repository_refresh[0];
    assert_eq!(report.retry_later, 2);
    assert!(
        report
            .retry_sample
            .as_deref()
            .unwrap()
            .contains("/recorded/")
    );
    assert_eq!(ids.map(|id| read_fact(id).unwrap()), before);
    assert_eq!(
        agent_sessions::render::resolved_projects(&archive).unwrap(),
        placements
    );
    let fleet = agent_sessions::fleet::FleetStats {
        totals: stats,
        hosts_pulled: 1,
        ..Default::default()
    };
    assert!(!fleet.failed());
    let summary = agent_sessions::fleet::summary_lines(&fleet, 1).join("\n");
    assert!(
        summary.contains("retry-later 2") && summary.contains("/recorded/"),
        "{summary}"
    );
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(ids.map(|id| read_fact(id).unwrap()), before);
    assert_eq!(
        agent_sessions::render::resolved_projects(&archive).unwrap(),
        placements
    );
    // No native change is necessary: the failed revision is still eligible.
    fake_git(
        dir,
        "case \"$3\" in rev-parse) echo .git;; config) echo https://github.com/example-org/infrastructure.git;; esac",
    );
    let stats = pull_localhost_with_resolver(&archive, &roots, "studio", &GitResolver).unwrap();
    assert!(stats.repository_refresh.is_empty());
    assert!(
        ids.iter()
            .all(|id| matches!(read_fact(id).unwrap().lookup, Lookup::Resolved(_)))
    );
}
