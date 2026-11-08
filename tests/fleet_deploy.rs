//! Platform-safe self-deploy: the central binary replaces a remote binary
//! only when the remote runs the platform it was built for. Uses a fake ssh
//! transport only.

mod common;

use std::path::Path;

use agent_sessions::archive::Archive;
use agent_sessions::fleet::{
    DEFAULT_REMOTE_BIN, FleetConfig, FleetPull, FleetStats, expected_version_line, pull_hosts,
};

use common::fake_remote::{FakeRemote, foreign_platform};
use common::temp_store;

const BIN: &str = env!("CARGO_BIN_EXE_agent-sessions");
const OLD_BINARY: &str = "#!/bin/sh\necho 'agent-sessions 0.4.8 (0123456789ab)'\n";

fn pull(remote: &FakeRemote) -> (tempfile::TempDir, Archive, FleetStats) {
    let store = temp_store();
    let missing = store.path().join("missing-store");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "[[host]]\nname = \"dev-linux\"\nssh = \"dev-linux\"\nclaude-root = {:?}\ncodex-root = {:?}\npi-root = {:?}\ncursor-root = {:?}\nchatgpt-root = {:?}\n",
            store.path().to_string_lossy(),
            missing.to_string_lossy(),
            missing.to_string_lossy(),
            missing.to_string_lossy(),
            missing.to_string_lossy(),
        ),
    )
    .unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let config = FleetConfig::load(archive.root()).unwrap().unwrap();
    let fleet = pull_hosts(
        &archive,
        &config.hosts,
        &FleetPull {
            transport: remote,
            local_binary: Path::new(BIN),
            full_source: None,
        },
    )
    .unwrap();
    (dir, archive, fleet)
}

fn source_errors(fleet: &FleetStats) -> Vec<String> {
    fleet
        .totals
        .sources
        .iter()
        .filter_map(|row| row.error.clone())
        .collect()
}

#[test]
fn cross_platform_remote_binary_is_never_replaced() {
    let (uname, target) = foreign_platform();
    let remote = FakeRemote::reachable().with_uname(uname);
    let installed = remote.install(DEFAULT_REMOTE_BIN, OLD_BINARY);

    let (_dir, _archive, fleet) = pull(&remote);

    assert_eq!(fleet.failed_hosts, vec!["dev-linux".to_string()]);
    assert!(fleet.unreachable_hosts.is_empty());
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        OLD_BINARY,
        "the remote's own binary is untouched"
    );
    assert_eq!(
        remote.calls().len(),
        1,
        "nothing after the probe: {:?}",
        remote.calls()
    );
    let errors = source_errors(&fleet);
    let error = errors.first().expect("failed host has error rows");
    assert!(error.contains(target), "{error}");
    assert!(error.contains(DEFAULT_REMOTE_BIN), "{error}");
    assert!(
        error.contains("cannot self-deploy across platforms"),
        "{error}"
    );
}

#[test]
fn missing_binary_on_foreign_platform_is_not_deployed() {
    let (uname, target) = foreign_platform();
    let remote = FakeRemote::reachable().with_uname(uname);

    let (_dir, _archive, fleet) = pull(&remote);

    assert_eq!(fleet.failed_hosts, vec!["dev-linux".to_string()]);
    assert!(!remote.home().join(DEFAULT_REMOTE_BIN).exists());
    assert!(source_errors(&fleet)[0].contains(target));
}

#[test]
fn matching_version_on_foreign_platform_needs_no_deploy() {
    let (uname, _) = foreign_platform();
    let remote = FakeRemote::reachable().with_uname(uname);
    // A same-release binary for the remote's platform, already installed.
    let script = format!("#!/bin/sh\nexec '{BIN}' \"$@\"\n");
    remote.install(DEFAULT_REMOTE_BIN, &script);
    assert!(
        std::process::Command::new(remote.home().join(DEFAULT_REMOTE_BIN))
            .arg("--version")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim() == expected_version_line())
            .unwrap()
    );

    let (_dir, _archive, fleet) = pull(&remote);

    assert!(!fleet.failed(), "{:?}", source_errors(&fleet));
    assert_eq!(fleet.totals.sessions_found, 4);
    assert!(
        !remote.calls().iter().any(|call| call.starts_with("copy")),
        "{:?}",
        remote.calls()
    );
}

#[test]
fn same_platform_remote_self_deploys_on_version_mismatch() {
    let remote = FakeRemote::reachable();
    let installed = remote.install(DEFAULT_REMOTE_BIN, OLD_BINARY);

    let (_dir, archive, fleet) = pull(&remote);

    assert!(!fleet.failed(), "{:?}", source_errors(&fleet));
    assert_eq!(fleet.totals.sessions_found, 4);
    assert_eq!(
        std::fs::read(&installed).unwrap(),
        std::fs::read(BIN).unwrap(),
        "the central binary was deployed"
    );
    assert!(
        archive
            .db()
            .unwrap()
            .bookmark("dev-linux", "claude")
            .unwrap()
            .is_some()
    );
}
