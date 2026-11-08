//! Slice 4 Bundle tests: export from a temp Store → ingest into a temp
//! Archive must equal a direct local pull; manifest correctness; supersede
//! rules (growth extends, lost Dialog refused) over the Bundle path.

mod common;

use std::fs;
use std::io::Read;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use agent_sessions::adapters::{Capture, CaptureMeta, adapter_for};
use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::bundle::{BundleManifest, export, ingest_bundle};
use agent_sessions::pull::StoreRoots;
use agent_sessions::render::render_all;

use common::{
    HOST, align_capture_times, copy_tree, empty_roots, pull, source_fixtures_dir, temp_store,
    tree_bytes,
};

fn export_bundle(store: &Path, since: Option<i64>) -> Vec<u8> {
    let mut bundle = Vec::new();
    let roots = StoreRoots {
        claude: store.to_path_buf(),
        ..empty_roots(store)
    };
    export(&mut bundle, &roots, HOST, Some("claude"), since).unwrap();
    bundle
}

fn all_source_roots() -> (tempfile::TempDir, StoreRoots) {
    let stores = tempfile::tempdir().unwrap();
    let claude = stores.path().join("claude");
    let codex = stores.path().join("codex");
    let pi = stores.path().join("pi");
    let cursor = stores.path().join("no-cursor-store/state.vscdb");
    let t3 = stores.path().join("no-t3-store");
    copy_tree(&source_fixtures_dir("claude").join("projects"), &claude);
    copy_tree(&source_fixtures_dir("codex").join("store"), &codex);
    copy_tree(&source_fixtures_dir("pi").join("store"), &pi);
    (
        stores,
        StoreRoots {
            chatgpt: claude.parent().unwrap().join("no-chatgpt-store"),
            claude,
            codex,
            pi,
            cursor,
            t3,
            t3_explicit: false,
        },
    )
}

/// Ingest `bundle` into the Archive at `dir`, advancing Bookmarks the way
/// `pull` does on success.
fn ingest(bundle: &[u8], dir: &Path) -> agent_sessions::bundle::BundleIngest {
    let archive = Archive::open(dir).unwrap();
    let mut db = archive.db().unwrap();
    let ingested = ingest_bundle(bundle, &archive, &mut db).unwrap();
    for ((host, source), bookmark_ms) in &ingested.bookmarks {
        db.advance_bookmark(host, source, *bookmark_ms).unwrap();
    }
    ingested
}

fn tamper_sidecars(bundle: &[u8], mutate: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
    let mut input = tar::Archive::new(bundle);
    let mut output = Vec::new();
    let mut tar = tar::Builder::new(&mut output);
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if path.to_string_lossy().ends_with(".meta.json") {
            let mut meta: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            mutate(&mut meta);
            bytes = serde_json::to_vec(&meta).unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap();
    output
}

fn tamper_manifest(bundle: &[u8], mutate: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
    let mut input = tar::Archive::new(bundle);
    let mut output = Vec::new();
    let mut tar = tar::Builder::new(&mut output);
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if path == Path::new("manifest.json") {
            let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            mutate(&mut manifest);
            bytes = serde_json::to_vec(&manifest).unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap();
    output
}

fn corrupt_first_capture_body(bundle: &[u8]) -> Vec<u8> {
    let mut input = tar::Archive::new(bundle);
    let mut output = Vec::new();
    let mut tar = tar::Builder::new(&mut output);
    let mut corrupted = false;
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if !corrupted && path.to_string_lossy().ends_with(".zst") {
            bytes = b"not a zstd stream".to_vec();
            corrupted = true;
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap();
    assert!(corrupted);
    output
}

fn duplicate_first_identity_over_second(bundle: &[u8], path_alias: bool) -> Vec<u8> {
    let mut input = tar::Archive::new(bundle);
    let mut entries = Vec::new();
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.push((path, bytes));
    }
    assert!(entries.len() >= 5, "manifest plus at least two pairs");
    entries[3] = entries[1].clone();
    entries[4] = entries[2].clone();
    if path_alias {
        let mut meta: serde_json::Value = serde_json::from_slice(&entries[1].1).unwrap();
        let native_id = meta["nativeId"].as_str().unwrap();
        meta["nativeId"] = format!("./{native_id}").into();
        entries[1].1 = serde_json::to_vec(&meta).unwrap();
    }

    let mut output = Vec::new();
    let mut tar = tar::Builder::new(&mut output);
    for (path, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap();
    output
}

#[test]
fn bundle_round_trip_equals_direct_local_pull() {
    let store = temp_store();

    // Path A: export → Bundle bytes → central ingest.
    let bundle = export_bundle(store.path(), None);
    let bundled_dir = tempfile::tempdir().unwrap();
    let ingested = ingest(&bundle, bundled_dir.path());
    assert_eq!(ingested.stats.units_in_bundle, 4, "3 main + 1 subagent");
    assert_eq!(ingested.stats.sessions_captured, 4);
    assert_eq!(ingested.stats.conversations_extracted, 4);

    // Path B: direct local pull from the same Store.
    let direct_dir = tempfile::tempdir().unwrap();
    pull(store.path(), direct_dir.path());

    // Same database content (sessions, hosts, messages, Bookmarks).
    let bundled = Archive::open(bundled_dir.path()).unwrap();
    let direct = Archive::open(direct_dir.path()).unwrap();
    align_capture_times(&bundled, &direct);
    assert_eq!(
        bundled.db().unwrap().content_dump().unwrap(),
        direct.db().unwrap().content_dump().unwrap(),
    );

    // Same Capture bodies, byte for byte (sidecars differ only in
    // captured-at time).
    let body_bytes = |dir: &Path| {
        tree_bytes(&dir.join("captures"))
            .into_iter()
            .filter(|(p, _)| p.ends_with(".zst"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        body_bytes(bundled_dir.path()),
        body_bytes(direct_dir.path())
    );

    // And byte-identical Views (rendering is a pure function of content).
    render_all(&bundled).unwrap();
    render_all(&direct).unwrap();
    assert_eq!(
        tree_bytes(&bundled_dir.path().join("views")),
        tree_bytes(&direct_dir.path().join("views")),
    );
}

#[test]
fn irrecoverable_bundle_extraction_failure_is_counted_once() {
    let store = temp_store();
    let bundle = corrupt_first_capture_body(&export_bundle(store.path(), None));
    let archive_dir = tempfile::tempdir().unwrap();
    let ingested = ingest(&bundle, archive_dir.path());
    assert_eq!(ingested.stats.units_in_bundle, 4);
    assert_eq!(ingested.stats.sessions_failed, 1);
}

#[test]
fn bundle_round_trip_covers_every_available_source() {
    let (_stores, roots) = all_source_roots();

    let mut bundle = Vec::new();
    export(&mut bundle, &roots, HOST, None, None).unwrap();

    let bundled_dir = tempfile::tempdir().unwrap();
    let bundled = ingest(&bundle, bundled_dir.path());
    assert_eq!(
        bundled.manifest.sources,
        agent_sessions::adapters::AVAILABLE_SOURCES
    );

    let direct_dir = tempfile::tempdir().unwrap();
    let direct_archive = Archive::open(direct_dir.path()).unwrap();
    agent_sessions::pull::pull_localhost(&direct_archive, &roots, HOST).unwrap();
    let direct = Archive::open(direct_dir.path()).unwrap();
    let bundled = Archive::open(bundled_dir.path()).unwrap();
    align_capture_times(&bundled, &direct);
    assert_eq!(
        bundled.db().unwrap().content_dump().unwrap(),
        direct.db().unwrap().content_dump().unwrap(),
    );
}

#[test]
fn incremental_export_requires_exactly_one_source() {
    let store = tempfile::tempdir().unwrap();
    let roots = empty_roots(store.path());
    let error = export(Vec::new(), &roots, HOST, None, Some(123)).unwrap_err();
    assert!(error.to_string().contains("requires a single Source"));
}

#[test]
fn manifest_is_first_entry_and_correct() {
    let store = temp_store();
    let bundle = export_bundle(store.path(), Some(123));

    let mut tar = tar::Archive::new(&bundle[..]);
    let mut entries = tar.entries().unwrap();
    let mut first = entries.next().unwrap().unwrap();
    assert_eq!(first.path().unwrap().as_ref(), Path::new("manifest.json"));
    let mut raw = String::new();
    first.read_to_string(&mut raw).unwrap();
    let manifest: BundleManifest = serde_json::from_str(&raw).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.host, HOST);
    assert_eq!(manifest.sources, vec!["claude".to_string()]);
    assert_eq!(manifest.since_ms, Some(123));
    assert_eq!(manifest.unit_count, 4);

    // Every session contributes a sidecar entry immediately followed by its
    // body, under captures/<host>/<source>/.
    let mut pairs = 0;
    while let Some(entry) = entries.next() {
        let sidecar = entry.unwrap();
        let sidecar_path = sidecar.path().unwrap().into_owned();
        assert!(
            sidecar_path.starts_with(format!("captures/{HOST}/claude")),
            "unexpected entry path {}",
            sidecar_path.display()
        );
        assert!(sidecar_path.to_string_lossy().ends_with(".meta.json"));
        let body = entries.next().expect("body after sidecar").unwrap();
        assert!(body.path().unwrap().to_string_lossy().ends_with(".zst"));
        pairs += 1;
    }
    assert_eq!(pairs, 4);
}

#[test]
fn bundle_rejects_sidecars_outside_its_manifest_before_writing() {
    let store = temp_store();
    let bundle = export_bundle(store.path(), None);
    let tampered = tamper_sidecars(&bundle, |meta| {
        meta["host"] = "untrusted-host".into();
    });
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut db = archive.db().unwrap();

    let error = ingest_bundle(&tampered[..], &archive, &mut db).unwrap_err();
    assert!(error.to_string().contains("does not match manifest"));
    assert!(
        !archive.captures_dir().join("untrusted-host").exists(),
        "a rejected Bundle must not write its claimed Host directory"
    );
}

#[test]
fn bundle_rejects_manifest_unit_count_mismatch() {
    let store = temp_store();
    let bundle = export_bundle(store.path(), None);
    for declared in [3, 5] {
        let tampered = tamper_manifest(&bundle, |manifest| {
            manifest["unitCount"] = declared.into();
        });
        let archive_dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(archive_dir.path()).unwrap();
        let mut db = archive.db().unwrap();
        let error = ingest_bundle(&tampered[..], &archive, &mut db).unwrap_err();
        assert!(error.to_string().contains("manifest declares"));
    }
}

#[test]
fn bundle_rejects_duplicate_identity_even_when_declared_count_matches() {
    let store = temp_store();
    let tampered = duplicate_first_identity_over_second(&export_bundle(store.path(), None), false);
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    let error = ingest_bundle(&tampered[..], &archive, &mut db).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("duplicate native archival unit identity")
    );
    assert!(db.bookmark(HOST, "claude").unwrap().is_none());
}

#[test]
fn bundle_rejects_path_alias_identity_before_archive_mutation() {
    let store = temp_store();
    let tampered = duplicate_first_identity_over_second(&export_bundle(store.path(), None), true);
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let mut db = archive.db().unwrap();
    let error = ingest_bundle(&tampered[..], &archive, &mut db).unwrap_err();
    assert!(error.to_string().contains("unsafe path component"));
    assert_eq!(db.session_count().unwrap(), 0);
    assert!(db.bookmark(HOST, "claude").unwrap().is_none());
    assert!(
        !archive.captures_dir().join(HOST).exists(),
        "alias is rejected before any Capture destination is mutated"
    );
}

#[test]
fn bundle_ingest_is_idempotent_and_since_prunes() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();

    let bundle = export_bundle(store.path(), None);
    ingest(&bundle, archive_dir.path());

    // Re-ingesting the same Bundle changes nothing.
    let again = ingest(&bundle, archive_dir.path());
    assert_eq!(again.stats.sessions_captured, 0);
    assert_eq!(again.stats.sessions_unchanged, 4);

    // An export since the Bookmark is empty (stateless remote, mtime gate).
    let archive = Archive::open(archive_dir.path()).unwrap();
    let bookmark = archive
        .db()
        .unwrap()
        .bookmark(HOST, "claude")
        .unwrap()
        .expect("bookmark advanced");
    let incremental = export_bundle(store.path(), Some(bookmark));
    let after = ingest(&incremental, archive_dir.path());
    assert_eq!(after.stats.units_in_bundle, 0);
}

#[test]
fn incremental_bundle_carries_meta_only_revisions_without_replacing_body() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    ingest(&export_bundle(store.path(), None), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let bookmark = archive
        .db()
        .unwrap()
        .bookmark(HOST, "claude")
        .unwrap()
        .unwrap();
    let child_id = "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef";
    let body = archive
        .captures_dir()
        .join(HOST)
        .join("claude")
        .join(format!("{child_id}.jsonl.zst"));
    let body_before = fs::read(&body).unwrap();

    sleep(Duration::from_millis(25));
    fs::write(
        store
            .path()
            .join("-workspace-Projects-example")
            .join(format!("{child_id}.meta.json")),
        r#"{"agentType":"Explore","description":"Map codebase","toolUseId":"toolu_01AAA","spawnDepth":3}"#,
    )
    .unwrap();
    let incremental = export_bundle(store.path(), Some(bookmark));
    let ingested = ingest(&incremental, archive_dir.path());
    assert_eq!(ingested.stats.units_in_bundle, 1);
    assert_eq!(ingested.stats.sessions_captured, 1);
    assert_eq!(fs::read(&body).unwrap(), body_before);
    let child = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == child_id)
        .unwrap();
    assert_eq!(child.subagent_parent.unwrap().depth, Some(3));
}

#[test]
fn unchanged_bundle_body_accepts_resolved_goal_attachment_metadata() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let native_path = "/workspace/.codex/attachments/example/goal-objective.md";

    let missing = tamper_sidecars(&export_bundle(store.path(), None), |meta| {
        meta["adapterVersion"] = 2.into();
        meta["goalAttachments"] = serde_json::json!([{
            "nativePath": native_path,
            "status": "missing"
        }]);
    });
    ingest(&missing, archive_dir.path());

    let resolved = tamper_sidecars(&export_bundle(store.path(), None), |meta| {
        meta["adapterVersion"] = 2.into();
        meta["goalAttachments"] = serde_json::json!([{
            "nativePath": native_path,
            "status": "resolved",
            "text": "Preserved remote objective"
        }]);
    });
    let ingested = ingest(&resolved, archive_dir.path());
    assert_eq!(ingested.stats.sessions_captured, 4);

    let sidecar = archive_dir
        .path()
        .join("captures")
        .join(HOST)
        .join("claude")
        .join("11111111-1111-4111-8111-111111111111.meta.json");
    let meta: CaptureMeta = serde_json::from_slice(&fs::read(sidecar).unwrap()).unwrap();
    assert_eq!(meta.goal_attachments.len(), 1);
    assert_eq!(
        meta.goal_attachments[0].text.as_deref(),
        Some("Preserved remote objective")
    );
}

#[test]
fn grown_session_supersedes_and_shrunken_is_refused() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    ingest(&export_bundle(store.path(), None), archive_dir.path());

    let session_path = store
        .path()
        .join("-workspace-Projects-example")
        .join("11111111-1111-4111-8111-111111111111.jsonl");
    let original = fs::read_to_string(&session_path).unwrap();

    // Growth: append a new user turn, re-export, ingest → record extends.
    let extra = r#"{"type":"user","message":{"role":"user","content":"One more thing."},"timestamp":"2026-08-20T12:00:00.000Z","cwd":"/workspace/Projects/example"}"#;
    fs::write(&session_path, format!("{original}{extra}\n")).unwrap();
    let grown = ingest(&export_bundle(store.path(), None), archive_dir.path());
    assert_eq!(grown.stats.sessions_captured, 1, "only the grown session");
    assert_eq!(grown.stats.sessions_unchanged, 3);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    let s1 = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == "11111111-1111-4111-8111-111111111111")
        .unwrap();
    assert_eq!(db.messages(s1.id).unwrap().len(), 7, "extended by one");

    // Shrink: rewind the native file below the Capture, re-export, ingest →
    // the richer Capture is kept and the record stays at 7 messages.
    fs::write(&session_path, &original).unwrap();
    let shrunk = ingest(&export_bundle(store.path(), None), archive_dir.path());
    assert_eq!(shrunk.stats.sessions_captured, 0);
    assert_eq!(shrunk.stats.sessions_unchanged, 3);
    assert_eq!(shrunk.stats.sessions_refused, 1);
    assert_eq!(shrunk.stats.sessions_failed, 1);
    let db = archive.db().unwrap();
    assert_eq!(
        db.messages(s1.id).unwrap().len(),
        7,
        "richer Capture stands after shrink"
    );
}

#[test]
fn refused_bundle_shrink_still_merges_newer_native_metadata() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    let child_id = "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef";
    let child_path = store
        .path()
        .join("-workspace-Projects-example")
        .join(format!("{child_id}.jsonl"));
    let meta_path = store
        .path()
        .join("-workspace-Projects-example")
        .join(format!("{child_id}.meta.json"));
    let original = fs::read_to_string(&child_path).unwrap();
    let extra = r#"{"type":"user","message":{"role":"user","content":"Retain this grown turn."},"timestamp":"2026-08-20T12:00:00.000Z","cwd":"/workspace/Projects/example"}"#;
    fs::write(&child_path, format!("{original}{extra}\n")).unwrap();
    ingest(&export_bundle(store.path(), None), archive_dir.path());

    let archive = Archive::open(archive_dir.path()).unwrap();
    let body_path = archive
        .captures_dir()
        .join(HOST)
        .join("claude")
        .join(format!("{child_id}.jsonl.zst"));
    let capture_meta_path = archive
        .captures_dir()
        .join(HOST)
        .join("claude")
        .join(format!("{child_id}.meta.json"));
    let body_before = fs::read(&body_path).unwrap();
    let sidecar_before: serde_json::Value =
        serde_json::from_slice(&fs::read(&capture_meta_path).unwrap()).unwrap();

    fs::write(&child_path, original).unwrap();
    sleep(Duration::from_millis(25));
    fs::write(
        &meta_path,
        r#"{"agentType":"Explore","description":"New metadata on shrunken body","toolUseId":"toolu_01AAA","spawnDepth":7}"#,
    )
    .unwrap();
    let ingested = ingest(&export_bundle(store.path(), None), archive_dir.path());
    assert_eq!(ingested.stats.sessions_refused, 1);
    assert_eq!(fs::read(&body_path).unwrap(), body_before);
    let sidecar_after: serde_json::Value =
        serde_json::from_slice(&fs::read(&capture_meta_path).unwrap()).unwrap();
    assert_eq!(sidecar_after["sourceSize"], sidecar_before["sourceSize"]);
    assert_eq!(
        sidecar_after["sourceMtimeMs"],
        sidecar_before["sourceMtimeMs"]
    );
    assert_eq!(sidecar_after["nativeMeta"]["spawnDepth"], 7);

    let child = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|session| session.native_id == child_id)
        .unwrap();
    assert_eq!(child.subagent_parent.unwrap().depth, Some(7));
}

#[test]
fn settled_bundle_repairs_newer_same_host_database_revision_before_rebuild() {
    let store = temp_store();
    let archive_dir = tempfile::tempdir().unwrap();
    ingest(&export_bundle(store.path(), None), archive_dir.path());
    let archive = Archive::open(archive_dir.path()).unwrap();
    let native_id = "11111111-1111-4111-8111-111111111111";
    let base = archive.captures_dir().join(HOST).join("claude");
    let meta_path = base.join(format!("{native_id}.meta.json"));
    let body_path = base.join(format!("{native_id}.jsonl.zst"));
    let settled_meta: CaptureMeta = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    let capture = Capture {
        body_path,
        meta_path,
        meta: settled_meta.clone(),
        written: false,
        outcome: agent_sessions::adapters::CaptureOutcome::Unchanged,
    };
    let mut stale_conversation = adapter_for("claude")
        .unwrap()
        .extract(&capture)
        .unwrap()
        .unwrap();
    stale_conversation.messages[0].text = "database-only future projection".into();
    let mut stale_revision = settled_meta;
    stale_revision.source_mtime_ms += 100_000;
    stale_revision.source_size += 100_000;
    stale_revision.native_meta_mtime_ms = Some(9_999_999_999_999);
    stale_revision.native_meta_size = Some(99_999);
    archive
        .db()
        .unwrap()
        .upsert_conversation_from_capture(&stale_conversation, &stale_revision)
        .unwrap();

    // The Bundle body is already settled by Capture supersede policy. Its
    // extraction must overwrite the same Host even though SQLite claims a
    // logically newer revision.
    ingest(&export_bundle(store.path(), None), archive_dir.path());
    let before_rebuild = archive.db().unwrap().content_dump().unwrap();
    assert!(!before_rebuild.contains("database-only future projection"));
    rebuild(&archive).unwrap();
    assert_eq!(
        archive.db().unwrap().content_dump().unwrap(),
        before_rebuild
    );
}
