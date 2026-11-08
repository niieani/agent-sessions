mod common;
#[allow(dead_code)]
#[path = "push_titles/support.rs"]
mod support;
use agent_sessions::push_titles::{PushTitleOptions, run};
use support::*;
#[test]
fn codex_external_names_require_explicit_source_opt_in() {
    let fixture = Fixture::new();
    let path = fixture.codex_root.join("state_5.sqlite");
    // First settle owned names; the next protected pass must be byte-identical.
    let options = PushTitleOptions {
        source: Some("codex".into()),
        dry_run: false,
    };
    run(&fixture.archive(), &options).unwrap();
    let before = std::fs::read(&path).unwrap();
    run(&fixture.archive(), &options).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let config_path = fixture.archive_dir.path().join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[titles.codex]\noverwrite-external = true\n");
    std::fs::write(config_path, config).unwrap();
    let result = run(&fixture.archive(), &options).unwrap();
    assert_eq!(result.pushed, 1);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let title: String = connection
        .query_row(
            "SELECT name FROM threads WHERE id = ?1",
            [CODEX_FOREIGN],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(title, "Codex Rejected");
    assert_eq!(
        read_pushed(fixture.archive_dir.path(), "codex", CODEX_FOREIGN).as_deref(),
        Some("Codex Rejected")
    );
    assert_eq!(run(&fixture.archive(), &options).unwrap().pushed, 0);
}
