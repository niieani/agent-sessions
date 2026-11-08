mod common;

use std::{fs, process::Command};

fn temp_dir() -> tempfile::TempDir {
    let staging = std::path::PathBuf::from("temp.local")
        .join(time::OffsetDateTime::now_utc().date().to_string());
    fs::create_dir_all(&staging).unwrap();
    tempfile::tempdir_in(staging).unwrap()
}

#[test]
fn rebuild_cli_never_creates_or_changes_render_target() {
    let root = temp_dir();
    let store = temp_dir();
    common::copy_tree(&common::fixtures_dir().join("projects"), store.path());
    common::pull(store.path(), root.path());
    let target = root.path().join("external-views");
    fs::write(
        root.path().join("config.toml"),
        format!("[views]\ntimezone='UTC'\noutput={target:?}\n"),
    )
    .unwrap();
    for existing in [false, true] {
        if existing {
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("sentinel.md"), "caller-owned bytes").unwrap();
        }
        let before = common::tree_bytes(&target);
        let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(["rebuild", "--archive"])
            .arg(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(target.exists(), existing);
        assert_eq!(common::tree_bytes(&target), before);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("rendered"));
    }
}
