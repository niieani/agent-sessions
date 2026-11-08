//! Supersede by Dialog coverage (ADR 2026-09-28): an append-oriented
//! native payload that differs from its Capture is judged by the Dialog the
//! Adapter parses out of both, never by byte size. Covered on both ingest
//! paths — local pull and Bundle ingest — with a synthetic Codex format
//! migration pair (the archived form and its smaller rewritten form, same Dialog and time span).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agent_sessions::archive::Archive;
use agent_sessions::bundle::{BundleIngest, export, ingest_bundle};
use agent_sessions::pull::StoreRoots;

use common::{HOST, empty_roots, pull_codex, source_fixtures_dir};

const NATIVE_ID: &str = "11111111-1111-4111-8111-111111111111";

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        source_fixtures_dir("codex")
            .join("format-migration")
            .join(name),
    )
    .unwrap()
}

fn rollout_path(store: &Path) -> PathBuf {
    store
        .join("sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T00-00-00-{NATIVE_ID}.jsonl"))
}

/// A Codex Store holding the archived-format rollout.
fn archived_store() -> tempfile::TempDir {
    let store = tempfile::tempdir().unwrap();
    let path = rollout_path(store.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, fixture("archived.jsonl")).unwrap();
    set_mtime(
        &path,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_778_916_512),
    );
    store
}

/// Replace the native rollout with `bytes`, stamped strictly newer.
fn rewrite_native(store: &Path, bytes: &[u8]) {
    let path = rollout_path(store);
    fs::write(&path, bytes).unwrap();
    set_mtime(
        &path,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
    );
}

fn set_mtime(path: &Path, time: SystemTime) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn capture_dir(archive: &Archive) -> PathBuf {
    archive.captures_dir().join(HOST).join("codex")
}

fn capture_body(archive: &Archive) -> Vec<u8> {
    let body = capture_dir(archive).join(format!("{NATIVE_ID}.jsonl.zst"));
    zstd::stream::decode_all(fs::File::open(body).unwrap()).unwrap()
}

fn sidecar(archive: &Archive) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(capture_dir(archive).join(format!("{NATIVE_ID}.meta.json"))).unwrap(),
    )
    .unwrap()
}

fn revisions_dir(archive: &Archive) -> PathBuf {
    capture_dir(archive).join(format!("{NATIVE_ID}.revisions"))
}

/// The archived fixture with the assistant's reply reworded at the same
/// timestamp: same span, Dialog that no longer lines up.
fn diverged() -> Vec<u8> {
    String::from_utf8(fixture("archived.jsonl"))
        .unwrap()
        .replace(
            "Inspecting the example project now.",
            "Something else entirely was said",
        )
        .into_bytes()
}

/// The archived fixture without its last Dialog message and anything after.
fn truncated() -> Vec<u8> {
    let text = String::from_utf8(fixture("archived.jsonl")).unwrap();
    text.lines()
        .take_while(|line| !line.contains("<turn_aborted>"))
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes()
}

fn codex_roots(store: &Path) -> StoreRoots {
    StoreRoots {
        codex: store.to_path_buf(),
        ..empty_roots(store)
    }
}

fn export_codex(store: &Path, since: Option<i64>) -> Vec<u8> {
    let mut bundle = Vec::new();
    export(&mut bundle, &codex_roots(store), HOST, Some("codex"), since).unwrap();
    bundle
}

/// Ingest and advance Bookmarks exactly as fleet pull does: only when no
/// unit failed.
fn ingest(bundle: &[u8], archive: &Archive) -> BundleIngest {
    let mut db = archive.db().unwrap();
    let ingested = ingest_bundle(bundle, archive, &mut db).unwrap();
    if ingested.stats.sessions_failed == 0 {
        for ((host, source), bookmark_ms) in &ingested.bookmarks {
            db.advance_bookmark(host, source, *bookmark_ms).unwrap();
        }
    }
    ingested
}

fn message_count(archive: &Archive) -> i64 {
    archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == NATIVE_ID)
        .unwrap()
        .message_count
}

#[test]
fn smaller_rewrite_with_the_same_dialog_supersedes_on_local_pull() {
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull_codex(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let messages_before = message_count(&archive);

    let rewritten = fixture("rewritten.jsonl");
    assert!(rewritten.len() < fixture("archived.jsonl").len());
    rewrite_native(store.path(), &rewritten);

    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(stats.sessions_extended, 1, "rewrite supersedes");
    assert_eq!(stats.sessions_revised, 0);
    assert_eq!(capture_body(&archive), rewritten);
    assert_eq!(sidecar(&archive)["sourceSize"], rewritten.len() as u64);
    assert!(!revisions_dir(&archive).exists());
    assert_eq!(message_count(&archive), messages_before);

    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0, "settled: the next pull is a no-op");
}

#[test]
fn smaller_rewrite_with_the_same_dialog_supersedes_over_a_bundle_and_advances_the_bookmark() {
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    ingest(&export_codex(store.path(), None), &archive);
    let since = archive.db().unwrap().bookmark(HOST, "codex").unwrap();

    let rewritten = fixture("rewritten.jsonl");
    rewrite_native(store.path(), &rewritten);
    let ingested = ingest(&export_codex(store.path(), since), &archive);
    assert_eq!(ingested.stats.sessions_failed, 0);
    assert_eq!(ingested.stats.sessions_extended, 1);
    assert_eq!(ingested.stats.sessions_revised, 0);
    assert_eq!(capture_body(&archive), rewritten);
    let advanced = archive.db().unwrap().bookmark(HOST, "codex").unwrap();
    assert_eq!(advanced, Some(1_790_000_000_000), "Bookmark advances");

    let again = ingest(&export_codex(store.path(), advanced), &archive);
    assert_eq!(again.stats.units_in_bundle, 0, "nothing is re-sent");
}

#[test]
fn diverged_dialog_keeps_the_prior_capture_as_a_revision_without_failing() {
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull_codex(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let archived_sidecar = sidecar(&archive);

    let replacement = diverged();
    rewrite_native(store.path(), &replacement);
    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(
        stats.sessions_failed, 0,
        "divergence is reported, not failed"
    );
    assert_eq!(stats.sessions_revised, 1);
    assert_eq!(
        capture_body(&archive),
        replacement,
        "new payload is current"
    );

    let revisions: Vec<PathBuf> = fs::read_dir(revisions_dir(&archive))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let body = revisions
        .iter()
        .find(|path| path.to_string_lossy().ends_with(".jsonl.zst"))
        .expect("prior body retained");
    let decoded = zstd::stream::decode_all(fs::File::open(body).unwrap()).unwrap();
    assert_eq!(decoded, fixture("archived.jsonl"));
    let revision_meta = revisions
        .iter()
        .find(|path| path.to_string_lossy().ends_with(".revision.json"))
        .expect("prior Provenance retained");
    let revision_meta: serde_json::Value =
        serde_json::from_slice(&fs::read(revision_meta).unwrap()).unwrap();
    assert_eq!(revision_meta, archived_sidecar);

    // A retained revision is never mistaken for a current Capture.
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 1);
}

#[test]
fn diverged_dialog_over_a_bundle_is_revised_and_advances_the_bookmark() {
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    ingest(&export_codex(store.path(), None), &archive);
    let since = archive.db().unwrap().bookmark(HOST, "codex").unwrap();

    rewrite_native(store.path(), &diverged());
    let ingested = ingest(&export_codex(store.path(), since), &archive);
    assert_eq!(ingested.stats.sessions_failed, 0);
    assert_eq!(ingested.stats.sessions_revised, 1);
    assert!(revisions_dir(&archive).is_dir());
    assert_eq!(
        archive.db().unwrap().bookmark(HOST, "codex").unwrap(),
        Some(1_790_000_000_000)
    );
}

#[test]
fn truncated_dialog_is_refused_and_counted_as_failed_on_both_paths() {
    // Local pull.
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull_codex(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let messages_before = message_count(&archive);
    rewrite_native(store.path(), &truncated());
    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_refused, 1);
    assert_eq!(
        stats.sessions_failed, 1,
        "refusal keeps the retry window open"
    );
    assert_eq!(capture_body(&archive), fixture("archived.jsonl"));
    assert_eq!(message_count(&archive), messages_before);

    // Bundle ingest.
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    ingest(&export_codex(store.path(), None), &archive);
    let since = archive.db().unwrap().bookmark(HOST, "codex").unwrap();
    rewrite_native(store.path(), &truncated());
    let ingested = ingest(&export_codex(store.path(), since), &archive);
    assert_eq!(ingested.stats.sessions_refused, 1);
    assert_eq!(ingested.stats.sessions_failed, 1);
    assert_eq!(capture_body(&archive), fixture("archived.jsonl"));
    assert_eq!(
        archive.db().unwrap().bookmark(HOST, "codex").unwrap(),
        since
    );
}

#[test]
fn an_unreadable_capture_body_fails_loudly_instead_of_being_replaced() {
    let store = archived_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pull_codex(store.path(), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let body = capture_dir(&archive).join(format!("{NATIVE_ID}.jsonl.zst"));
    fs::write(&body, b"not zstd").unwrap();

    rewrite_native(store.path(), &fixture("rewritten.jsonl"));
    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_failed, 1);
    assert_eq!(fs::read(&body).unwrap(), b"not zstd", "nothing overwritten");
}
