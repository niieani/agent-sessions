use std::io::{BufRead, BufReader};

use super::extract::link_fact;
use super::*;

const THREAD: &str = "11111111-1111-4111-8111-111111111111";
const ARCHIVED: &str = "22222222-2222-4222-8222-222222222222";

#[path = "../../../tests/fixtures/t3/support.rs"]
mod support;
use support::fixture;

fn captured_lines(capture: &Capture) -> Vec<Value> {
    let file = std::fs::File::open(&capture.body_path).unwrap();
    let decoder = zstd::stream::read::Decoder::new(file).unwrap();
    BufReader::new(decoder)
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect()
}

#[test]
fn enumerate_includes_archived_threads_and_hashes_exact_typed_values() {
    let dir = tempfile::tempdir().unwrap();
    let writer = fixture(dir.path());
    let before = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    assert_eq!(
        before
            .iter()
            .map(|reference| reference.native_id.as_str())
            .collect::<Vec<_>>(),
        [THREAD, ARCHIVED]
    );
    let old = before.iter().find(|item| item.native_id == THREAD).unwrap();
    writer
            .execute(
                "UPDATE orchestration_v2_projection_messages SET payload_json=json_set(payload_json,'$.text','jello') WHERE message_id='message'",
                [],
            )
            .unwrap();
    let after = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    let changed = after.iter().find(|item| item.native_id == THREAD).unwrap();
    assert_eq!(changed.mtime_ms, old.mtime_ms, "revision is timestamp-only");
    assert_ne!(
        changed.size, old.size,
        "same-size body change affects digest"
    );
}

#[test]
fn capture_is_ordered_typed_self_contained_and_store_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let writer = fixture(dir.path());
    let store = dir.path().join(STORE_PATH);
    let fingerprints = |path: &Path| {
        [
            path.to_path_buf(),
            path.with_extension("sqlite-wal"),
            path.with_extension("sqlite-shm"),
        ]
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            let bytes = std::fs::read(&path).ok()?;
            Some((path, metadata.len(), metadata.modified().ok(), bytes))
        })
        .collect::<Vec<_>>()
    };
    let refs = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    let before = fingerprints(&store);
    let capture = T3Adapter
        .capture(
            refs.iter().find(|item| item.native_id == THREAD).unwrap(),
            &dir.path().join("captures"),
            "host",
        )
        .unwrap();
    assert_eq!(fingerprints(&store), before);
    let lines = captured_lines(&capture);
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
    assert_eq!(attachment["record"], "attachment");
    assert_eq!(attachment["status"], "captured");
    assert_eq!(attachment["valueType"], "blob");
    let body = lines
        .iter()
        .filter(|line| line["record"] == "attachment-body")
        .flat_map(|line| {
            base64::engine::general_purpose::STANDARD
                .decode(line["value"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(body, b"hello");
    drop(writer);
}

#[test]
fn enumerate_reads_an_idle_wal_store_without_shm() {
    let dir = tempfile::tempdir().unwrap();
    drop(fixture(dir.path()));
    let store = dir.path().join(STORE_PATH);
    let _ = std::fs::remove_file(format!("{}-shm", store.display()));
    std::fs::write(format!("{}-wal", store.display()), []).unwrap();

    let refs = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap();
    assert_eq!(
        refs.iter()
            .map(|reference| reference.native_id.as_str())
            .collect::<Vec<_>>(),
        [THREAD, ARCHIVED]
    );
}

#[test]
fn bad_existing_root_and_incompatible_schema_fail_loudly() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        T3Adapter
            .enumerate(dir.path(), &EnumerateGate::default())
            .is_err()
    );
    std::fs::create_dir_all(dir.path().join("userdata")).unwrap();
    Connection::open(dir.path().join(STORE_PATH)).unwrap();
    let error = T3Adapter
        .enumerate(dir.path(), &EnumerateGate::default())
        .unwrap_err();
    assert!(format!("{error:#}").contains("projection_projects"));
}

#[test]
fn provider_cursor_registry_is_exact_and_fail_closed() {
    let runtime = |provider: &str, cursor: &str| {
        BTreeMap::from([
            ("provider_name".into(), Value::String(provider.into())),
            ("resume_cursor_json".into(), Value::String(cursor.into())),
        ])
    };
    let claude = link_fact(&runtime(
        "claudeAgent",
        r#"{"threadId":"11111111-1111-4111-8111-111111111111","resume":"33333333-3333-4333-8333-333333333333","resumeSessionAt":"22222222-2222-4222-8222-222222222222","turnCount":1}"#,
    ));
    assert_eq!(claude.status, OrchestratorLinkStatus::Candidate);
    assert_eq!(claude.provider_source.as_deref(), Some("claude"));

    let codex = link_fact(&runtime("codex", r#"{"threadId":"codex-thread"}"#));
    assert_eq!(codex.status, OrchestratorLinkStatus::Candidate);
    assert_eq!(codex.provider_native_id.as_deref(), Some("codex-thread"));

    assert_eq!(
        link_fact(&runtime("cursor", r#"{"schemaVersion":1,"sessionId":"x"}"#)).status,
        OrchestratorLinkStatus::Unsupported
    );
    assert_eq!(
        link_fact(&runtime("claudeAgent", "not-json")).status,
        OrchestratorLinkStatus::Malformed
    );
    assert_eq!(
        link_fact(&runtime("claudeAgent", r#"{"resume":""}"#)).status,
        OrchestratorLinkStatus::Malformed
    );
    assert_eq!(
        link_fact(&runtime(
            "claudeAgent",
            r#"{"resume":"33333333-3333-4333-8333-333333333333","schemaVersion":2}"#,
        ))
        .status,
        OrchestratorLinkStatus::Malformed
    );
}

#[test]
fn attachment_capture_rejects_unsafe_and_preserves_retryable_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let descriptor = |id: &str, size| AttachmentDescriptor {
        id: id.into(),
        kind: "file".into(),
        name: "note.txt".into(),
        mime_type: "text/plain".into(),
        size,
    };
    assert_eq!(
        inspect_attachment(dir.path(), &descriptor("../escape", 1))
            .0
            .status,
        "rejected"
    );
    assert_eq!(
        inspect_attachment(dir.path(), &descriptor("large", MAX_ATTACHMENT_BYTES + 1))
            .0
            .status,
        "oversize"
    );
    assert_eq!(
        inspect_attachment(dir.path(), &descriptor("missing", 1))
            .0
            .status,
        "missing"
    );
    std::fs::write(dir.path().join("wrong.txt"), b"two").unwrap();
    assert_eq!(
        inspect_attachment(dir.path(), &descriptor("wrong", 9))
            .0
            .status,
        "rejected"
    );

    std::fs::write(dir.path().join("body.txt"), b"streamed body").unwrap();
    let mut lines = Vec::new();
    let captured = write_captured_attachment(
        dir.path(),
        &descriptor("body", b"streamed body".len() as u64),
        &mut lines,
    )
    .unwrap();
    assert_eq!(captured.status, "captured");
    let records = lines
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0]["encoding"], "base64-chunks");
    assert_eq!(records[0]["valueType"], "blob");
    let body = records[1..]
        .iter()
        .flat_map(|record| {
            assert_eq!(record["valueType"], "blob");
            base64::engine::general_purpose::STANDARD
                .decode(record["value"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(body, b"streamed body");
}
