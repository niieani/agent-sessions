use std::io::{BufRead, BufReader};
use std::process::Command;

use agent_sessions::adapters::cursor::{CursorAdapter, SOURCE};
use agent_sessions::adapters::{
    Adapter, EnumerateGate, RevisionCheck, SessionKind, revision_check,
};
use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::bundle::{export, ingest_bundle};
use agent_sessions::pull::pull_localhost;
use agent_sessions::render::render_transcript;
use rusqlite::{Connection, params};

mod common;
use common::{HOST, empty_roots};

const MAIN: &str = "11111111-1111-4111-8111-111111111111";
const CHILD: &str = "22222222-2222-4222-8222-222222222222";
const BIN: &str = env!("CARGO_BIN_EXE_agent-sessions");

fn create_store(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(
        "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
         CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
         CREATE TABLE composerHeaders (
           composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER,
           lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER,
           recency INTEGER, checkpointAt INTEGER, value TEXT, futureColumn BLOB
         );",
    )
    .unwrap();
    // A future vendor timestamp is a clock-skew adversary: warm detection
    // must compare logical per-composer metadata, not filesystem time.
    seed_main(&conn, 2_000_000_000_000);
    seed_child(&conn);
    conn
}

fn live_store_fingerprints(path: &std::path::Path) -> Vec<(std::path::PathBuf, u64, u64)> {
    let file_name = path.file_name().unwrap().to_string_lossy();
    [
        path.to_path_buf(),
        path.with_file_name(format!("{file_name}-wal")),
        path.with_file_name(format!("{file_name}-shm")),
    ]
    .into_iter()
    .map(|path| {
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!("reading live SQLite companion {}: {error}", path.display())
        });
        let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        (path, bytes.len() as u64, hash)
    })
    .collect()
}

fn main_root(updated: i64, extra_header: bool) -> String {
    let mut headers = vec![
        "z-user",
        "a-tool",
        "task-child",
        "m-assistant",
        "n-rich",
        "missing",
    ];
    if extra_header {
        headers.push("y-new");
    }
    serde_json::json!({
        "_v": 17,
        "name": "Pinned Cursor title 🚀",
        "createdAt": 1_772_966_400_000_i64,
        "lastUpdatedAt": updated,
        "workspaceIdentifier": {"configPath": {"path": "/work/demo.code-workspace"}},
        "subagentComposerIds": [CHILD],
        "conversation": [{"type":1,"text":"wrong inline copy"}],
        "fullConversationHeadersOnly": headers.into_iter().map(|bubble_id| serde_json::json!({"bubbleId": bubble_id})).collect::<Vec<_>>()
    })
    .to_string()
}

fn seed_main(conn: &Connection, updated: i64) {
    conn.execute(
        "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
        params![format!("composerData:{MAIN}"), main_root(updated, false)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO composerHeaders
         (composerId,workspaceId,createdAt,lastUpdatedAt,isArchived,isSubagent,recency,checkpointAt,value,futureColumn)
         VALUES (?1,'workspace',?2,?3,0,0,?3,NULL,?4,CAST(?5 AS TEXT))",
        params![MAIN, 1_772_966_400_000_i64, updated, "{\"native\":true}", &[0xff_u8, 0x01]],
    )
    .unwrap();
    let bubbles = [
        (
            "z-user",
            serde_json::json!({"_v":3,"type":1,"text":"ship it","createdAt":"2026-03-08T10:00:01.000Z"}).to_string(),
        ),
        (
            "a-tool",
            serde_json::json!({"_v":3,"type":2,"text":"","toolFormerData":{"name":"run","result":"ok"},"createdAt":"2026-03-08T10:00:02.000Z"}).to_string(),
        ),
        (
            "task-child",
            serde_json::json!({"_v":3,"type":2,"text":"","toolFormerData":{"name":"task_v2","toolCallId":"Task_1","params":{"description":"Map child","subagentType":"explore","prompt":"Inspect the child task."},"result":{"agentId":CHILD}},"createdAt":"2026-03-08T10:00:02.500Z"}).to_string(),
        ),
        (
            "m-assistant",
            serde_json::json!({"_v":3,"type":2,"text":"done","createdAt":1_772_966_403_000_i64}).to_string(),
        ),
        (
            "n-rich",
            serde_json::json!({"_v":3,"type":2,"text":"","richText":"{\"type\":\"doc\",\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"rich answer\"}]}]}","createdAt":"2026-03-08T10:00:04.000Z"}).to_string(),
        ),
        (
            "b-orphan",
            serde_json::json!({"_v":3,"type":2,"text":"","toolFormerData":{"name":"task_v2","toolCallId":"Task_1","params":{"description":"Stale duplicate","subagentType":"generalPurpose","prompt":"Do not use this stale prompt."},"result":{"agentId":CHILD}},"createdAt":"2026-03-08T10:00:03.000Z"}).to_string(),
        ),
    ];
    for (id, value) in bubbles {
        conn.execute(
            "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
            params![format!("bubbleId:{MAIN}:{id}"), value],
        )
        .unwrap();
    }
    let binary = [0xff_u8, 0x00, 0x80];
    conn.execute(
        "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
        params![format!("bubbleId:{MAIN}:x-binary"), &binary],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cursorDiskKV(key,value) VALUES (?1,NULL)",
        [format!("bubbleId:{MAIN}:x-null")],
    )
    .unwrap();
}

fn seed_child(conn: &Connection) {
    let root = serde_json::json!({
        "_v": 17,
        "createdAt": 1_772_966_500_000_i64,
        "lastUpdatedAt": 1_772_966_501_000_i64,
        "subagentInfo": {"parentComposerId": MAIN, "toolCallId": "Task_1", "subagentTypeName": "explore"},
        "fullConversationHeadersOnly": [{"bubbleId":"child-user"}, {"bubbleId":"child-answer"}]
    })
    .to_string();
    conn.execute(
        "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
        params![format!("composerData:{CHILD}"), root],
    )
    .unwrap();
    for (id, value) in [
        ("child-user", serde_json::json!({"type":1,"text":"Inspect the child task.","createdAt":1_772_966_500_100_i64}).to_string()),
        ("child-answer", serde_json::json!({"type":2,"text":"Child result.","createdAt":1_772_966_500_200_i64}).to_string()),
    ] {
        conn.execute(
            "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
            params![format!("bubbleId:{CHILD}:{id}"), value],
        ).unwrap();
    }
    conn.execute(
        "INSERT INTO composerHeaders
         (composerId,workspaceId,createdAt,lastUpdatedAt,isArchived,isSubagent)
         VALUES (?1,'workspace',?2,?3,0,1)",
        params![CHILD, 1_772_966_500_000_i64, 1_772_966_501_000_i64],
    )
    .unwrap();
}

fn rewrite_main_without_task(conn: &Connection, retain_composer_fallback: bool) {
    let mut root: serde_json::Value =
        serde_json::from_str(&main_root(2_000_000_000_000, false)).unwrap();
    if !retain_composer_fallback {
        root.as_object_mut().unwrap().remove("subagentComposerIds");
    }
    root["fullConversationHeadersOnly"]
        .as_array_mut()
        .unwrap()
        .retain(|header| header["bubbleId"] != "task-child");
    conn.execute(
        "UPDATE cursorDiskKV SET value = ?1 WHERE key = ?2",
        params![root.to_string(), format!("composerData:{MAIN}")],
    )
    .unwrap();
    for bubble_id in ["task-child", "b-orphan"] {
        conn.execute(
            "DELETE FROM cursorDiskKV WHERE key = ?1",
            params![format!("bubbleId:{MAIN}:{bubble_id}")],
        )
        .unwrap();
    }
}

#[test]
fn read_only_wal_capture_extract_and_warm_noop() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store); // keep WAL owner open during pull
    writer
        .execute("INSERT INTO ItemTable VALUES ('live-writer','yes')", [])
        .unwrap();
    let native_before = live_store_fingerprints(&store);

    // Run through the CLI in a separate process: otherwise this test's WAL
    // writer shares SQLite's process-local SHM mapping with the reader and
    // masks whether the reader opened its own mapping read-only.
    let subprocess_archive = Archive::open(&temp.path().join("subprocess-archive")).unwrap();
    let missing = temp.path().join("missing-store");
    std::fs::write(
        subprocess_archive.root().join("config.toml"),
        format!(
            r#"[[host]]
name = "{HOST}"
claude-root = {missing:?}
codex-root = {missing:?}
pi-root = {missing:?}
cursor-root = {store:?}
"#,
        ),
    )
    .unwrap();
    let output = Command::new(BIN)
        .env("HOME", temp.path())
        .args([
            "pull",
            "--archive",
            subprocess_archive.root().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(&format!("bubbleId:{MAIN}:x-null")),
        "typed NULL bubble is a tombstone, not malformed JSON: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(subprocess_archive.db().unwrap().session_count().unwrap(), 2);
    assert_eq!(
        live_store_fingerprints(&store),
        native_before,
        "Cursor reads must not mutate the live database, WAL, or WAL index"
    );

    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let mut roots = empty_roots(temp.path());
    roots.cursor = store.clone();
    let stats = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(stats.sessions_found, 2);
    assert_eq!(stats.sessions_captured, 2);
    assert_eq!(stats.conversations_extracted, 2);

    let db = archive.db().unwrap();
    assert_eq!(db.session_count().unwrap(), 2);
    let child = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == CHILD)
        .unwrap();
    assert_eq!(child.subagent_parent.unwrap().parent_native_id, MAIN);
    let link = db.subagent_links(SOURCE, MAIN).unwrap().pop().unwrap().link;
    assert_eq!(link.child_native_id, CHILD);
    assert_eq!(link.description.as_deref(), Some("Map child"));
    assert_eq!(
        link.instructions.as_deref(),
        Some("Inspect the child task.")
    );
    assert!(link.spawned_at_ms.is_some());
    let dump = db.content_dump().unwrap();
    assert_eq!(
        dump.matches("link_key=Some(\"Task_1\")").count(),
        2,
        "one parent reference and one child fact retain the authoritative task key"
    );
    assert!(dump.contains("Pinned Cursor title"));
    assert!(dump.contains("ship it"));
    assert!(dump.contains("done"));
    assert!(dump.contains("rich answer"));
    assert!(!dump.contains("superseded duplicate"));
    assert!(!dump.contains("wrong inline copy"));
    assert!(dump.contains("actions_before=2"));
    assert!(dump.contains("project_path=Some(\"/work/demo\")"));
    drop(db);

    let warm = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(warm.sessions_found, 0);

    let body = archive
        .captures_dir()
        .join(HOST)
        .join(SOURCE)
        .join(format!("{MAIN}.ndjson.zst"));
    let decoder = zstd::stream::read::Decoder::new(std::fs::File::open(&body).unwrap()).unwrap();
    let rows: Vec<serde_json::Value> = BufReader::new(decoder)
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    assert_eq!(rows[0]["table"], "cursorDiskKV");
    assert_eq!(rows[0]["valueType"], "text");
    assert_eq!(rows[1]["table"], "composerHeaders");
    let header: serde_json::Value =
        serde_json::from_str(rows[1]["value"].as_str().unwrap()).unwrap();
    assert_eq!(header["futureColumn"]["valueType"], "text");
    assert_eq!(header["futureColumn"]["encoding"], "base64");
    assert_eq!(
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            header["futureColumn"]["value"].as_str().unwrap(),
        )
        .unwrap(),
        [0xff, 0x01]
    );
    let binary = rows
        .iter()
        .find(|row| row["key"].as_str().unwrap().ends_with("x-binary"))
        .unwrap();
    assert_eq!(binary["valueType"], "blob");
    assert_eq!(binary["encoding"], "base64");

    // Bookmark alone is not enough locally: losing an old Capture must make
    // that composer visible again even though its lastUpdatedAt is unchanged.
    std::fs::remove_file(&body).unwrap();
    std::fs::create_dir(&body).unwrap();
    let decoy = body.with_file_name(format!("{MAIN}.decoy.zst"));
    std::fs::write(&decoy, b"not a Capture").unwrap();
    let gate = EnumerateGate::load(
        &archive.captures_dir(),
        HOST,
        SOURCE,
        archive.db().unwrap().bookmark(HOST, SOURCE).unwrap(),
    )
    .unwrap();
    let refs = CursorAdapter.enumerate(&store, &gate).unwrap();
    assert!(refs.iter().any(|session| session.native_id == MAIN));
    std::fs::remove_dir(&body).unwrap();
    std::fs::remove_file(decoy).unwrap();
    let repaired = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(repaired.sessions_found, 1);
    assert_eq!(repaired.sessions_captured, 1);

    // Header-only state participates in the logical discriminator. No root
    // bytes or timestamps change, yet classification and Capture refresh.
    writer
        .execute(
            "UPDATE composerHeaders SET isSubagent=1 WHERE composerId=?1",
            [MAIN],
        )
        .unwrap();
    let header_changed = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(header_changed.sessions_captured, 1);
    assert_eq!(header_changed.conversations_extracted, 0);
    assert_eq!(
        archive.db().unwrap().session_count().unwrap(),
        1,
        "a newly non-extractable winner retracts only its stale Conversation"
    );
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .iter()
            .all(|session| session.native_id != MAIN)
    );
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            archive
                .captures_dir()
                .join(HOST)
                .join(SOURCE)
                .join(format!("{MAIN}.meta.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["kind"], "subagent");
}

#[test]
fn cursor_subagent_fallback_and_child_only_recovery_are_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store);
    rewrite_main_without_task(&writer, true);

    let archive = Archive::open(&temp.path().join("fallback-archive")).unwrap();
    let mut roots = empty_roots(temp.path());
    roots.cursor = store.clone();
    assert_eq!(
        pull_localhost(&archive, &roots, HOST)
            .unwrap()
            .conversations_extracted,
        2
    );
    let db = archive.db().unwrap();
    let fallback = db.subagent_links(SOURCE, MAIN).unwrap().pop().unwrap().link;
    assert_eq!(fallback.child_native_id, CHILD);
    assert_eq!(fallback.description, None);
    assert_eq!(fallback.spawned_at_ms, None);
    drop(db);

    let child_only = Archive::open(&temp.path().join("child-only-archive")).unwrap();
    rewrite_main_without_task(&writer, false);
    assert_eq!(
        pull_localhost(&child_only, &roots, HOST)
            .unwrap()
            .conversations_extracted,
        2
    );
    let db = child_only.db().unwrap();
    assert!(db.subagent_links(SOURCE, MAIN).unwrap().is_empty());
    let child = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == CHILD)
        .unwrap();
    let transcript = render_transcript(&child, &db.messages(child.id).unwrap());
    assert!(transcript.contains("\n\nInspect the child task.\n\n</parent>"));
    assert!(!transcript.contains("<subagent>"));
}

#[test]
fn cursor_subagent_without_authoritative_dialog_remains_capture_only() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store);
    writer
        .execute(
            "DELETE FROM cursorDiskKV WHERE key GLOB ?1",
            params![format!("bubbleId:{CHILD}:*")],
        )
        .unwrap();
    writer
        .execute(
            "UPDATE cursorDiskKV SET value = ?1 WHERE key = ?2",
            params![
                serde_json::json!({
                    "_v": 17,
                    "createdAt": 1_772_966_500_000_i64,
                    "lastUpdatedAt": 1_772_966_501_000_i64,
                    "subagentInfo": {
                        "parentComposerId": MAIN,
                        "toolCallId": "Task_1",
                        "subagentTypeName": "explore"
                    },
                    "fullConversationHeadersOnly": []
                })
                .to_string(),
                format!("composerData:{CHILD}")
            ],
        )
        .unwrap();

    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let mut roots = empty_roots(temp.path());
    roots.cursor = store;
    let stats = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(stats.sessions_captured, 2);
    assert_eq!(stats.conversations_extracted, 1);
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 1);
}

#[test]
fn non_uuid_composer_ids_are_captured_under_safe_paths_and_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store);
    for (id, root) in [
        (
            "empty-state-draft",
            serde_json::json!({
                "createdAt": 1_000,
                "lastUpdatedAt": 2_000,
                "conversation": [{"type": 1, "text": "draft prompt"}],
            })
            .to_string(),
        ),
        (
            "../../escape",
            serde_json::json!({"createdAt": 1_000, "lastUpdatedAt": 2_000}).to_string(),
        ),
    ] {
        writer
            .execute(
                "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
                params![format!("composerData:{id}"), root],
            )
            .unwrap();
    }
    let long_id = format!("../../{}", "long-id".repeat(50));
    writer
        .execute(
            "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
            params![
                format!("composerData:{long_id}"),
                serde_json::json!({
                    "createdAt": 1_000,
                    "lastUpdatedAt": 2_000,
                    "conversation": [{"type": 1, "text": "long identity prompt"}],
                })
                .to_string(),
            ],
        )
        .unwrap();
    let refs = CursorAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    assert!(
        refs.iter()
            .any(|session| session.native_id == "empty-state-draft")
    );
    assert!(
        refs.iter()
            .any(|session| session.native_id == "../../escape")
    );
    assert!(refs.iter().any(|session| session.native_id == long_id));
    assert_eq!(
        refs.iter()
            .find(|session| session.native_id == CHILD)
            .unwrap()
            .kind,
        SessionKind::Subagent
    );

    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let mut roots = empty_roots(temp.path());
    roots.cursor = store.clone();
    let cold = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(cold.sessions_found, 5);
    assert_eq!(cold.sessions_captured, 5);
    assert_eq!(cold.conversations_extracted, 5);

    let captures = archive.captures_dir().join(HOST).join(SOURCE);
    let draft_body = captures.join("id-656d7074792d73746174652d6472616674.ndjson.zst");
    assert!(draft_body.is_file());
    let draft_meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            draft_body.with_file_name("id-656d7074792d73746174652d6472616674.meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(draft_meta["nativeId"], "empty-state-draft");
    let decoder =
        zstd::stream::read::Decoder::new(std::fs::File::open(&draft_body).unwrap()).unwrap();
    let first: serde_json::Value =
        serde_json::from_str(&BufReader::new(decoder).lines().next().unwrap().unwrap()).unwrap();
    assert_eq!(first["key"], "composerData:empty-state-draft");
    let long_meta = std::fs::read_dir(&captures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("id-sha256-") && name.ends_with(".meta.json"))
        })
        .expect("bounded long-id sidecar");
    assert!(long_meta.file_name().unwrap().len() <= 255);
    let long_meta_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(long_meta).unwrap()).unwrap();
    assert_eq!(long_meta_json["nativeId"], long_id);
    assert!(!temp.path().join("escape.ndjson.zst").exists());
    assert_eq!(
        pull_localhost(&archive, &roots, HOST)
            .unwrap()
            .sessions_found,
        0
    );

    let mut bundle = Vec::new();
    let exported = export(&mut bundle, &roots, HOST, Some(SOURCE), None).unwrap();
    assert_eq!(exported.sessions_found, 5);
    let central = Archive::open(&temp.path().join("central")).unwrap();
    let mut db = central.db().unwrap();
    let ingested = ingest_bundle(&bundle[..], &central, &mut db).unwrap();
    assert_eq!(ingested.stats.units_in_bundle, 5);
    assert_eq!(ingested.stats.conversations_extracted, 5);
    assert!(
        central
            .captures_dir()
            .join(HOST)
            .join(SOURCE)
            .join("id-2e2e2f2e2e2f657363617065.ndjson.zst")
            .is_file()
    );

    let main = refs
        .iter()
        .find(|session| session.native_id == MAIN)
        .unwrap();
    let broken_captures = temp.path().join("broken-captures");
    let body = broken_captures
        .join(HOST)
        .join(SOURCE)
        .join(format!("{MAIN}.ndjson.zst"));
    std::fs::create_dir_all(&body).unwrap(); // force file -> directory rename failure
    assert!(CursorAdapter.capture(main, &broken_captures, HOST).is_err());
    assert!(
        !body
            .with_file_name(format!("{MAIN}.ndjson.zst.tmp"))
            .exists()
    );
}

#[test]
fn growth_rebuild_and_cursor_shrink_supersede() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store);
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let mut roots = empty_roots(temp.path());
    roots.cursor = store.clone();
    pull_localhost(&archive, &roots, HOST).unwrap();

    let updated = 2_000_000_500_000_i64;
    writer
        .execute(
            "UPDATE cursorDiskKV SET value=?1 WHERE key=?2",
            params![main_root(updated, true), format!("composerData:{MAIN}")],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",
            params![
                format!("bubbleId:{MAIN}:y-new"),
                serde_json::json!({"type":1,"text":"one more","createdAt":updated}).to_string()
            ],
        )
        .unwrap();
    let grown = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(grown.sessions_captured, 1);
    assert!(
        archive
            .db()
            .unwrap()
            .content_dump()
            .unwrap()
            .contains("one more")
    );
    assert_eq!(
        archive
            .db()
            .unwrap()
            .subagent_links(SOURCE, MAIN)
            .unwrap()
            .len(),
        1,
        "rebuild resolves the same Cursor child edge from Capture facts"
    );

    std::fs::remove_file(archive.db_path()).unwrap();
    let rebuilt = rebuild(&archive).unwrap();
    assert_eq!(rebuilt.conversations_extracted, 2);
    assert!(
        archive
            .db()
            .unwrap()
            .content_dump()
            .unwrap()
            .contains("one more")
    );

    let meta = agent_sessions::adapters::CaptureMeta {
        repository_resolution: None,
        schema_version: 1,
        source: SOURCE.into(),
        native_id: MAIN.into(),
        host: HOST.into(),
        native_path: store.to_string_lossy().into_owned(),
        project_dir: None,
        kind: agent_sessions::adapters::SessionKind::Main,
        source_mtime_ms: 10,
        source_size: 500,
        source_files: Vec::new(),
        native_meta_mtime_ms: None,
        native_meta_size: None,
        captured_at_ms: 10,
        adapter_version: 1,
        superseded_native_ids: Vec::new(),
        needs_retry: false,
        native_meta: None,
        native_birth_time_ms: None,
        goal_attachments: Vec::new(),
    };
    assert_eq!(revision_check(&meta, 11, 100), RevisionCheck::Changed);
    assert_eq!(revision_check(&meta, 10, 500), RevisionCheck::Unchanged);
    assert_eq!(
        revision_check(&meta, 10, 501),
        RevisionCheck::Changed,
        "equal revision uses the deterministic discriminator"
    );
    assert_eq!(revision_check(&meta, 9, 900), RevisionCheck::Stale);
}

#[test]
fn cursor_bundle_roundtrip_and_sql_uses_range_search() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let writer = create_store(&store);
    let mut roots = empty_roots(temp.path());
    roots.cursor = store.clone();

    let mut bundle = Vec::new();
    let exported = export(&mut bundle, &roots, HOST, Some(SOURCE), None).unwrap();
    assert_eq!(exported.sessions_found, 2);
    let archive = Archive::open(&temp.path().join("central")).unwrap();
    let mut db = archive.db().unwrap();
    let ingested = ingest_bundle(&bundle[..], &archive, &mut db).unwrap();
    assert_eq!(ingested.stats.units_in_bundle, 2);
    assert_eq!(ingested.stats.conversations_extracted, 2);
    assert!(db.content_dump().unwrap().contains("rich answer"));
    assert_eq!(db.subagent_links(SOURCE, MAIN).unwrap().len(), 1);

    let mut plan = writer
        .prepare(
            "EXPLAIN QUERY PLAN SELECT key FROM cursorDiskKV
             WHERE key >= 'composerData:' AND key < 'composerData;'",
        )
        .unwrap();
    let detail: Vec<String> = plan
        .query_map([], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        detail.iter().any(|line| line.contains("SEARCH")),
        "{detail:?}"
    );

    let bookmark = ingested.bookmarks[&(HOST.to_string(), SOURCE.to_string())];
    let mut incremental = Vec::new();
    let conservative =
        export(&mut incremental, &roots, HOST, Some(SOURCE), Some(bookmark)).unwrap();
    assert_eq!(
        conservative.sessions_found, 2,
        "stateless exports re-cover header-bearing composers"
    );

    // Bundle ingestion has the same projection-retraction semantics as a
    // local pull. The Capture remains, but a header-only main → subagent
    // transition must remove the old standalone Conversation and messages.
    writer
        .execute(
            "UPDATE composerHeaders SET isSubagent=1 WHERE composerId=?1",
            [MAIN],
        )
        .unwrap();
    let mut reclassified_bundle = Vec::new();
    let reclassified_export = export(
        &mut reclassified_bundle,
        &roots,
        HOST,
        Some(SOURCE),
        Some(bookmark),
    )
    .unwrap();
    assert_eq!(reclassified_export.sessions_found, 2);
    let reclassified = ingest_bundle(&reclassified_bundle[..], &archive, &mut db).unwrap();
    assert_eq!(reclassified.stats.sessions_captured, 1);
    assert_eq!(reclassified.stats.conversations_extracted, 1);
    assert_eq!(db.session_count().unwrap(), 1);
    let dump = db.content_dump().unwrap();
    assert!(!dump.contains("rich answer"));
}

#[test]
fn inline_legacy_and_lexical_fallback_extract() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let conn = Connection::open(&store).unwrap();
    conn.execute_batch(
        "CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
    )
    .unwrap();
    let id = "33333333-3333-4333-8333-333333333333";
    let root = serde_json::json!({
        "createdAt": 1000,
        "lastUpdatedAt": 2000,
        "conversation": [
            {"type":1,"richText":"{\"root\":{\"children\":[{\"type\":\"text\",\"text\":\"legacy prompt\"}]}}","createdAt":1000,"context":{"folderSelections":["/inline/project"]}},
            {"type":2,"markdown":"legacy answer","updatedAt":"2026-03-08T10:00:00Z"}
        ]
    });
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1,?2)",
        params![format!("composerData:{id}"), root.to_string()],
    )
    .unwrap();
    let refs = CursorAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let capture = CursorAdapter
        .capture(&refs[0], &temp.path().join("captures"), HOST)
        .unwrap();
    let conv = CursorAdapter.extract(&capture).unwrap().unwrap();
    assert_eq!(conv.title, None, "message text is not promoted to a title");
    assert_eq!(conv.messages[0].text, "legacy prompt");
    assert_eq!(conv.messages[1].text, "legacy answer");
    assert_eq!(conv.project_path.as_deref(), Some("/inline/project"));
}

#[test]
fn scan_fallback_orders_by_time_and_incremental_keeps_unrevisioned_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let conn = Connection::open(&store).unwrap();
    conn.execute(
        "CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
        [],
    )
    .unwrap();
    let scan_id = "44444444-4444-4444-8444-444444444444";
    let no_revision_id = "55555555-5555-4555-8555-555555555555";
    let bad_revision_id = "66666666-6666-4666-8666-666666666666";
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1,?2)",
        params![format!("composerData:{scan_id}"), r#"{"createdAt":9000}"#],
    )
    .unwrap();
    for (key, value) in [
        (
            "a-late",
            serde_json::json!({"type":1,"text":"late","createdAt":3000}),
        ),
        (
            "z-early",
            serde_json::json!({"type":1,"text":"early","createdAt":1000}),
        ),
        ("m-none", serde_json::json!({"type":2,"text":"untimed"})),
    ] {
        conn.execute(
            "INSERT INTO cursorDiskKV VALUES (?1,?2)",
            params![format!("bubbleId:{scan_id}:{key}"), value.to_string()],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1,'{}')",
        [format!("composerData:{no_revision_id}")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES (?1,?2)",
        params![
            format!("composerData:{bad_revision_id}"),
            r#"{"lastUpdatedAt":"not-a-time","createdAt":10000}"#
        ],
    )
    .unwrap();

    let refs = CursorAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let scan = refs
        .iter()
        .find(|session| session.native_id == scan_id)
        .unwrap();
    let capture = CursorAdapter
        .capture(scan, &temp.path().join("captures"), HOST)
        .unwrap();
    let conv = CursorAdapter.extract(&capture).unwrap().unwrap();
    assert_eq!(
        conv.messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["early", "late", "untimed"]
    );

    let mut roots = empty_roots(temp.path());
    roots.cursor = store;
    let incremental_refs = CursorAdapter
        .enumerate(&roots.cursor, &EnumerateGate::bookmark_only(Some(5_000)))
        .unwrap();
    assert_eq!(
        incremental_refs
            .iter()
            .map(|session| session.native_id.as_str())
            .collect::<Vec<_>>(),
        [scan_id, no_revision_id, bad_revision_id]
    );
    let mut bundle = Vec::new();
    let stats = export(&mut bundle, &roots, HOST, Some(SOURCE), Some(5_000)).unwrap();
    assert_eq!(
        stats.sessions_found, 3,
        "createdAt fallback + unrevisioned/invalid roots"
    );
}

#[test]
fn non_uuid_cursor_candidate_resolves_using_the_writers_sidecar_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("state.vscdb");
    let conn = create_store(&store);
    let id = "composer:non-uuid/opaque";
    conn.execute("INSERT INTO cursorDiskKV(key,value) VALUES (?1,?2)",params![format!("composerData:{id}"),serde_json::json!({"createdAt":1_772_966_400_000_i64,"lastUpdatedAt":2_000_000_000_000_i64,"workspaceIdentifier":{"configPath":{"path":tmp.path()}},"conversation":[{"type":1,"text":"Resolve this repository"}]}).to_string()]).unwrap();
    let archive = Archive::open(&tmp.path().join("archive")).unwrap();
    let mut roots = empty_roots(tmp.path());
    roots.cursor = store;
    let stats = pull_localhost(&archive, &roots, HOST).unwrap();
    assert_eq!(stats.sessions_failed, 0);
    assert!(stats.repository_refresh.is_empty(), "{stats:?}");
    let refs = CursorAdapter
        .enumerate(&roots.cursor, &EnumerateGate::default())
        .unwrap();
    let capture = CursorAdapter
        .capture(
            refs.iter().find(|r| r.native_id == id).unwrap(),
            &archive.captures_dir(),
            HOST,
        )
        .unwrap();
    let paths =
        agent_sessions::adapters::capture_paths(&archive.captures_dir(), HOST, SOURCE, id).unwrap();
    assert_eq!(capture.meta_path, paths.meta);
    let meta: agent_sessions::adapters::CaptureMeta =
        serde_json::from_slice(&std::fs::read(paths.meta).unwrap()).unwrap();
    assert!(meta.repository_resolution.is_some());
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .iter()
            .find(|s| s.native_id == id)
            .unwrap()
            .repository_resolution
            .is_some()
    );
}

#[test]
fn changed_composer_keeps_previous_repository_observation_for_retry() {
    use agent_sessions::repository_resolution::{Fact, Lookup};
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("state.vscdb");
    let conn = create_store(&store);
    let captures = temp.path().join("captures");
    let refs = CursorAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let session = refs.iter().find(|r| r.native_id == MAIN).unwrap();
    let mut capture = CursorAdapter.capture(session, &captures, HOST).unwrap();
    let fact = Fact {
        source_revision: (capture.meta.source_mtime_ms, capture.meta.source_size),
        branch_recorded: true,
        path: "/work/repo".into(),
        lookup: Lookup::Missing,
        unresolved: true,
    };
    capture.meta.repository_resolution = Some(fact.clone());
    std::fs::write(
        &capture.meta_path,
        serde_json::to_vec(&capture.meta).unwrap(),
    )
    .unwrap();
    conn.execute(
        "UPDATE composerHeaders SET lastUpdatedAt=lastUpdatedAt+1 WHERE composerId=?1",
        [MAIN],
    )
    .unwrap();
    let capture = CursorAdapter.capture(session, &captures, HOST).unwrap();
    assert!(capture.written);
    assert_eq!(capture.meta.repository_resolution, Some(fact.clone()));
    assert_ne!(
        fact.source_revision,
        (capture.meta.source_mtime_ms, capture.meta.source_size)
    );
}

#[test]
fn admission_has_no_working_path_in_native_or_capture() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.vscdb");
    let _conn = create_store(&path);
    let sessions = CursorAdapter
        .enumerate(&path, &EnumerateGate::default())
        .unwrap();
    assert!(!sessions.is_empty());
    let dest = tempfile::tempdir().unwrap();
    for session in sessions {
        assert_eq!(CursorAdapter.session_working_path(&session).unwrap(), None);
        let capture = CursorAdapter.capture(&session, dest.path(), HOST).unwrap();
        assert_eq!(CursorAdapter.capture_working_path(&capture).unwrap(), None);
    }
}
