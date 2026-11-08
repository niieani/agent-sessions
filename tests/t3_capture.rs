mod common;

use std::io::{BufRead, BufReader};

use agent_sessions::adapters::t3::{SOURCE, T3Adapter};
use agent_sessions::adapters::{Adapter, EnumerateGate};
use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::bundle::{export, ingest_bundle};
use agent_sessions::pull::pull_localhost;
use agent_sessions::render::render_all;
use rusqlite::Connection;

use common::{align_capture_times, empty_roots, tree_bytes};

const THREAD: &str = "11111111-1111-4111-8111-111111111111";
const ARCHIVED: &str = "22222222-2222-4222-8222-222222222222";

#[path = "fixtures/t3/support.rs"]
mod support;
use support::fixture;

#[test]
fn t3_enumerate_and_capture_are_exact_ordered_and_self_contained() {
    let dir = tempfile::tempdir().unwrap();
    let writer = fixture(dir.path());
    let before = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    assert_eq!(
        before
            .iter()
            .map(|item| item.native_id.as_str())
            .collect::<Vec<_>>(),
        [THREAD, ARCHIVED]
    );
    let first = before.iter().find(|item| item.native_id == THREAD).unwrap();
    writer
        .execute(
            "UPDATE orchestration_v2_projection_messages SET payload_json=json_set(payload_json,'$.text','jello') WHERE message_id='message'",
            [],
        )
        .unwrap();
    let changed = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    let changed = changed
        .iter()
        .find(|item| item.native_id == THREAD)
        .unwrap();
    assert_eq!(changed.mtime_ms, first.mtime_ms);
    assert_ne!(
        changed.size, first.size,
        "exact same-length value changes digest"
    );

    let capture = T3Adapter
        .capture(changed, &dir.path().join("captures"), "host")
        .unwrap();
    assert_eq!(capture.meta.source, SOURCE);
    assert_eq!(
        capture.meta.native_meta.as_ref().unwrap()["projectId"],
        "project"
    );
    let decoder =
        zstd::stream::read::Decoder::new(std::fs::File::open(&capture.body_path).unwrap()).unwrap();
    let lines = BufReader::new(decoder)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
        .collect::<Vec<_>>();
    let tables = lines
        .iter()
        .filter_map(|line| line["table"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        &tables[..3],
        [
            "projection_projects",
            "orchestration_v2_projection_threads",
            "orchestration_v2_projection_provider_threads"
        ]
    );
    assert!(
        tables
            .windows(2)
            .any(|pair| pair == ["orchestration_v2_events", "orchestration_v2_events"])
    );
    let message = lines
        .iter()
        .find(|line| line["table"] == "orchestration_v2_projection_messages")
        .unwrap();
    assert!(
        message["columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|column| column["name"] == "future_blob"
                && column["valueType"] == "blob"
                && column["encoding"] == "base64")
    );
    let attachment = lines
        .iter()
        .find(|line| line["record"] == "attachment")
        .unwrap();
    assert_eq!(attachment["status"], "captured");
    assert_eq!(attachment["valueType"], "blob");
    let attachment_body = lines
        .iter()
        .filter(|line| line["record"] == "attachment-body")
        .flat_map(|line| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(line["value"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(attachment_body, b"hello");
    let projection = agent_sessions::adapters::t3::extract_projection(&capture).unwrap();
    assert_eq!(projection.title.as_deref(), Some("Live"));
    assert!(
        projection
            .model
            .as_deref()
            .unwrap()
            .contains("claude-fable-5")
    );
    assert_eq!(projection.messages[0].text, "jello");
    assert_eq!(projection.activities[0].native_id, "activity");
    assert_eq!(
        projection.link_facts[0].provider_source.as_deref(),
        Some("claude")
    );
    assert_eq!(
        projection.link_facts[0].provider_native_id.as_deref(),
        Some("33333333-3333-4333-8333-333333333333")
    );
}

#[test]
fn t3_existing_bad_root_and_schema_fail_loudly() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        T3Adapter
            .enumerate(dir.path(), &EnumerateGate::default())
            .is_err()
    );
    std::fs::create_dir_all(dir.path().join("userdata")).unwrap();
    Connection::open(dir.path().join("userdata/statev2.sqlite")).unwrap();
    let error = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap_err();
    assert!(format!("{error:#}").contains("projection_projects"));
}

#[test]
fn missing_default_t3_store_is_empty_but_missing_explicit_root_is_a_source_failure() {
    let stores = tempfile::tempdir().unwrap();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut roots = empty_roots(stores.path());

    let default_missing = pull_localhost(&archive, &roots, "host").unwrap();
    let t3 = default_missing
        .sources
        .iter()
        .find(|source| source.source == SOURCE)
        .unwrap();
    assert_eq!(t3.sessions_failed, 0);
    assert!(t3.error.is_none());

    roots.t3_explicit = true;
    let explicit_missing = pull_localhost(&archive, &roots, "host").unwrap();
    let t3 = explicit_missing
        .sources
        .iter()
        .find(|source| source.source == SOURCE)
        .unwrap();
    assert_eq!(t3.sessions_failed, 1);
    assert!(
        t3.error
            .as_deref()
            .unwrap()
            .contains("explicit T3 root has no Store")
    );
}

#[test]
fn rebuild_isolates_one_bad_t3_thread_and_withholds_its_source_bookmark() {
    let stores = tempfile::tempdir().unwrap();
    let _writer = fixture(stores.path());
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut roots = empty_roots(stores.path());
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;
    pull_localhost(&archive, &roots, "host").unwrap();

    let bad = archive
        .captures_dir()
        .join("host/t3")
        .join(format!("{ARCHIVED}.ndjson.zst"));
    std::fs::write(
        bad,
        zstd::stream::encode_all(b"not valid T3 NDJSON\n".as_slice(), 0).unwrap(),
    )
    .unwrap();
    let stats = rebuild(&archive).unwrap();
    assert_eq!(stats.captures_failed, 1);
    assert_eq!(stats.orchestrator_threads_extracted, 1);
    assert!(
        archive
            .db()
            .unwrap()
            .orchestrator_thread(SOURCE, THREAD)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        archive.db().unwrap().bookmark("host", SOURCE).unwrap(),
        None
    );
}

#[test]
fn t3_direct_bundle_arrival_orders_and_rebuild_converge() {
    let stores = tempfile::tempdir().unwrap();
    let _writer = fixture(stores.path());
    let claude = stores.path().join("claude/-work");
    std::fs::create_dir_all(&claude).unwrap();
    let provider_id = "33333333-3333-4333-8333-333333333333";
    std::fs::write(
        claude.join(format!("{provider_id}.jsonl")),
        concat!(
            r#"{"type":"user","cwd":"/work","timestamp":"2026-08-28T10:00:00Z","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-28T10:00:01Z","message":{"role":"assistant","content":"world"}}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut roots = empty_roots(stores.path());
    roots.claude = stores.path().join("claude");
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;

    let direct_dir = tempfile::tempdir().unwrap();
    let direct = Archive::open(direct_dir.path()).unwrap();
    let pulled = pull_localhost(&direct, &roots, "host").unwrap();
    assert_eq!(pulled.conversations_extracted, 1);
    let lookup = direct
        .db()
        .unwrap()
        .lookup_session_by_orchestrator_id(SOURCE, THREAD)
        .unwrap()
        .unwrap();
    assert_eq!(lookup.session_native_id, provider_id);
    render_all(&direct).unwrap();
    let direct_views = tree_bytes(&direct.views_dir());
    let provider_view = direct_views
        .iter()
        .find(|(path, _)| path.ends_with(".claude.md"))
        .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        .unwrap();
    assert!(provider_view.contains("orchestrators:"));
    assert!(provider_view.contains(&format!("threadId: \"{THREAD}\"")));
    assert!(
        !direct_views
            .iter()
            .any(|(path, _)| path.ends_with(".t3.md"))
    );
    let direct_dump = direct.db().unwrap().content_dump().unwrap();
    rebuild(&direct).unwrap();
    render_all(&direct).unwrap();
    assert_eq!(direct.db().unwrap().content_dump().unwrap(), direct_dump);
    assert_eq!(tree_bytes(&direct.views_dir()), direct_views);

    let export_source = |source: &str| {
        let mut bytes = Vec::new();
        export(&mut bytes, &roots, "host", Some(source), None).unwrap();
        bytes
    };
    let claude_bundle = export_source("claude");
    let t3_bundle = export_source("t3");
    for bundles in [
        [&t3_bundle[..], &claude_bundle[..]],
        [&claude_bundle[..], &t3_bundle[..]],
    ] {
        let archive_dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(archive_dir.path()).unwrap();
        for bundle in bundles {
            let mut db = archive.db().unwrap();
            let ingested = ingest_bundle(bundle, &archive, &mut db).unwrap();
            for ((host, source), bookmark) in ingested.bookmarks {
                db.advance_bookmark(&host, &source, bookmark).unwrap();
            }
        }
        assert!(
            archive
                .db()
                .unwrap()
                .lookup_session_by_orchestrator_id(SOURCE, THREAD)
                .unwrap()
                .is_some()
        );
        align_capture_times(&direct, &archive);
        render_all(&archive).unwrap();
        assert_eq!(archive.db().unwrap().content_dump().unwrap(), direct_dump);
        assert_eq!(tree_bytes(&archive.views_dir()), direct_views);
    }
}

#[test]
fn t3_recovered_dialog_and_provenance_survive_rebuild_and_render() {
    let stores = tempfile::tempdir().unwrap();
    let writer = fixture(stores.path());
    writer.execute("INSERT INTO orchestration_v2_projection_messages(message_id,thread_id,created_at,updated_at,payload_json,role,streaming)
        VALUES ('response',?1,'2026-08-28T11:00:00Z','2026-08-28T11:00:00Z','{\"text\":\"Exact assistant text 🦀\",\"attachments\":[]}','assistant',0)", [THREAD]).unwrap();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut roots = empty_roots(stores.path());
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;
    pull_localhost(&archive, &roots, "host").unwrap();
    let db = archive.db().unwrap();
    let rows = db.sessions_with_dialog().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "claude");
    assert_eq!(rows[0].native_id, "33333333-3333-4333-8333-333333333333");
    let via = rows[0].transcript_facts.recovered_via.as_ref().unwrap();
    assert_eq!(via.thread_id, THREAD);
    assert_eq!(via.host, "host");
    let messages = serde_json::to_value(db.messages(rows[0].id).unwrap()).unwrap();
    let captures = tree_bytes(&archive.captures_dir());
    render_all(&archive).unwrap();
    let views = tree_bytes(&archive.views_dir());
    let transcript = views
        .iter()
        .find(|(path, _)| path.ends_with(".claude.md"))
        .unwrap();
    let text = std::str::from_utf8(&transcript.1).unwrap();
    assert!(text.contains("session-recovered-via:"));
    assert!(text.contains("Exact assistant text 🦀"));
    drop(db);
    rebuild(&archive).unwrap();
    render_all(&archive).unwrap();
    assert_eq!(tree_bytes(&archive.views_dir()), views);
    assert_eq!(tree_bytes(&archive.captures_dir()), captures);
    let db = archive.db().unwrap();
    let row = db.sessions_with_dialog().unwrap().remove(0);
    assert_eq!(
        serde_json::to_value(db.messages(row.id).unwrap()).unwrap(),
        messages
    );
    assert_eq!(row.transcript_facts.recovered_via.as_ref(), Some(via));
}

#[path = "t3/recovery_presence.rs"]
mod recovery_presence;
