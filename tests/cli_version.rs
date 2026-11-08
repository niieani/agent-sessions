use std::process::Command;

#[test]
fn binary_version_matches_package_and_fleet_handshake() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let line = agent_sessions::fleet::expected_version_line();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), line);
    assert!(line.starts_with(&format!("agent-sessions {} (", env!("CARGO_PKG_VERSION"))));
}
