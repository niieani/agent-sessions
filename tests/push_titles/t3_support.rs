use super::support::*;
use agent_sessions::model::*;
use std::fs;

pub fn orchestrator(thread_id: &str, host: &str, revision: i64) -> OrchestratorProjection {
    OrchestratorProjection {
        repository_url: None,
        source: "t3".into(),
        native_id: thread_id.into(),
        host: host.into(),
        native_path: "/store/statev2.sqlite".into(),
        logical_revision_ms: revision,
        discriminator: revision as u64,
        adapter_version: 1,
        title: Some(format!("Thread {thread_id}")),
        project_id: Some("project-1".into()),
        project_title: Some("Project".into()),
        workspace_root: Some("/work/project".into()),
        provider_name: Some("claudeAgent".into()),
        provider_instance_id: Some("instance-1".into()),
        model: Some("claude-fable-5".into()),
        runtime_mode: Some("local".into()),
        interaction_mode: Some("default".into()),
        branch: Some("main".into()),
        worktree: None,
        created_at_ms: Some(1),
        updated_at_ms: Some(revision),
        archived: false,
        pinned: true,
        settled: false,
        linked_pull_request: None,
        checkpoint_count: 1,
        link_facts: vec![OrchestratorLinkFact {
            status: OrchestratorLinkStatus::Candidate,
            provider_source: Some("claude".into()),
            provider_native_id: Some("provider-1".into()),
            reason: None,
        }],
        messages: vec![OrchestratorMessage {
            role: "user".into(),
            text: "hello".into(),
            timestamp_ms: Some(2),
        }],
        turns: vec![OrchestratorTurn {
            native_id: "turn-1".into(),
            state: Some("completed".into()),
            checkpoint_id: Some("checkpoint-1".into()),
            payload_json: Some("{\"future\":true}".into()),
        }],
        activities: vec![OrchestratorActivity {
            native_id: "activity-1".into(),
            kind: Some("future-kind".into()),
            payload_json: Some("{\"unknown\":true}".into()),
        }],
    }
}

pub fn fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    let root = f.stores_dir.path().join("t3");
    fs::create_dir_all(root.join("userdata")).unwrap();
    let config = f.archive_dir.path().join("config.toml");
    let s = fs::read_to_string(&config)
        .unwrap()
        .replace("[views]", &format!("t3-root = {:?}\n[views]", root));
    fs::write(config, s).unwrap();
    let c = rusqlite::Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    c.execute_batch("CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,title TEXT); INSERT INTO orchestration_v2_projection_threads VALUES ('empty',''),('own','Old'),('equal','Claude Fresh'),('external','Person'),('crash','Claude Fresh');").unwrap();
    let mut db = f.archive().db().unwrap();
    for id in ["empty", "own", "equal", "external", "missing", "crash"] {
        let mut t = orchestrator(id, HOST, 1);
        t.link_facts[0].provider_native_id = Some(CLAUDE_FRESH.into());
        db.upsert_orchestrator_projection(&t).unwrap();
    }
    db.resolve_orchestrator_links().unwrap();
    let p = agent_sessions::digest::digest_dir(f.archive_dir.path(), "claude", CLAUDE_FRESH)
        .unwrap()
        .join("session.json");
    let mut d: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    d["pushed"]["t3:own"] = "Old".into();
    d["pushed"]["t3:equal"] = "Claude Fresh".into();
    fs::write(p, serde_json::to_vec(&d).unwrap()).unwrap();
    (f, root)
}
