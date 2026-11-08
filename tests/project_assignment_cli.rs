mod common;
use agent_sessions::{archive::Archive, assignments};
const ID: &str = "11111111-1111-4111-8111-111111111111";
fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Archive) {
    let store = common::temp_store();
    let dir = tempfile::tempdir().unwrap();
    common::pull(store.path(), dir.path());
    let archive = Archive::open_existing(dir.path()).unwrap();
    (store, dir, archive)
}
#[test]
fn cli_assign_rebuild_unassign_cycle() {
    let (_store, _dir, archive) = fixture();
    for arguments in [
        vec![
            "project",
            "assign",
            &format!("claude:{ID}"),
            "--project",
            "CLI Project",
        ],
        vec!["rebuild"],
        vec!["project", "unassign", &format!("claude:{ID}")],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(arguments)
            .arg("--archive")
            .arg(archive.root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(assignments::load(archive.root()).unwrap().is_empty());
}
