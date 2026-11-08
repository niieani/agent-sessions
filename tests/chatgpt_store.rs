use agent_sessions::{
    adapters::{Adapter, EnumerateGate},
    chatgpt::ChatGptAdapter,
};
use serde_json::json;
use sha2::{Digest, Sha256};
#[test]
fn browser_capture_folder_is_a_native_file_store_with_machine_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let payload=json!({"update_time":1,"messages":[{"id":"u","create_time":1770000000,"author":{"role":"user"},"content":{"parts":["hello"]}}]}).to_string();
    let payload = format!("\n {payload}\n");
    std::fs::write(store.join("one.json"), &payload).unwrap();
    std::fs::write(store.join("one.meta.json"),json!({"accountId":"account-one","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
    let adapter = ChatGptAdapter;
    let refs = adapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    assert_eq!(refs.len(), 1);
    let capture = adapter
        .capture(&refs[0], &temp.path().join("captures"), "browser-machine")
        .unwrap();
    assert_eq!(capture.meta.host, "browser-machine");
    let paths = agent_sessions::adapters::capture_paths(
        &temp.path().join("captures"),
        "browser-machine",
        "chatgpt",
        &format!("{}~opening-message", capture.meta.native_id),
    )
    .unwrap();
    assert_eq!(paths.meta, capture.meta_path);
    assert_eq!(paths.body, capture.body_path);
    assert_eq!(
        capture.meta.native_meta.as_ref().unwrap()["accountId"],
        "account-one"
    );
    assert_eq!(
        adapter.extract(&capture).unwrap().unwrap().messages[0].text,
        "hello"
    );
}
#[test]
fn collector_must_record_account_identity() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("one.meta.json"),json!({"url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":"abc"}).to_string()).unwrap();
    assert!(
        format!(
            "{:#}",
            ChatGptAdapter
                .enumerate(temp.path(), &EnumerateGate::default())
                .unwrap_err()
        )
        .contains("accountId")
    );
}

#[test]
fn bundle_collection_accumulates_pages_without_losing_older_unique_messages() {
    use agent_sessions::{
        archive::Archive,
        bundle::{export, ingest_bundle},
        pull::StoreRoots,
    };
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    for (revision, ids) in [(1, vec!["u", "a", "b"]), (2, vec!["b", "c"])] {
        let store = temp.path().join(format!("store-{revision}"));
        std::fs::create_dir(&store).unwrap();
        let payload=json!({"update_time":revision,"messages":ids.iter().map(|id|json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]}})).collect::<Vec<_>>()}).to_string();
        std::fs::write(store.join("one.json"), &payload).unwrap();
        std::fs::write(store.join("one.meta.json"),json!({"accountId":"one","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":format!("2026-10-19T00:00:0{revision}Z"),"sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
        let empty = temp.path().join("empty");
        let roots = StoreRoots {
            chatgpt: store,
            claude: empty.clone(),
            codex: empty.clone(),
            cursor: empty.clone(),
            pi: empty.clone(),
            t3: empty,
            t3_explicit: false,
        };
        let mut bundle = Vec::new();
        export(
            &mut bundle,
            &roots,
            "browser-machine",
            Some("chatgpt"),
            None,
        )
        .unwrap();
        assert_eq!(
            ingest_bundle(bundle.as_slice(), &archive, &mut archive.db().unwrap())
                .unwrap()
                .stats
                .sessions_failed,
            0
        );
    }
    let capture = archive
        .lookup_capture("c", Some("chatgpt"), None)
        .unwrap()
        .capture;
    let conversation = ChatGptAdapter.extract(&capture).unwrap().unwrap();
    assert_eq!(
        conversation
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["u", "a", "b", "c"]
    );
}

#[test]
fn edited_opening_message_creates_independent_sessions_without_empty_root_views() {
    use agent_sessions::{
        archive::{Archive, rebuild},
        pull::{StoreRoots, pull_localhost},
        render::render_all,
    };
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let message = |id: &str, text: &str, time: i64| json!({"id":id,"create_time":time,"author":{"role":"user"},"content":{"parts":[text]}});
    for (name, url, payload) in [
        (
            "selected",
            "https://chatgpt.com/backend-api/conversations/c",
            json!({"update_time":1,"messages":[message("u","First opening",1770000000)]}),
        ),
        (
            "versions",
            "https://chatgpt.com/backend-api/conversations/c/versions?message_id=u",
            json!([[message("v", "Edited opening", 1770000010)]]),
        ),
    ] {
        let payload = payload.to_string();
        std::fs::write(store.join(format!("{name}.json")), &payload).unwrap();
        std::fs::write(store.join(format!("{name}.meta.json")),json!({"accountId":"one","url":url,"capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
    }
    let empty = temp.path().join("empty");
    let roots = StoreRoots {
        chatgpt: store,
        claude: empty.clone(),
        codex: empty.clone(),
        cursor: empty.clone(),
        pi: empty.clone(),
        t3: empty,
        t3_explicit: false,
    };
    let stats = pull_localhost(&archive, &roots, "browser-machine").unwrap();
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(
        archive.db().unwrap().sessions_with_dialog().unwrap().len(),
        2
    );
    assert_eq!(render_all(&archive).unwrap().transcripts, 2);
    assert_eq!(rebuild(&archive).unwrap().conversations_extracted, 2);
}

#[test]
fn exclusions_admit_incomplete_history_using_archive_context() {
    use agent_sessions::{
        archive::Archive,
        pull::{StoreRoots, pull_localhost},
    };
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let missing = temp.path().join("missing");
    let roots = StoreRoots {
        chatgpt: store.clone(),
        claude: missing.clone(),
        codex: missing.clone(),
        pi: missing.clone(),
        cursor: missing.clone(),
        t3: missing,
        t3_explicit: false,
    };
    for (revision, ids, incomplete) in [(1, vec!["u", "a", "b"], false), (2, vec!["b", "c"], true)]
    {
        let payload = json!({"update_time":revision,"page_info":{"has_previous_page":incomplete},
            "messages":ids.iter().map(|id|json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]}})).collect::<Vec<_>>()}).to_string();
        std::fs::write(store.join("one.json"), &payload).unwrap();
        std::fs::write(store.join("one.meta.json"),json!({"accountId":"one","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":format!("2026-10-19T00:00:0{revision}Z"),"sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
        if incomplete {
            std::fs::write(
                archive.root().join("config.toml"),
                "[pull]\nexclude-paths = ['**/*eval-workspaces/**/vault']\n",
            )
            .unwrap();
            // The native page alone cannot be extracted, but admission must not
            // extract it: the Archive already holds its missing ancestry.
            let session = ChatGptAdapter
                .enumerate(&store, &EnumerateGate::default())
                .unwrap()
                .remove(0);
            let capture = ChatGptAdapter
                .capture(&session, &temp.path().join("isolated"), "fixture")
                .unwrap();
            assert!(
                format!("{:#}", ChatGptAdapter.extract(&capture).unwrap_err())
                    .contains("incomplete history")
            );
        }
        let stats = pull_localhost(&archive, &roots, "fixture").unwrap();
        assert_eq!(stats.sessions_failed, 0);
        assert_eq!(stats.sessions_found, 1);
        if incomplete {
            // Full-source Export has no Bookmark and must use the same cheap
            // admission path as local Pull, even for an incomplete native page.
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
                .args([
                    "export",
                    "--source",
                    "chatgpt",
                    "--skip-repository-lookup",
                    "--exclude-path",
                    "**/*eval-workspaces/**/vault",
                    "--chatgpt-root",
                ])
                .arg(&store)
                .args(["--host", "fixture"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let ingested = agent_sessions::bundle::ingest_bundle(
                output.stdout.as_slice(),
                &archive,
                &mut archive.db().unwrap(),
            )
            .unwrap();
            assert_eq!(ingested.manifest.units_failed, 0);
            assert_eq!(ingested.manifest.unit_count, 1);
            let t3 = temp.path().join("empty-t3");
            std::fs::create_dir_all(t3.join("userdata")).unwrap();
            rusqlite::Connection::open(t3.join("userdata/statev2.sqlite"))
                .unwrap()
                .execute_batch(include_str!("fixtures/t3/v2-schema.sql"))
                .unwrap();
            let host = agent_sessions::fleet::HostConfig::localhost(
                "fixture".into(),
                Some(roots.claude.clone()),
                Some(roots.codex.clone()),
                Some(roots.pi.clone()),
                Some(roots.cursor.clone()),
                Some(t3),
                Some(store.clone()),
            );
            let stats = agent_sessions::fleet::pull_hosts(
                &archive,
                &[host],
                &agent_sessions::fleet::FleetPull {
                    transport: &agent_sessions::fleet::SystemSsh,
                    local_binary: std::path::Path::new(env!("CARGO_BIN_EXE_agent-sessions")),
                    full_source: Some("chatgpt"),
                },
            )
            .unwrap();
            assert!(!stats.failed(), "{stats:?}");
            assert_eq!(stats.totals.sessions_found, 1);
            let session = ChatGptAdapter
                .enumerate(&store, &EnumerateGate::default())
                .unwrap()
                .remove(0);
            assert_eq!(ChatGptAdapter.session_working_path(&session).unwrap(), None);
        }
    }
}
