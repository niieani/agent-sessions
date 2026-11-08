mod common;
use agent_sessions::{
    adapters::{Adapter, claude::ClaudeAdapter},
    pull,
};
#[test]
fn settings_report_default_explicit_and_invalid_retention() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        ClaudeAdapter.store_retention(&root).unwrap().unwrap().days,
        30
    );
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"cleanupPeriodDays":90}"#,
    )
    .unwrap();
    assert_eq!(
        ClaudeAdapter.store_retention(&root).unwrap().unwrap().days,
        90
    );
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"cleanupPeriodDays":"never"}"#,
    )
    .unwrap();
    assert!(ClaudeAdapter.store_retention(&root).is_err());
}
#[test]
fn export_and_local_pull_report_retention_and_warnings_are_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let roots = common::empty_roots(dir.path());
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = agent_sessions::archive::Archive::open(archive_dir.path()).unwrap();
    let mut stats = pull::pull_localhost(&archive, &roots, "fixture").unwrap();
    let warnings = pull::store_retention_warnings(&stats, true);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("cleanupPeriodDays"));
    stats.sources.push(stats.sources[0].clone());
    assert_eq!(pull::store_retention_warnings(&stats, true).len(), 1);
    assert!(pull::store_retention_warnings(&stats, false).is_empty());
    let mut bytes = Vec::new();
    agent_sessions::bundle::export(&mut bytes, &roots, "fixture", Some("claude"), None).unwrap();
    let result = agent_sessions::bundle::ingest_bundle(
        bytes.as_slice(),
        &archive,
        &mut archive.db().unwrap(),
    )
    .unwrap();
    assert_eq!(result.manifest.retention["claude"].days, 30);
}

#[test]
fn warning_switch_defaults_on_and_title_opt_in_is_source_scoped() {
    use agent_sessions::fleet::FleetConfig;
    assert!(FleetConfig::parse("").unwrap().pull.warn_store_retention);
    assert!(
        !FleetConfig::parse("[pull]\nwarn-store-retention = false")
            .unwrap()
            .pull
            .warn_store_retention
    );
    assert!(
        FleetConfig::parse("[titles.codex]\noverwrite-external = true")
            .unwrap()
            .titles["codex"]
            .overwrite_external
    );
    assert!(FleetConfig::parse("[titles.codxe]\noverwrite-external = true").is_err());
    assert!(FleetConfig::parse("[titles.cursor]\noverwrite-external = true").is_err());
}

#[test]
fn long_retention_is_silent_but_default_and_short_windows_warn() {
    for (days, expected) in [(0, 1), (7, 1), (30, 1), (31, 0), (36500, 0)] {
        let mut stats = pull::PullStats::default();
        stats.add_source(pull::SourcePullStats {
            host: "fixture".into(),
            source: "claude".into(),
            retention: Some(pull::StoreRetention {
                days,
                setting: "cleanupPeriodDays".into(),
                settings_path: "settings.json".into(),
            }),
            ..Default::default()
        });
        assert_eq!(
            pull::store_retention_warnings(&stats, true).len(),
            expected,
            "{days}"
        );
    }
}
