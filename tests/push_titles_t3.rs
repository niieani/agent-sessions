#[allow(dead_code)]
#[path = "push_titles/support.rs"]
mod support;
use agent_sessions::push_titles::{PushTitleDisposition as D, PushTitleOptions, run};
use std::fs;
use support::*;

#[path = "push_titles/t3_support.rs"]
mod t3_support;
use t3_support::*;

#[test]
fn t3_dry_run_matrix_and_multiple_threads() {
    let (f, _) = fixture();
    let r = run(
        &f.archive(),
        &PushTitleOptions {
            source: Some("t3".into()),
            dry_run: true,
        },
    )
    .unwrap();
    let dispositions = r
        .plans
        .iter()
        .map(|p| (p.native_id.as_str(), p.disposition))
        .collect::<Vec<_>>();
    assert_eq!(
        dispositions,
        vec![
            ("crash", D::Keep),
            ("empty", D::Push),
            ("equal", D::Unchanged),
            ("external", D::Keep),
            ("missing", D::Missing),
            ("own", D::Push)
        ]
    );
    let config = f.archive_dir.path().join("config.toml");
    let mut s = fs::read_to_string(&config).unwrap();
    s.push_str("\n[titles.t3]\noverwrite-external=true\n");
    fs::write(config, s).unwrap();
    let r = run(
        &f.archive(),
        &PushTitleOptions {
            source: Some("t3".into()),
            dry_run: true,
        },
    )
    .unwrap();
    assert_eq!((r.pushed, r.unchanged, r.missing), (4, 1, 1));
}
#[test]
fn t3_crash_recovery_preserves_other_thread_ownership_without_server() {
    let (f, root) = fixture();
    let c = rusqlite::Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    c.execute(
        "UPDATE orchestration_v2_projection_threads SET title='Person' WHERE thread_id IN ('empty','own')",
        [],
    )
    .unwrap();
    let dir =
        agent_sessions::digest::digest_dir(f.archive_dir.path(), "claude", CLAUDE_FRESH).unwrap();
    fs::write(dir.join("push-title.pending"),serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"source":"t3","nativeId":"crash","previous":null,"title":"Claude Fresh"})).unwrap()).unwrap();
    let opts = PushTitleOptions {
        source: Some("t3".into()),
        dry_run: false,
    };
    run(&f.archive(), &opts).unwrap();
    assert!(!dir.join("push-title.pending").exists());
    let d: agent_sessions::digest::SessionDigest =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(d.pushed["t3:crash"], "Claude Fresh");
    assert_eq!(d.pushed["t3:equal"], "Claude Fresh");
    assert_eq!(run(&f.archive(), &opts).unwrap().pushed, 0);
}

#[test]
fn t3_local_thread_remains_target_when_remote_projection_wins() {
    let (f, _) = fixture();
    let mut db = f.archive().db().unwrap();
    let mut t = orchestrator("external", "remote", 10);
    t.link_facts[0].provider_native_id = Some(CLAUDE_FRESH.into());
    db.upsert_orchestrator_projection(&t).unwrap();
    db.resolve_orchestrator_links().unwrap();
    let r = run(
        &f.archive(),
        &PushTitleOptions {
            source: Some("t3".into()),
            dry_run: true,
        },
    )
    .unwrap();
    assert!(r.plans.iter().any(|p| p.native_id == "external"));
}

#[test]
fn unfiltered_title_push_reports_t3_drift_and_keeps_native_plans() {
    let (f, root) = fixture();
    let c = rusqlite::Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    c.execute_batch("DROP TABLE orchestration_v2_projection_threads")
        .unwrap();
    let r = run(
        &f.archive(),
        &PushTitleOptions {
            source: None,
            dry_run: true,
        },
    )
    .unwrap();
    assert!(!r.plans.is_empty());
    assert_eq!(r.source_errors.len(), 1);
    assert_eq!(r.source_errors[0].0, "t3");
}
