//! Slice 4 fleet tests: per-host failure isolation, the `export` binary's
//! stdout purity, and — when `ssh localhost` works in BatchMode — a live
//! pull over ssh into a temp Archive (self-deploy included). Everything
//! writes to temp directories only.

mod common;

use std::path::Path;
use std::process::Command;

use agent_sessions::archive::Archive;
use agent_sessions::bundle::ingest_bundle;
use agent_sessions::fleet::{
    DEFAULT_REMOTE_BIN, FleetConfig, FleetPull, HostConfig, SystemSsh, expected_version_line,
    pull_hosts, remote_export_command, remote_prepare_command, remote_probe_command, scp_args,
    ssh_args,
};

use common::fake_remote::FakeRemote;
use common::temp_store;

const BIN: &str = env!("CARGO_BIN_EXE_agent-sessions");

fn ssh_localhost_available() -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            "localhost",
            "true",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn fleet_config_rejects_invalid_input_and_preserves_source_roots() {
    let config = FleetConfig::parse(
        r#"
        [[host]]
        name = "central"

        [[host]]
        name = "laptop"
        ssh = "user@laptop.local"
        claude-root = "/stores/claude"
        codex-root = "/stores/codex"
        pi-root = "/stores/pi"
        remote-bin = "/opt/agent-sessions"
        "#,
    )
    .unwrap();
    assert_eq!(
        config.hosts[0],
        HostConfig::localhost("central".into(), None, None, None, None, None, None)
    );
    assert_eq!(
        config.hosts[1].codex_root.as_deref(),
        Some(Path::new("/stores/codex"))
    );
    assert_eq!(
        config.hosts[1].pi_root.as_deref(),
        Some(Path::new("/stores/pi"))
    );
    assert!(FleetConfig::parse("[[host]]\nname = 'a'\n[[host]]\nname = 'a'\n").is_err());
    assert!(FleetConfig::parse("[[host]]\nname = 'a'\nsshh = 'typo'\n").is_err());
}

#[test]
fn ssh_commands_are_quoted_and_source_scoped() {
    assert_eq!(
        remote_export_command(
            DEFAULT_REMOTE_BIN,
            "laptop",
            "claude",
            Some(1755600000000),
            None,
        ),
        "'.agent-sessions/bin/agent-sessions' export --host 'laptop' --source 'claude' --since 1755600000000"
    );
    assert_eq!(
        remote_export_command(
            "/opt/as",
            "my host",
            "pi",
            None,
            Some(Path::new("/store's root")),
        ),
        r#"'/opt/as' export --host 'my host' --source 'pi' --pi-root '/store'\''s root'"#
    );
    assert_eq!(
        ssh_args("user@laptop.local", "'/opt/as' --version"),
        vec![
            "-o",
            "BatchMode=yes",
            "user@laptop.local",
            "'/opt/as' --version"
        ]
    );
    assert_eq!(
        scp_args(
            Path::new("/usr/local/bin/agent-sessions"),
            "laptop",
            DEFAULT_REMOTE_BIN,
        ),
        vec![
            "-o",
            "BatchMode=yes",
            "/usr/local/bin/agent-sessions",
            "laptop:.agent-sessions/bin/agent-sessions"
        ]
    );
    assert_eq!(
        remote_probe_command(DEFAULT_REMOTE_BIN),
        "uname -sm; '.agent-sessions/bin/agent-sessions' --version"
    );
    assert_eq!(
        remote_prepare_command(DEFAULT_REMOTE_BIN).as_deref(),
        Some("mkdir -p '.agent-sessions/bin'")
    );
    assert_eq!(
        expected_version_line(),
        format!(
            "agent-sessions {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("ASESSIONS_BUILD_HASH")
        )
    );
}

#[test]
fn one_failing_host_does_not_abort_the_pull() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();

    let config = FleetConfig::parse(&format!(
        r#"
        [[host]]
        name = "broken"
        ssh = "nobody@broken"

        [[host]]
        name = "goodhost"
        claude-root = {claude_root:?}
        codex-root = {missing_root:?}
        pi-root = {missing_root:?}
        cursor-root = {missing_root:?}
        t3-root = {missing_root:?}
        "#,
        claude_root = store.path().to_string_lossy(),
        missing_root = store.path().join("missing-store").to_string_lossy(),
    ))
    .unwrap();

    let remote = FakeRemote::failing_connection("nobody@broken: Permission denied (publickey).");
    let fleet = pull_hosts(
        &archive,
        &config.hosts,
        &FleetPull {
            transport: &remote,
            local_binary: Path::new(BIN),
            full_source: None,
        },
    )
    .unwrap();
    assert_eq!(fleet.hosts_failed, 1);
    assert_eq!(fleet.failed_hosts, vec!["broken".to_string()]);
    assert_eq!(fleet.hosts_pulled, 1, "goodhost still pulled");
    assert_eq!(fleet.totals.sessions_found, 4);
    assert_eq!(fleet.totals.conversations_extracted, 4);
    let failed_rows = fleet
        .totals
        .sources
        .iter()
        .filter(|row| row.host == "broken")
        .collect::<Vec<_>>();
    assert_eq!(
        failed_rows.len(),
        agent_sessions::adapters::AVAILABLE_SOURCES.len(),
        "one error row per available Source"
    );
    assert!(failed_rows.iter().all(|row| row.sessions_failed == 1));
    assert!(failed_rows.iter().all(|row| row.error.is_some()));

    // The failed host left no Bookmark; the good one advanced.
    let db = archive.db().unwrap();
    assert!(db.bookmark("broken", "claude").unwrap().is_none());
    assert!(db.bookmark("goodhost", "claude").unwrap().is_some());
}

#[test]
fn export_binary_stdout_is_exactly_a_bundle() {
    let store = temp_store();
    let out = Command::new(BIN)
        .args([
            "export",
            "--host",
            "binhost",
            "--source",
            "claude",
            "--claude-root",
            &store.path().to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout must be a clean Bundle (any stray print would corrupt the tar
    // stream): ingest it and check the content arrived.
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    let ingested = ingest_bundle(&out.stdout[..], &archive, &mut db).unwrap();
    assert_eq!(ingested.manifest.host, "binhost");
    assert_eq!(ingested.stats.units_in_bundle, 4);
    assert_eq!(ingested.stats.conversations_extracted, 4);
}

#[test]
fn partial_export_is_valid_bundle_and_exits_nonzero() {
    let store = temp_store();
    let meta = store
        .path()
        .join("-workspace-Projects-example/33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef.meta.json");
    std::fs::write(meta, "not json").unwrap();
    let out = Command::new(BIN)
        .args([
            "export",
            "--host",
            "binhost",
            "--source",
            "claude",
            "--claude-root",
            &store.path().to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "partial export is an honest failure");

    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    let ingested = ingest_bundle(&out.stdout[..], &archive, &mut db).unwrap();
    assert_eq!(ingested.manifest.units_failed, 1);
    assert_eq!(ingested.stats.units_in_bundle, 3);
    assert_eq!(
        ingested.manifest.unit_count + ingested.manifest.units_failed,
        4,
        "found includes successful and failed exports"
    );
}

#[test]
fn live_pull_over_ssh_localhost() {
    if !ssh_localhost_available() {
        eprintln!("skipping: ssh to localhost is not available in BatchMode");
        return;
    }

    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();

    // remote-bin points into a temp dir, so self-deploy (mkdir + scp +
    // chmod + re-handshake) is exercised without touching the real home.
    let deploy_dir = tempfile::tempdir().unwrap();
    let remote_bin = deploy_dir.path().join("bin").join("agent-sessions");
    let config = FleetConfig::parse(&format!(
        r#"
        [[host]]
        name = "sshhost"
        ssh = "localhost"
        claude-root = {claude_root:?}
        codex-root = {missing_root:?}
        pi-root = {missing_root:?}
        cursor-root = {missing_root:?}
        t3-root = {missing_root:?}
        remote-bin = {remote_bin:?}
        "#,
        claude_root = store.path().to_string_lossy(),
        missing_root = store.path().join("missing-store").to_string_lossy(),
        remote_bin = remote_bin.to_string_lossy(),
    ))
    .unwrap();

    // First pull: deploys the binary, exports everything, ingests.
    let pull = FleetPull {
        transport: &SystemSsh,
        local_binary: Path::new(BIN),
        full_source: None,
    };
    let fleet = pull_hosts(&archive, &config.hosts, &pull).unwrap();
    assert_eq!(fleet.hosts_failed, 0, "failed: {:?}", fleet.failed_hosts);
    assert_eq!(fleet.totals.sessions_found, 4);
    assert_eq!(fleet.totals.sessions_captured, 4);
    assert_eq!(fleet.totals.conversations_extracted, 4);
    assert!(remote_bin.is_file(), "binary was self-deployed");

    let db = archive.db().unwrap();
    assert_eq!(db.session_count().unwrap(), 4);
    assert!(db.bookmark("sshhost", "claude").unwrap().is_some());
    assert!(
        archive_dir
            .path()
            .join("captures")
            .join("sshhost")
            .join("claude")
            .is_dir()
    );

    // Second pull: the central Bookmark drives --since; nothing to export.
    let fleet = pull_hosts(&archive, &config.hosts, &pull).unwrap();
    assert_eq!(fleet.hosts_failed, 0);
    assert_eq!(fleet.totals.sessions_found, 0, "idempotent over ssh");
}

#[test]
fn remote_busy_bundle_keeps_bookmark_without_failing_fleet() {
    use agent_sessions::{
        adapters::{Adapter, EnumerateGate, claude::ClaudeAdapter},
        bundle::{BUNDLE_SCHEMA_VERSION, BundleManifest, write_bundle},
    };
    let remote = FakeRemote::reachable();
    let store = temp_store();
    let refs = ClaudeAdapter
        .enumerate(store.path(), &EnumerateGate::bookmark_only(None))
        .unwrap();
    let spool = remote.home().join("spool");
    let capture = ClaudeAdapter.capture(&refs[0], &spool, "laptop").unwrap();
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        host: "laptop".into(),
        sources: vec!["claude".into()],
        since_ms: Some(1),
        unit_count: 1,
        units_failed: 0,
        units_retry_later: 1,
        exported_at_ms: 0,
        retention: Default::default(),
    };
    let bundle = remote.home().join("busy.tar");
    write_bundle(
        std::fs::File::create(&bundle).unwrap(),
        &manifest,
        &[capture],
        &spool,
    )
    .unwrap();
    remote.install("bin/asessions", &format!("#!/bin/sh\ncase \"$*\" in\n  *'--source claude'*) cat '{}' ;;\n  *) exec '{}' \"$@\" ;;\nesac\n", bundle.display(), BIN));
    let config = FleetConfig::parse(
        "[[host]]\nname = 'laptop'\nssh = 'laptop'\nremote-bin = 'bin/asessions'\n",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    archive
        .db()
        .unwrap()
        .advance_bookmark("laptop", "claude", 1)
        .unwrap();
    let fleet = pull_hosts(
        &archive,
        &config.hosts,
        &FleetPull {
            transport: &remote,
            local_binary: Path::new(BIN),
            full_source: None,
        },
    )
    .unwrap();
    assert!(!fleet.failed(), "{fleet:?}");
    assert_eq!(fleet.totals.sessions_retry_later, 1);
    assert_eq!(fleet.totals.sessions_failed, 0);
    assert_eq!(fleet.totals.sessions_new, 1);
    assert_eq!(
        archive.db().unwrap().bookmark("laptop", "claude").unwrap(),
        Some(1)
    );
    // Historical Bundles default to no busy units.
    let mut historical = serde_json::to_value(&manifest).unwrap();
    historical
        .as_object_mut()
        .unwrap()
        .remove("unitsRetryLater");
    assert_eq!(
        serde_json::from_value::<BundleManifest>(historical)
            .unwrap()
            .units_retry_later,
        0
    );
}
