//! Slice 2 — archival core: Bookmark no-ops, growth (capture supersede +
//! record extension), and `rebuild` from Captures alone. All against TEMP
//! directories built from fixtures; real Stores are never touched.

mod common;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use agent_sessions::adapters::{Adapter, Capture, CaptureMeta, CaptureOutcome, EnumerateGate};
use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::pull::pull_localhost;
use agent_sessions::render::render_all;

use common::{HOST, empty_roots, pull, temp_store, tree_bytes};

const GROWN_ID: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ID: &str = "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef";

fn grown_session_path(store: &Path) -> PathBuf {
    store
        .join("-workspace-Projects-example")
        .join(format!("{GROWN_ID}.jsonl"))
}

/// Append native JSONL records to a Session file in the TEMP Store, after a
/// short sleep so the file's mtime demonstrably moves past the Bookmark.
fn append_records(store: &Path, records: &[&str]) {
    sleep(Duration::from_millis(25));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(grown_session_path(store))
        .unwrap();
    for record in records {
        writeln!(file, "{record}").unwrap();
    }
}

/// Set a file's mtime (TEMP Store copies only — never a native Store).
fn set_mtime(path: &Path, time: std::time::SystemTime) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn native_meta_changes_refresh_only_the_capture_sidecar() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let capture_dir = archive_dir
        .path()
        .join("captures")
        .join(HOST)
        .join("claude");
    let body = capture_dir.join(format!("{CHILD_ID}.jsonl.zst"));
    let sidecar = capture_dir.join(format!("{CHILD_ID}.meta.json"));
    let body_before = fs::read(&body).unwrap();

    // Old sidecars had nativeMeta but no revision fields. They are refreshed
    // compatibly without recompressing the unchanged JSONL body.
    let mut old: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("nativeMetaMtimeMs");
    old.as_object_mut().unwrap().remove("nativeMetaSize");
    fs::write(&sidecar, serde_json::to_string_pretty(&old).unwrap()).unwrap();
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1);
    assert_eq!(stats.sessions_captured, 1);
    assert_eq!(fs::read(&body).unwrap(), body_before);

    sleep(Duration::from_millis(25));
    let native_meta = store
        .path()
        .join("-workspace-Projects-example")
        .join(format!("{CHILD_ID}.meta.json"));
    fs::write(
        &native_meta,
        r#"{"agentType":"Explore","description":"Map codebase","toolUseId":"toolu_01AAA","spawnDepth":2}"#,
    )
    .unwrap();
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "meta-only revision is enumerable");
    assert_eq!(stats.sessions_captured, 1, "sidecar revision is captured");
    assert_eq!(
        fs::read(&body).unwrap(),
        body_before,
        "body not recompressed"
    );
    let refreshed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(refreshed["nativeMeta"]["spawnDepth"], 2);
    assert!(refreshed["nativeMetaMtimeMs"].is_number());

    let archive = Archive::open(archive_dir.path()).unwrap();
    let child = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == CHILD_ID)
        .unwrap();
    assert_eq!(child.subagent_parent.unwrap().depth, Some(2));
    assert_eq!(pull(store.path(), archive_dir.path()).sessions_found, 0);
}

#[test]
fn rebuild_restores_winning_capture_time_from_the_sidecar() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let sidecar = archive_dir
        .path()
        .join(format!("captures/{HOST}/claude/{GROWN_ID}.meta.json"));
    let meta: CaptureMeta = serde_json::from_str(&fs::read_to_string(sidecar).unwrap()).unwrap();

    let archive = Archive::open(archive_dir.path()).unwrap();
    rebuild(&archive).unwrap();
    let row = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == GROWN_ID)
        .unwrap();
    assert_eq!(row.captured_at_ms, meta.captured_at_ms);
}

const APPENDED_DIALOG: [&str; 2] = [
    r#"{"type":"user","message":{"role":"user","content":"Also add YAML support"},"uuid":"00000000-0000-4000-8000-00000000000a","timestamp":"2026-03-05T15:00:00.000Z","cwd":"/workspace/Projects/example","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
    r#"{"type":"assistant","requestId":"req_005","message":{"id":"msg_006","type":"message","role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"YAML support added."}],"stop_reason":"end_turn"},"uuid":"00000000-0000-4000-8000-00000000000b","timestamp":"2026-03-05T15:00:30.000Z","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
];

#[test]
fn capture_restats_after_enumeration_before_claiming_size() {
    let store = temp_store();
    let refs = agent_sessions::adapters::claude::ClaudeAdapter
        .enumerate(store.path(), &EnumerateGate::default())
        .unwrap();
    let session = refs.iter().find(|r| r.native_id == GROWN_ID).unwrap();
    append_records(store.path(), &APPENDED_DIALOG);

    let captures = tempfile::tempdir().unwrap();
    let capture = agent_sessions::adapters::claude::ClaudeAdapter
        .capture(session, captures.path(), HOST)
        .unwrap();
    assert!(
        capture.meta_path.is_file(),
        "Provenance is continuously present"
    );
    assert!(
        capture.retry_pending(),
        "new Capture is retry-marked before Extract/upsert can begin"
    );
    let native = fs::read(grown_session_path(store.path())).unwrap();
    let body = zstd::stream::decode_all(fs::File::open(capture.body_path).unwrap()).unwrap();
    assert_eq!(body, native);
    assert_eq!(capture.meta.source_size, body.len() as u64);
}

#[test]
fn stale_capture_temps_are_swept_recursively() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let nested = archive.captures_dir().join("host/claude/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("body.zst.tmp"), b"partial").unwrap();
    fs::write(nested.join("session.meta.json.tmp"), b"partial").unwrap();
    fs::write(nested.join("keep.zst"), b"capture").unwrap();
    assert_eq!(archive.sweep_capture_temps().unwrap(), 2);
    assert!(nested.join("keep.zst").is_file());
    assert!(!nested.join("body.zst.tmp").exists());
}

#[test]
fn archive_writer_lock_fails_fast_and_releases_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let held = archive.acquire_writer_lock().unwrap();

    let error = match archive.acquire_writer_lock() {
        Ok(_) => panic!("a second archive writer lock must fail"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("another writer active"),
        "unexpected lock error: {error:#}"
    );

    drop(held);
    archive.acquire_writer_lock().unwrap();
}

#[test]
fn mutating_workflows_reject_an_active_archive_writer() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let roots = empty_roots(dir.path());
    let held = archive.acquire_writer_lock().unwrap();

    for error in [
        pull_localhost(&archive, &roots, HOST).unwrap_err(),
        rebuild(&archive).unwrap_err(),
        render_all(&archive).unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("another writer active"),
            "unexpected lock error: {error:#}"
        );
    }

    drop(held);
    assert_eq!(
        pull_localhost(&archive, &roots, HOST)
            .unwrap()
            .sessions_found,
        0
    );
}

#[test]
fn failed_capture_retry_marker_defeats_an_existing_bookmark() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let sidecar = archive
        .captures_dir()
        .join(HOST)
        .join("claude")
        .join(format!("{GROWN_ID}.meta.json"));
    let meta: CaptureMeta = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    let mut capture = Capture {
        body_path: sidecar.with_file_name(format!("{GROWN_ID}.jsonl.zst")),
        meta_path: sidecar.clone(),
        meta,
        written: false,
        outcome: CaptureOutcome::Unchanged,
    };
    capture.set_needs_retry(true).unwrap();
    assert!(sidecar.is_file(), "retry marker never removes Provenance");
    assert!(
        capture.retry_pending(),
        "retry obligation is durable before Extract/upsert starts"
    );

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(
        stats.sessions_found, 1,
        "retry marker defeats exact sidecar skip"
    );
    assert_eq!(
        stats.sessions_unchanged, 1,
        "durable Capture body is reused"
    );
    let repaired: serde_json::Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(
        repaired.get("needsRetry"),
        None,
        "successful retry clears marker"
    );
    assert!(
        !capture.retry_pending(),
        "successful retry removes sentinel"
    );

    let mut legacy = repaired;
    legacy["needsRetry"] = true.into();
    fs::write(&sidecar, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let legacy_retry = pull(store.path(), archive_dir.path());
    assert_eq!(
        legacy_retry.sessions_found, 1,
        "inline legacy marker is honored"
    );
    let migrated: serde_json::Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(
        migrated.get("needsRetry"),
        None,
        "legacy marker is cleared safely"
    );
}

#[test]
fn second_pull_is_a_noop_via_bookmark() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 4);
    assert_eq!(stats.sessions_captured, 4);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let bookmark = archive
        .db()
        .unwrap()
        .bookmark(HOST, "claude")
        .unwrap()
        .expect("bookmark recorded after successful ingest");
    let captures_before = tree_bytes(&archive.captures_dir());

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0, "everything at or below bookmark");
    assert_eq!(stats.sessions_captured, 0);
    assert_eq!(stats.sessions_unchanged, 0);
    assert_eq!(stats.conversations_extracted, 0);
    assert_eq!(
        tree_bytes(&archive.captures_dir()),
        captures_before,
        "no capture bytes changed"
    );
    assert_eq!(
        archive.db().unwrap().bookmark(HOST, "claude").unwrap(),
        Some(bookmark),
        "bookmark unchanged by a no-op run"
    );
}

#[test]
fn grown_session_is_extended_and_capture_superseded() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let bookmark_before = archive
        .db()
        .unwrap()
        .bookmark(HOST, "claude")
        .unwrap()
        .unwrap();

    append_records(store.path(), &APPENDED_DIALOG);

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "only the grown session");
    assert_eq!(stats.sessions_captured, 1, "richer capture supersedes");
    assert_eq!(stats.sessions_unchanged, 0);
    assert_eq!(stats.conversations_extracted, 1);

    // Same identity, record extended — never duplicated.
    let db = archive.db().unwrap();
    assert_eq!(db.session_count().unwrap(), 4);
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == GROWN_ID)
        .unwrap();
    assert_eq!(session.message_count, 8, "6 + appended user/assistant pair");
    let messages = db.messages(session.id).unwrap();
    assert_eq!(messages[6].text, "Also add YAML support");
    assert_eq!(messages[7].text, "YAML support added.");
    assert!(db.bookmark(HOST, "claude").unwrap().unwrap() > bookmark_before);

    // The superseding capture is the full new payload, atomically in place:
    // exactly one body + one sidecar for the id, no temp leftovers.
    let capture_dir = archive.captures_dir().join(HOST).join("claude");
    let body = capture_dir.join(format!("{GROWN_ID}.jsonl.zst"));
    let decompressed = zstd::stream::decode_all(fs::File::open(&body).unwrap()).unwrap();
    let native = fs::read(grown_session_path(store.path())).unwrap();
    assert_eq!(decompressed, native, "capture is the grown file, verbatim");
    let siblings: Vec<String> = fs::read_dir(&capture_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(GROWN_ID) && !Path::new(n).is_dir())
        .collect();
    assert!(
        !siblings.iter().any(|n| n.ends_with(".tmp")),
        "no temp files left behind: {siblings:?}"
    );
    let sidecar: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(capture_dir.join(format!("{GROWN_ID}.meta.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["sourceSize"], native.len() as u64);
}

#[test]
fn session_arriving_with_an_old_mtime_is_still_ingested() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());

    // A brand-new session file lands with an mtime far below the Bookmark —
    // backup restores, `rsync -a`, tar extracts, and vendor renames all
    // preserve old mtimes. The Bookmark alone must not hide it.
    let new_id = "44444444-4444-4444-8444-444444444444";
    let path = store
        .path()
        .join("-workspace-Projects-example")
        .join(format!("{new_id}.jsonl"));
    fs::write(
        &path,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"Restored from backup"},"uuid":"00000000-0000-4000-8000-000000000010","timestamp":"2026-03-01T10:00:00.000Z","cwd":"/workspace/Projects/example","sessionId":"44444444-4444-4444-8444-444444444444"}"#,
            "\n",
            r#"{"type":"assistant","requestId":"req_010","message":{"id":"msg_010","type":"message","role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"Welcome back."}],"stop_reason":"end_turn"},"uuid":"00000000-0000-4000-8000-000000000011","timestamp":"2026-03-01T10:00:30.000Z","sessionId":"44444444-4444-4444-8444-444444444444"}"#,
            "\n",
        ),
    )
    .unwrap();
    set_mtime(
        &path,
        std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
    );

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "old mtime must not hide the file");
    assert_eq!(stats.sessions_captured, 1);
    assert_eq!(stats.conversations_extracted, 1);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    assert_eq!(db.session_count().unwrap(), 5);
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == new_id)
        .expect("backdated session archived");
    assert_eq!(session.message_count, 2);

    // Once captured, its sidecar vouches for it: pull is a no-op again.
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0);
}

#[test]
fn growth_hidden_below_the_bookmark_is_still_recaptured() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());

    // The session grows, but its mtime is rewound to the pre-growth value
    // (at or below the Bookmark). The sidecar size mismatch must still
    // defeat the mtime gate.
    let path = grown_session_path(store.path());
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
    append_records(store.path(), &APPENDED_DIALOG);
    set_mtime(&path, mtime_before);

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "sidecar mismatch defeats the gate");
    assert_eq!(stats.sessions_captured, 1, "richer capture supersedes");

    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == GROWN_ID)
        .unwrap();
    assert_eq!(session.message_count, 8, "appended tail is archived");

    // The new sidecar records the rewound mtime + grown size, so the next
    // pull is a no-op again.
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0);
}

#[test]
fn shrunken_native_file_never_supersedes_the_richer_capture() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();

    let native_path = grown_session_path(store.path());
    let original = fs::read(&native_path).unwrap();

    // The vendor prunes/compacts/rewinds the session file: rewrite it
    // shorter (first two lines only), with a fresh mtime.
    sleep(Duration::from_millis(25));
    let truncated: String = String::from_utf8(original.clone())
        .unwrap()
        .lines()
        .take(2)
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&native_path, &truncated).unwrap();

    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "shrunk session is re-checked");
    assert_eq!(stats.sessions_captured, 0, "supersede refused");
    assert_eq!(stats.sessions_refused, 1, "lost Dialog is refused");
    assert_eq!(stats.sessions_failed, 1, "refusal holds the Bookmark");

    // The Capture body is still the richer, pre-shrink payload, verbatim.
    let capture_dir = archive.captures_dir().join(HOST).join("claude");
    let body = capture_dir.join(format!("{GROWN_ID}.jsonl.zst"));
    let decompressed = zstd::stream::decode_all(fs::File::open(&body).unwrap()).unwrap();
    assert_eq!(decompressed, original, "capture kept the richer payload");
    let sidecar: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(capture_dir.join(format!("{GROWN_ID}.meta.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["sourceSize"], original.len() as u64);

    // Extraction still sees the full Conversation, and rebuild cements the
    // richer Capture, not the shrunk native file.
    let assert_full = |archive: &Archive| {
        let db = archive.db().unwrap();
        let session = db
            .sessions_with_dialog()
            .unwrap()
            .into_iter()
            .find(|s| s.native_id == GROWN_ID)
            .unwrap();
        assert_eq!(session.message_count, 6, "no archived messages lost");
    };
    assert_full(&archive);
    remove_db(&archive);
    rebuild(&archive).unwrap();
    assert_full(&archive);
}

#[test]
fn pull_with_lost_db_reuses_captures_and_repairs() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let captures_before = tree_bytes(&archive.captures_dir());

    remove_db(&archive);

    // No Bookmark, so everything is enumerated — but every Capture still
    // matches its native file (sidecar mtime + size), so bodies are reused
    // while the database is repopulated.
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 4);
    assert_eq!(stats.sessions_captured, 0, "no capture rewritten");
    assert_eq!(stats.sessions_unchanged, 4);
    assert_eq!(stats.conversations_extracted, 4);
    assert_eq!(tree_bytes(&archive.captures_dir()), captures_before);
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 4);
}

/// The slice 2 property (PLAN "Testing strategy"): for a range of native
/// growth shapes, extend-then-rebuild is content-equivalent to a fresh
/// ingest of the final Store — same database content dump, byte-identical
/// rendered Views.
#[test]
fn extend_then_rebuild_equals_fresh_ingest() {
    let growth_variants: [&[&str]; 3] = [
        // Ordinary dialog growth.
        &APPENDED_DIALOG,
        // Action-only growth: text + tool_use, then a trailing tool_use turn.
        &[
            r#"{"type":"assistant","requestId":"req_006","message":{"id":"msg_007","type":"message","role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"Running the tests."},{"type":"tool_use","id":"toolu_010","name":"Bash","input":{"command":"cargo test"}}]},"uuid":"00000000-0000-4000-8000-00000000000c","timestamp":"2026-03-05T15:05:00.000Z","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
            r#"{"type":"assistant","requestId":"req_007","message":{"id":"msg_008","type":"message","role":"assistant","model":"claude-opus-5","content":[{"type":"tool_use","id":"toolu_011","name":"Read","input":{"file_path":"src/lib.rs"}}]},"uuid":"00000000-0000-4000-8000-00000000000d","timestamp":"2026-03-05T15:05:10.000Z","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
        ],
        // Noise-only growth (meta record + title change): no new Messages,
        // but timestamps and title must still land identically.
        &[
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"},"uuid":"00000000-0000-4000-8000-00000000000e","timestamp":"2026-03-06T09:00:00.000Z","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
            r#"{"type":"custom-title","customTitle":"TOML and beyond","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
        ],
    ];

    for (i, variant) in growth_variants.iter().enumerate() {
        // Archive A: ingest, grow the Store, ingest again, then rebuild.
        let store = temp_store();
        let dir_a = tempfile::tempdir().unwrap();
        pull(store.path(), dir_a.path());
        append_records(store.path(), variant);
        pull(store.path(), dir_a.path());
        let archive_a = Archive::open(dir_a.path()).unwrap();
        rebuild(&archive_a).unwrap();
        render_all(&archive_a).unwrap();

        // Archive B: one fresh ingest of the final Store.
        let dir_b = tempfile::tempdir().unwrap();
        pull(store.path(), dir_b.path());
        let archive_b = Archive::open(dir_b.path()).unwrap();
        // Capture time records when this Archive performed IO, so two
        // independently captured Archives correctly differ. Normalize the
        // durable sidecars to test extraction/rebuild equivalence from the
        // same Capture set, then rebuild the projection under comparison.
        for (relative, bytes) in tree_bytes(&archive_a.captures_dir()) {
            if !relative.ends_with(".meta.json") {
                continue;
            }
            let source: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let target_path = archive_b.captures_dir().join(relative);
            let mut target: serde_json::Value =
                serde_json::from_slice(&fs::read(&target_path).unwrap()).unwrap();
            target["capturedAtMs"] = source["capturedAtMs"].clone();
            fs::write(target_path, serde_json::to_vec_pretty(&target).unwrap()).unwrap();
        }
        rebuild(&archive_b).unwrap();
        render_all(&archive_b).unwrap();

        assert_eq!(
            archive_a.db().unwrap().content_dump().unwrap(),
            archive_b.db().unwrap().content_dump().unwrap(),
            "variant {i}: database content differs"
        );
        assert_eq!(
            tree_bytes(&archive_a.views_dir()),
            tree_bytes(&archive_b.views_dir()),
            "variant {i}: rendered views differ"
        );
    }
}

#[test]
fn multi_host_projection_survives_stale_arrival_and_sorted_rebuild() {
    let rich_store = temp_store();
    append_records(rich_store.path(), &APPENDED_DIALOG);
    let stale_store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();

    let mut rich_roots = empty_roots(rich_store.path());
    rich_roots.claude = rich_store.path().to_path_buf();
    pull_localhost(&archive, &rich_roots, "z-rich").unwrap();
    let mut stale_roots = empty_roots(stale_store.path());
    stale_roots.claude = stale_store.path().to_path_buf();
    pull_localhost(&archive, &stale_roots, "a-stale").unwrap();

    let db = archive.db().unwrap();
    let session = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == GROWN_ID)
        .unwrap();
    assert_eq!(session.winner_host.as_deref(), Some("z-rich"));
    assert_eq!(db.messages(session.id).unwrap().len(), 8);
    let before = db.content_dump().unwrap();
    drop(db);

    rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().content_dump().unwrap(), before);
}

#[test]
fn delete_db_then_rebuild_restores_everything() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    render_all(&archive).unwrap();
    let dump_before = archive.db().unwrap().content_dump().unwrap();
    let views_before = tree_bytes(&archive.views_dir());
    assert!(dump_before.contains("bookmark testhost/claude"));

    remove_db(&archive);
    assert!(!archive.db_path().exists());

    let stats = rebuild(&archive).unwrap();
    assert_eq!(stats.captures_found, 4);
    assert_eq!(stats.conversations_extracted, 4);
    assert_eq!(stats.captures_skipped, 0);
    render_all(&archive).unwrap();

    assert_eq!(
        archive.db().unwrap().content_dump().unwrap(),
        dump_before,
        "rebuilt database is content-equivalent"
    );
    assert_eq!(
        tree_bytes(&archive.views_dir()),
        views_before,
        "re-rendered views are byte-identical"
    );

    // And the restored Bookmark keeps the next pull a no-op.
    let stats = pull(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0);
}

fn remove_db(archive: &Archive) {
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", archive.db_path().display()));
        let _ = fs::remove_file(path);
    }
}
