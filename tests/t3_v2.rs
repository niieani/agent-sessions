mod common;
use agent_sessions::adapters::{Adapter, EnumerateGate, t3::T3Adapter};
use rusqlite::Connection;
use serde_json::json;

fn fixture(root: &std::path::Path) -> Connection {
    std::fs::create_dir_all(root.join("userdata")).unwrap();
    let c = Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    c.execute_batch(include_str!("fixtures/t3/v2-schema.sql"))
        .unwrap();
    c.execute_batch("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES ('p','Project','/work','[]','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z');").unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,active_provider_thread_id,created_at,updated_at,payload_json) VALUES ('t','p','Current','codex','full-access','default','pt','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z',?1)", [json!({"id":"t","branch":"work","modelSelection":{"model":"gpt-test"}}).to_string()]).unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_provider_threads(provider_thread_id,thread_id,provider,status,updated_at,payload_json) VALUES ('pt','t','codex','idle','2026-10-05T00:00:00Z',?1)",[json!({"nativeThreadRef":{"driver":"codex","nativeId":"0199b700-0000-7000-8000-000000000001","strength":"strong"}}).to_string()]).unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_messages(message_id,thread_id,role,streaming,created_at,updated_at,payload_json) VALUES ('m','t','user',0,'2026-10-05T00:00:00Z','2026-10-05T00:00:00Z',?1)",[json!({"text":"Actual prompt","attachments":[]}).to_string()]).unwrap();
    c
}

#[test]
fn v2_is_authoritative_and_rebuilds_exact_provider_links_and_dialog() {
    let d = tempfile::tempdir().unwrap();
    let _c = fixture(d.path());
    // The abandoned database may coexist. It must never be consulted.
    Connection::open(d.path().join("userdata/state.sqlite")).unwrap();
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    assert_eq!(refs.len(), 1);
    let cap = T3Adapter
        .capture(&refs[0], &d.path().join("captures"), "host")
        .unwrap();
    let p = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
    assert_eq!(p.title.as_deref(), Some("Current"));
    assert_eq!(p.branch.as_deref(), Some("work"));
    assert_eq!(p.messages[0].text, "Actual prompt");
    assert_eq!(
        p.link_facts[0].provider_native_id.as_deref(),
        Some("0199b700-0000-7000-8000-000000000001")
    );
}

#[test]
fn missing_v2_schema_is_an_error_even_with_legacy_tables() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute_batch("ALTER TABLE orchestration_v2_projection_provider_threads RENAME COLUMN payload_json TO changed_payload;").unwrap();
    let e = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap_err();
    assert!(format!("{e:#}").contains("orchestration_v2_projection_provider_threads"));
}

#[test]
fn missing_current_checkpoint_table_fails_even_for_an_empty_store() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute_batch("DELETE FROM orchestration_v2_projection_threads; DROP TABLE orchestration_v2_projection_checkpoints;").unwrap();
    let error = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "incompatible T3 orchestration v2 Store: missing table orchestration_v2_projection_checkpoints"
    );
}

#[test]
fn shared_provider_process_and_context_transfer_rows_are_captured() {
    use std::io::{BufRead, BufReader};
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute_batch("INSERT INTO orchestration_v2_projection_provider_sessions(provider_session_id,thread_id,provider,status,updated_at,payload_json) VALUES ('shared',NULL,'codex','ready','2026-10-05T00:00:00Z','{}');
        INSERT INTO orchestration_v2_projection_provider_session_bindings VALUES ('shared','t');
        INSERT INTO orchestration_v2_projection_context_transfers(context_transfer_id,source_thread_id,target_thread_id,type,status,updated_at,payload_json) VALUES ('transfer','other','t','fork','completed','2026-10-05T00:00:00Z','{}');
        INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,child_thread_id,origin,status,updated_at,payload_json) VALUES ('child','t','node','codex','child-thread','delegated','completed','2026-10-05T00:00:00Z','{\"prompt\":\"Review\",\"childThreadId\":\"child-thread\"}');").unwrap();
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let cap = T3Adapter
        .capture(&refs[0], &d.path().join("captures"), "host")
        .unwrap();
    let decoder =
        zstd::stream::read::Decoder::new(std::fs::File::open(&cap.body_path).unwrap()).unwrap();
    let rows: Vec<serde_json::Value> = BufReader::new(decoder)
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    for (table, key) in [
        (
            "orchestration_v2_projection_provider_sessions",
            "provider_session_id",
        ),
        (
            "orchestration_v2_projection_context_transfers",
            "context_transfer_id",
        ),
        ("orchestration_v2_projection_subagents", "subagent_id"),
    ] {
        let row = rows.iter().find(|row| row["table"] == table).unwrap();
        assert!(row["primaryKey"][key].is_string());
    }
    let p = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
    assert_eq!(
        p.link_facts.len(),
        1,
        "shared process is not another Session"
    );
    assert_eq!(p.activities[0].kind.as_deref(), Some("subagent"));
}

#[test]
fn native_children_and_weak_or_malformed_references_cannot_link_root_dialog() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute("INSERT INTO orchestration_v2_projection_provider_threads(provider_thread_id,thread_id,owner_node_id,provider,status,updated_at,payload_json) VALUES ('child','t','owner','codex','idle','2026-10-05T00:00:00Z',?1)",[json!({"nativeThreadRef":{"driver":"codex","nativeId":"0199b700-0000-7000-8000-000000000002","strength":"strong"}}).to_string()]).unwrap();
    for native in [
        json!({"driver":"codex","nativeId":"0199b700-0000-7000-8000-000000000001","strength":"weak"}),
        json!({"driver":"codex","nativeId":"not-a-uuid","strength":"strong"}),
        json!({"driver":"claudeAgent","nativeId":"not-a-uuid","strength":"strong"}),
        json!({"driver":"unknown","nativeId":"native-id","strength":"strong"}),
    ] {
        c.execute("UPDATE orchestration_v2_projection_provider_threads SET payload_json=?1 WHERE provider_thread_id='pt'",[json!({"nativeThreadRef":native}).to_string()]).unwrap();
        let refs = T3Adapter
            .enumerate(d.path(), &EnumerateGate::default())
            .unwrap();
        let cap = T3Adapter
            .capture(&refs[0], &d.path().join("captures"), "host")
            .unwrap();
        let p = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
        assert_eq!(p.link_facts.len(), 1);
        assert_ne!(
            p.link_facts[0].status,
            agent_sessions::model::OrchestratorLinkStatus::Candidate
        );
        assert!(p.link_facts[0].reason.is_some());
    }
}

#[test]
fn legacy_only_installation_fails_source_without_advancing_bookmark() {
    use agent_sessions::{archive::Archive, pull::pull_localhost};
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("userdata")).unwrap();
    Connection::open(d.path().join("userdata/state.sqlite")).unwrap();
    let a = tempfile::tempdir().unwrap();
    let archive = Archive::open(a.path()).unwrap();
    let mut roots = common::empty_roots(d.path());
    roots.t3 = d.path().to_path_buf();
    let result = pull_localhost(&archive, &roots, "host").unwrap();
    let t3 = result.sources.iter().find(|s| s.source == "t3").unwrap();
    assert_eq!(t3.sessions_failed, 1);
    assert!(
        t3.error
            .as_ref()
            .unwrap()
            .contains("expected orchestration v2 Store")
    );
    assert!(
        archive
            .db()
            .unwrap()
            .bookmark("host", "t3")
            .unwrap()
            .is_none()
    );
}

#[test]
fn current_store_enumerates_and_captures_v2_checkpoints_without_dropped_v1_tables() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='checkpoint_diff_blobs'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0,
        "T3 migration 63 drops the obsolete v1 table"
    );
    c.execute_batch("INSERT INTO orchestration_v2_projection_checkpoints(checkpoint_id,thread_id,scope_id,node_id,ordinal_within_scope,status,captured_at,payload_json) VALUES ('checkpoint','t','scope','node',1,'ready','2026-10-05T00:00:00Z','{}');").unwrap();
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let cap = T3Adapter
        .capture(&refs[0], &d.path().join("captures"), "host")
        .unwrap();
    let p = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
    assert_eq!(p.checkpoint_count, 1);
}

fn replace_with_legacy(cap: &mut agent_sessions::adapters::Capture) -> Vec<u8> {
    use std::io::Write;
    cap.meta.adapter_version = 2;
    cap.meta.source_mtime_ms += 86_400_000;
    cap.meta.native_path = cap
        .meta
        .native_path
        .replace("statev2.sqlite", "state.sqlite");
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
    for (table, columns) in [
        (
            "projection_projects",
            json!({"project_id":"p","title":"Project","workspace_root":"/work"}),
        ),
        (
            "projection_threads",
            json!({"thread_id":"t","project_id":"p","title":"Frozen"}),
        ),
    ] {
        let columns: Vec<_> = columns
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| json!({"name":key,"valueType":"text","value":value}))
            .collect();
        writeln!(
            encoder,
            "{}",
            json!({"record":"row","table":table,"primaryKey":{},"columns":columns})
        )
        .unwrap();
    }
    let body = encoder.finish().unwrap();
    std::fs::write(&cap.body_path, &body).unwrap();
    std::fs::write(&cap.meta_path, serde_json::to_vec(&cap.meta).unwrap()).unwrap();
    body
}

#[test]
fn v2_revision_epoch_replaces_higher_legacy_revision_and_retains_evidence() {
    let d = tempfile::tempdir().unwrap();
    let _c = fixture(d.path());
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let mut old = T3Adapter
        .capture(&refs[0], &d.path().join("captures"), "host")
        .unwrap();
    let body = replace_with_legacy(&mut old);
    let current = T3Adapter
        .capture(&refs[0], &d.path().join("captures"), "host")
        .unwrap();
    assert_eq!(
        current.outcome,
        agent_sessions::adapters::CaptureOutcome::Revised
    );
    assert!(current.meta.source_mtime_ms < old.meta.source_mtime_ms);
    let stem = old
        .meta_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_suffix(".meta.json")
        .unwrap();
    let prior = old
        .meta_path
        .with_file_name(format!("{stem}.revisions"))
        .join(format!("{}.ndjson.zst", old.meta.captured_at_ms));
    assert_eq!(std::fs::read(prior).unwrap(), body);
    assert_eq!(
        agent_sessions::adapters::t3::extract_projection(&current)
            .unwrap()
            .title
            .as_deref(),
        Some("Current")
    );
}

#[test]
fn v2_epoch_wins_bundle_ingest_and_global_election_without_disabling_v2_rollback_guard() {
    use agent_sessions::{
        archive::Archive,
        bundle::{export, ingest_bundle},
    };
    let d = tempfile::tempdir().unwrap();
    let _c = fixture(d.path());
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let a = tempfile::tempdir().unwrap();
    let archive = Archive::open(a.path()).unwrap();
    let mut cap = T3Adapter
        .capture(&refs[0], &archive.captures_dir(), "host")
        .unwrap();
    replace_with_legacy(&mut cap);
    let mut legacy = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
    legacy.host = "legacy-other-host".into();
    archive
        .db()
        .unwrap()
        .upsert_orchestrator_projection(&legacy)
        .unwrap();
    let mut roots = common::empty_roots(d.path());
    roots.t3 = d.path().to_path_buf();
    roots.t3_explicit = true;
    let mut bundle = Vec::new();
    export(&mut bundle, &roots, "host", Some("t3"), None).unwrap();
    ingest_bundle(&bundle[..], &archive, &mut archive.db().unwrap()).unwrap();
    let winner = archive
        .db()
        .unwrap()
        .orchestrator_thread("t3", "t")
        .unwrap()
        .unwrap();
    assert_eq!(winner.title.as_deref(), Some("Current"));
    assert_eq!(winner.winner_host, "host");
    let meta: agent_sessions::adapters::CaptureMeta =
        serde_json::from_slice(&std::fs::read(&cap.meta_path).unwrap()).unwrap();
    assert_eq!(
        meta.adapter_version,
        agent_sessions::adapters::t3::ADAPTER_VERSION
    );
    assert!(meta.source_mtime_ms < cap.meta.source_mtime_ms);
    let mut future = meta.clone();
    future.source_mtime_ms += 86_400_000;
    std::fs::write(&cap.meta_path, serde_json::to_vec(&future).unwrap()).unwrap();
    assert!(
        T3Adapter
            .capture(&refs[0], &archive.captures_dir(), "host")
            .unwrap_err()
            .to_string()
            .contains("moved backwards")
    );
}

#[test]
fn imported_thread_preserves_durable_legacy_links_through_pull_bundle_and_rebuild() {
    use agent_sessions::{
        archive::Archive,
        bundle::{export, ingest_bundle},
        pull::pull_localhost,
    };
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute_batch("DELETE FROM orchestration_v2_projection_provider_threads; INSERT INTO orchestration_v2_legacy_imports VALUES ('t','2026-10-05','2026-10-05',NULL,0,NULL);").unwrap();
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let a = tempfile::tempdir().unwrap();
    let archive = Archive::open(a.path()).unwrap();
    let mut cap = T3Adapter
        .capture(&refs[0], &archive.captures_dir(), "host")
        .unwrap();
    replace_with_legacy(&mut cap);
    use std::io::Write;
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
    let old = zstd::stream::decode_all(&std::fs::read(&cap.body_path).unwrap()[..]).unwrap();
    enc.write_all(&old).unwrap();
    for (table, columns) in [
        (
            "provider_session_runtime",
            json!({"provider_name":"codex","resume_cursor_json":"{\"threadId\":\"0199b700-0000-7000-8000-000000000001\"}"}),
        ),
        (
            "projection_thread_messages",
            json!({"role":"assistant","text":"Recovered reply","created_at":1791158400000_i64}),
        ),
    ] {
        let cols: Vec<_> = columns
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| json!({"name":k,"value":v}))
            .collect();
        writeln!(
            enc,
            "{}",
            json!({"record":"row","table":table,"columns":cols})
        )
        .unwrap();
    }
    std::fs::write(&cap.body_path, enc.finish().unwrap()).unwrap();
    let mut roots = common::empty_roots(d.path());
    roots.t3 = d.path().to_owned();
    let mut bundle = Vec::new();
    export(&mut bundle, &roots, "host", Some("t3"), None).unwrap();
    ingest_bundle(&bundle[..], &archive, &mut archive.db().unwrap()).unwrap();
    let stats = pull_localhost(&archive, &roots, "host").unwrap();
    assert_eq!(stats.sessions_failed, 0);
    for _ in 0..2 {
        let meta = serde_json::from_slice(&std::fs::read(&cap.meta_path).unwrap()).unwrap();
        cap.meta = meta;
        let p = agent_sessions::adapters::t3::extract_projection(&cap).unwrap();
        assert_eq!(p.link_facts.len(), 1);
        assert_eq!(
            p.link_facts[0].provider_native_id.as_deref(),
            Some("0199b700-0000-7000-8000-000000000001")
        );
        assert!(p.messages.iter().any(|m| m.text == "Recovered reply"));
        agent_sessions::archive::rebuild(&archive).unwrap();
        let db = archive.db().unwrap();
        let recovered = db
            .lookup_session_by_orchestrator_id("t3", "t")
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered.session_native_id,
            "0199b700-0000-7000-8000-000000000001"
        );
        assert!(
            db.sessions_with_dialog()
                .unwrap()
                .iter()
                .any(|s| s.native_id == recovered.session_native_id)
        );
    }
    c.execute("INSERT INTO orchestration_v2_projection_provider_threads(provider_thread_id,thread_id,provider,status,updated_at,payload_json) VALUES ('new','t','codex','idle','2026-10-05T00:00:00Z',?1)", [json!({"nativeThreadRef":{"driver":"codex","nativeId":"0199b700-0000-7000-8000-000000000002","strength":"weak"}}).to_string()]).unwrap();
    let current = T3Adapter
        .capture(&refs[0], &archive.captures_dir(), "host")
        .unwrap();
    let p = agent_sessions::adapters::t3::extract_projection(&current).unwrap();
    assert_eq!(
        p.link_facts[0].status,
        agent_sessions::model::OrchestratorLinkStatus::Malformed
    );
    assert!(
        !p.messages.iter().any(|m| m.text == "Recovered reply"),
        "a root provider row suppresses inherited evidence even if malformed"
    );
}

#[test]
fn full_source_pull_reopens_bookmark_and_is_idempotent() {
    use agent_sessions::archive::Archive;
    let d = tempfile::tempdir().unwrap();
    let _c = fixture(d.path());
    let a = tempfile::tempdir().unwrap();
    let archive = Archive::open(a.path()).unwrap();
    let refs = T3Adapter
        .enumerate(d.path(), &EnumerateGate::default())
        .unwrap();
    let cap = T3Adapter
        .capture(&refs[0], &archive.captures_dir(), "host")
        .unwrap();
    let body = std::fs::read(&cap.body_path).unwrap();
    let missing = d.path().join("missing");
    std::fs::write(a.path().join("config.toml"),format!("[[host]]\nname='host'\nt3-root={:?}\nclaude-root={:?}\ncodex-root={:?}\npi-root={:?}\ncursor-root={:?}\n", d.path().to_str().unwrap(),missing.to_str().unwrap(),missing.to_str().unwrap(),missing.to_str().unwrap(),missing.to_str().unwrap())).unwrap();
    archive
        .db()
        .unwrap()
        .advance_bookmark("host", "t3", i64::MAX)
        .unwrap();
    archive
        .db()
        .unwrap()
        .advance_bookmark("host", "codex", i64::MAX)
        .unwrap();
    for flag in [false, true, true] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"));
        cmd.args(["pull", "--archive"]).arg(a.path());
        if flag {
            cmd.args(["--full-source", "t3"]);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let db = archive.db().unwrap();
        assert_eq!(
            db.bookmark("host", "t3").unwrap(),
            Some(if flag { refs[0].mtime_ms } else { i64::MAX })
        );
        assert_eq!(db.bookmark("host", "codex").unwrap(), Some(i64::MAX));
        assert_eq!(std::fs::read(&cap.body_path).unwrap(), body);
    }
}

#[test]
fn bare_bundle_export_isolates_schema_drift() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    c.execute_batch("DROP TABLE orchestration_v2_legacy_imports")
        .unwrap();
    let mut roots = common::empty_roots(d.path());
    roots.t3 = d.path().to_owned();
    let mut bytes = Vec::new();
    let stats = agent_sessions::bundle::export(&mut bytes, &roots, "host", None, None).unwrap();
    assert_eq!(stats.sessions_failed, 1);
    assert!(!bytes.is_empty());
    assert!(agent_sessions::bundle::export(Vec::new(), &roots, "host", Some("t3"), None).is_err());
}

#[test]
fn deleted_worktree_resolves_from_captured_t3_project_repository() {
    t3_repository_fallback(false);
}

#[test]
fn unavailable_workspace_resolves_from_t3_without_recording_missing() {
    t3_repository_fallback(true);
}

fn t3_repository_fallback(unavailable: bool) {
    use agent_sessions::{
        archive::{Archive, rebuild},
        pull::pull_localhost,
        render::resolved_projects,
    };
    let tmp = tempfile::tempdir().unwrap();
    let c = fixture(&tmp.path().join("t3"));
    rename_thread(&c, "thread:deleted-worktree/non-uuid");
    c.execute_batch("ALTER TABLE projection_projects ADD COLUMN repository_identity_json TEXT;
        UPDATE projection_projects SET repository_identity_json='{\"canonicalKey\":\"example-org/infrastructure\"}';").unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    c.execute("UPDATE orchestration_v2_projection_provider_threads SET provider='claudeAgent', payload_json=?1", [json!({"nativeThreadRef":{"driver":"claudeAgent","nativeId":id,"strength":"strong"}}).to_string()]).unwrap();
    let store = tmp.path().join("claude/project");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join(format!("{id}.jsonl")), format!("{}\n",json!({"type":"user","sessionId":id,"cwd":tmp.path().join("deleted-worktree"),"gitBranch":"feature","timestamp":"2026-10-05T00:00:00Z","message":{"role":"user","content":"Fix it"}}))).unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = tmp.path().join("claude");
    struct Unavailable;
    impl agent_sessions::repository_resolution::Resolver for Unavailable {
        fn lookup(&self, _: &str) -> anyhow::Result<agent_sessions::repository_resolution::Lookup> {
            anyhow::bail!("repository process timed out; retry later")
        }
    }
    // T3 may arrive after either an observed missing workspace or a lookup
    // execution failure. Neither case needs another directory probe.
    if unavailable {
        agent_sessions::pull::pull_localhost_with_resolver(&archive, &roots, "host", &Unavailable)
            .unwrap();
        assert!(
            archive.db().unwrap().sessions_with_dialog().unwrap()[0]
                .repository_resolution
                .is_none()
        );
    } else {
        pull_localhost(&archive, &roots, "host").unwrap();
        assert!(
            archive.db().unwrap().sessions_with_dialog().unwrap()[0]
                .repository_resolution
                .as_ref()
                .unwrap()
                .unresolved
        );
    }
    roots.t3 = tmp.path().join("t3");
    struct NoLookup;
    impl agent_sessions::repository_resolution::Resolver for NoLookup {
        fn lookup(&self, _: &str) -> anyhow::Result<agent_sessions::repository_resolution::Lookup> {
            panic!("settled Missing directory must not be looked up again");
        }
    }
    let resolver: &dyn agent_sessions::repository_resolution::Resolver =
        if unavailable { &Unavailable } else { &NoLookup };
    agent_sessions::pull::pull_localhost_with_resolver(&archive, &roots, "host", resolver).unwrap();
    let rows = archive.db().unwrap().sessions_with_dialog().unwrap();
    let session = rows.iter().find(|s| s.source == "claude").unwrap();
    assert_eq!(
        resolved_projects(&archive).unwrap()[&session.id].as_deref(),
        Some("infrastructure")
    );
    std::fs::remove_dir_all(tmp.path().join("t3")).unwrap();
    rebuild(&archive).unwrap();
    assert!(
        resolved_projects(&archive)
            .unwrap()
            .values()
            .any(|v| v.as_deref() == Some("infrastructure"))
    );
}

#[test]
fn repository_identity_prefers_fork_origin_to_upstream() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = fixture(tmp.path());
    conn.execute_batch("ALTER TABLE projection_projects ADD COLUMN repository_identity_json TEXT")
        .unwrap();
    conn.execute("UPDATE projection_projects SET repository_identity_json=?1",[json!({"canonicalKey":"github.com/example-upstream/importer","origin":{"canonicalKey":"github.com/example-org/importer"}}).to_string()]).unwrap();
    let refs = T3Adapter
        .enumerate(tmp.path(), &EnumerateGate::default())
        .unwrap();
    let capture = T3Adapter
        .capture(&refs[0], &tmp.path().join("captures"), "host")
        .unwrap();
    let projection = agent_sessions::adapters::t3::extract_projection(&capture).unwrap();
    assert_eq!(
        projection.repository_url.as_deref(),
        Some("example-org/importer")
    );
    // The same rule applies to repositoryIdentity in captured Project events.
    conn.execute(
        "UPDATE projection_projects SET repository_identity_json=NULL",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO orchestration_events(sequence,event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json) VALUES (1,'e','project','p',1,'project.updated','2026-10-05T00:00:00Z','user',?1,'{}')",[json!({"repositoryIdentity":{"canonicalKey":"github.com/example-upstream/importer","origin":{"canonicalKey":"github.com/example-org/importer"}}}).to_string()]).unwrap();
    let refs = T3Adapter
        .enumerate(tmp.path(), &EnumerateGate::default())
        .unwrap();
    let capture = T3Adapter
        .capture(&refs[0], &tmp.path().join("captures"), "host")
        .unwrap();
    assert_eq!(
        agent_sessions::adapters::t3::extract_projection(&capture)
            .unwrap()
            .repository_url
            .as_deref(),
        Some("example-org/importer")
    );
}

#[test]
fn corrupt_legacy_t3_repository_unit_is_counted_without_aborting_pull() {
    let tmp = tempfile::tempdir().unwrap();
    let conn = fixture(&tmp.path().join("t3"));
    let id = "11111111-1111-4111-8111-111111111111";
    conn.execute("UPDATE orchestration_v2_projection_provider_threads SET provider='claudeAgent',payload_json=?1",[json!({"nativeThreadRef":{"driver":"claudeAgent","nativeId":id,"strength":"strong"}}).to_string()]).unwrap();
    let store = tmp.path().join("claude/project");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join(format!("{id}.jsonl")),format!("{}\n",json!({"type":"user","sessionId":id,"cwd":tmp.path().join("gone"),"gitBranch":"feature","timestamp":"2026-10-05T00:00:00Z","message":{"role":"user","content":"fix"}}))).unwrap();
    let archive = agent_sessions::archive::Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(tmp.path());
    roots.claude = tmp.path().join("claude");
    roots.t3 = tmp.path().join("t3");
    agent_sessions::pull::pull_localhost(&archive, &roots, "host").unwrap();
    rusqlite::Connection::open(archive.db_path())
        .unwrap()
        .execute("DELETE FROM t3_repository_evidence", [])
        .unwrap();
    std::fs::write(
        agent_sessions::adapters::capture_paths(&archive.captures_dir(), "host", "t3", "t")
            .unwrap()
            .body,
        b"broken zstd",
    )
    .unwrap();
    let stats = agent_sessions::pull::pull_localhost(&archive, &roots, "host").unwrap();
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(stats.repository_refresh[0].units_failed, 1);
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .iter()
            .any(|s| s.native_id == id)
    );
}

fn rename_thread(conn: &Connection, id: &str) {
    let tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        let has_thread = conn
            .prepare(&format!("PRAGMA table_info({quoted})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .any(|r| r.unwrap() == "thread_id");
        if has_thread {
            conn.execute(
                &format!("UPDATE {quoted} SET thread_id=?1 WHERE thread_id='t'"),
                [id],
            )
            .unwrap();
        }
    }
}

#[test]
fn pull_resolves_non_uuid_and_overlong_t3_capture_names_and_legacy_evidence() {
    use agent_sessions::{archive::Archive, pull::pull_localhost, repository_resolution::Lookup};
    for id in [
        "thread:non-uuid/project".to_owned(),
        format!("thread:{}", "long-native-identity/".repeat(30)),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let conn = fixture(&tmp.path().join("t3"));
        rename_thread(&conn, &id);
        conn.execute(
            "UPDATE projection_projects SET workspace_root=?1",
            [tmp.path().to_str().unwrap()],
        )
        .unwrap();
        for args in [
            vec!["init"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/example-org/infrastructure.git",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(tmp.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let store = tmp.path().join("claude/project");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("11111111-1111-4111-8111-111111111111.jsonl"),format!("{}\n",json!({"type":"user","sessionId":"11111111-1111-4111-8111-111111111111","cwd":tmp.path(),"gitBranch":"main","timestamp":"2026-10-05T00:00:00Z","message":{"role":"user","content":"resolve repository"}}))).unwrap();
        let archive = Archive::open(&tmp.path().join("archive")).unwrap();
        let mut roots = common::empty_roots(tmp.path());
        roots.claude = tmp.path().join("claude");
        roots.t3 = tmp.path().join("t3");
        let stats = pull_localhost(&archive, &roots, "host").unwrap();
        assert_eq!(stats.sessions_failed, 0, "{id}: {stats:?}");
        assert_eq!(stats.sessions_found, 2);
        let sql = Connection::open(archive.db_path()).unwrap();
        let fact: String = sql
            .query_row(
                "SELECT fact FROM repository_resolutions WHERE source='t3' AND native_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        let fact: agent_sessions::repository_resolution::Fact =
            serde_json::from_str(&fact).unwrap();
        assert!(matches!(fact.lookup, Lookup::Resolved(_)));
        sql.execute("DELETE FROM t3_repository_evidence", [])
            .unwrap();
        sql.execute("DELETE FROM repository_resolutions", [])
            .unwrap();
        let stats = pull_localhost(&archive, &roots, "host").unwrap();
        assert_eq!(stats.sessions_found, 0);
        assert_eq!(stats.sessions_failed, 0, "{stats:?}");
        let count: i64 = sql
            .query_row(
                "SELECT COUNT(*) FROM t3_repository_evidence WHERE native_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let count: i64 = sql
            .query_row(
                "SELECT COUNT(*) FROM repository_resolutions WHERE source='t3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn changed_t3_thread_keeps_previous_repository_observation_for_retry() {
    use agent_sessions::repository_resolution::{Fact, Lookup};
    let temp = tempfile::tempdir().unwrap();
    let conn = fixture(temp.path());
    let captures = temp.path().join("captures");
    let refs = T3Adapter
        .enumerate(temp.path(), &EnumerateGate::default())
        .unwrap();
    let mut capture = T3Adapter.capture(&refs[0], &captures, "host").unwrap();
    let fact = Fact {
        source_revision: (capture.meta.source_mtime_ms, capture.meta.source_size),
        branch_recorded: true,
        path: "/work".into(),
        lookup: Lookup::Path,
        unresolved: false,
    };
    capture.meta.repository_resolution = Some(fact.clone());
    std::fs::write(
        &capture.meta_path,
        serde_json::to_vec(&capture.meta).unwrap(),
    )
    .unwrap();
    conn.execute_batch(
        "UPDATE orchestration_v2_projection_threads SET updated_at='2026-10-06T00:00:00Z'",
    )
    .unwrap();
    let capture = T3Adapter.capture(&refs[0], &captures, "host").unwrap();
    assert!(capture.written);
    assert_eq!(capture.meta.repository_resolution, Some(fact.clone()));
    assert_ne!(
        fact.source_revision,
        (capture.meta.source_mtime_ms, capture.meta.source_size)
    );
}

#[test]
fn admission_uses_recorded_worktree_in_native_and_capture_metadata() {
    let d = tempfile::tempdir().unwrap();
    let c = fixture(d.path());
    for path in [Some("/work/eval-workspaces/run/vault"), None] {
        c.execute(
            "UPDATE orchestration_v2_projection_threads SET payload_json=?1",
            [json!({"worktreePath":path}).to_string()],
        )
        .unwrap();
        let session = T3Adapter
            .enumerate(d.path(), &EnumerateGate::default())
            .unwrap()
            .remove(0);
        let native = T3Adapter.session_working_path(&session).unwrap();
        assert_eq!(native.as_deref(), path);
        let dest = tempfile::tempdir().unwrap();
        let capture = T3Adapter.capture(&session, dest.path(), "fixture").unwrap();
        assert_eq!(T3Adapter.capture_working_path(&capture).unwrap(), native);
    }
}
