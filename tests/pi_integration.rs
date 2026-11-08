//! Slice 3 — Pi adapter end to end: enumerate, verbatim capture, leaf-path
//! extraction (abandoned branches excluded), legacy v1 files, growth, and a
//! rebuild round-trip. All against TEMP directories built from fixtures.

mod common;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::render::render_all;

use common::{HOST, copy_tree, pin_utc_workday, pull_pi, source_fixtures_dir, tree_bytes};

const TREE_ID: &str = "0198c9a4-7e01-7abc-8def-0123456789ab";
const LEGACY_ID: &str = "0193aaaa-1111-7bbb-8ccc-2222dddd3333";

fn temp_pi_store() -> tempfile::TempDir {
    let store = tempfile::tempdir().unwrap();
    copy_tree(&source_fixtures_dir("pi").join("store"), store.path());
    store
}

fn tree_session_path(store: &Path) -> PathBuf {
    store
        .join("--workspace-dev-myapp--")
        .join(format!("2026-03-10T14-00-00-000Z_{TREE_ID}.jsonl"))
}

fn pulled_archive() -> (tempfile::TempDir, tempfile::TempDir) {
    let store = temp_pi_store();
    let archive_dir = tempfile::tempdir().unwrap();
    pin_utc_workday(archive_dir.path());
    let stats = pull_pi(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 2);
    assert_eq!(stats.sessions_captured, 2);
    assert_eq!(stats.conversations_extracted, 2);
    (store, archive_dir)
}

#[test]
fn captures_are_verbatim_and_laid_out_per_spec() {
    let (store, archive_dir) = pulled_archive();
    let captures = archive_dir.path().join("captures").join(HOST).join("pi");

    for id in [TREE_ID, LEGACY_ID] {
        assert!(captures.join(format!("{id}.jsonl.zst")).is_file());
        assert!(captures.join(format!("{id}.meta.json")).is_file());
    }
    let original = fs::read(tree_session_path(store.path())).unwrap();
    let compressed = fs::File::open(captures.join(format!("{TREE_ID}.jsonl.zst"))).unwrap();
    assert_eq!(zstd::stream::decode_all(compressed).unwrap(), original);

    let sidecar: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(captures.join(format!("{TREE_ID}.meta.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["source"], "pi");
    assert_eq!(sidecar["projectDir"], "--workspace-dev-myapp--");
}

#[test]
fn leaf_path_extraction_and_legacy_linear_files() {
    let (_store, archive_dir) = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let db = archive.db().unwrap();
    let sessions = db.sessions_with_dialog().unwrap();
    assert_eq!(sessions.len(), 2);

    // v3 tree file: the Conversation is the root→leaf path; the abandoned
    // branch (user + assistant) is excluded but stays in the Capture.
    let tree = sessions.iter().find(|s| s.native_id == TREE_ID).unwrap();
    assert_eq!(tree.title.as_deref(), Some("Health endpoint"));
    assert_eq!(tree.project.as_deref(), Some("myapp"));
    assert_eq!(tree.project_path.as_deref(), Some("/workspace/dev/myapp"));
    assert_eq!(
        tree.transcript_facts.starting_model.as_deref(),
        Some("claude-sonnet-4-5")
    );
    let messages = db.messages(tree.id).unwrap();
    let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Add a health endpoint",
            "Adding a GET /health route that returns 200.",
            "Return build info in the payload\n\n[image]",
            "Done: /health now returns { status, version, commit }.",
        ]
    );
    // Thinking + toolCall are Actions: omitted, counted for the seam before
    // the next Message.
    assert_eq!(messages[2].actions_before, 2);
    // Message-level unix-ms timestamps are preferred.
    assert_eq!(messages[0].timestamp_ms, Some(1773151201000));

    // Legacy v1-era file: no entry ids — file order; hookMessage skipped;
    // bashExecution counted as a trailing Action; the cleared session_info
    // leaves the Conversation untitled.
    let legacy = sessions.iter().find(|s| s.native_id == LEGACY_ID).unwrap();
    assert_eq!(legacy.title, None, "cleared native name stays absent");
    let messages = db.messages(legacy.id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].text, "Tagged v1.2.0 and pushed.");
    assert_eq!(legacy.trailing_actions, 1, "bashExecution seam");
}

#[test]
fn rendered_views_match_goldens() {
    let (_store, archive_dir) = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let rstats = render_all(&archive).unwrap();
    assert_eq!(rstats.transcripts, 2);
    assert_eq!(rstats.indexes, 1);

    let views = archive_dir.path().join("views");
    let expected_root = source_fixtures_dir("pi").join("expected");
    let mut compared = 0;
    for project in fs::read_dir(&expected_root).unwrap() {
        let project = project.unwrap();
        let name = project.file_name();
        for file in fs::read_dir(project.path()).unwrap() {
            let file = file.unwrap();
            let rendered_path = views.join("projects").join(&name).join(file.file_name());
            let rendered = fs::read_to_string(&rendered_path)
                .unwrap_or_else(|_| panic!("missing rendered view {}", rendered_path.display()));
            assert_eq!(
                rendered,
                fs::read_to_string(file.path()).unwrap(),
                "transcript mismatch for {}/{}",
                name.to_string_lossy(),
                file.file_name().to_string_lossy()
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 3, "2 transcripts + 1 index");
}

#[test]
fn second_pull_is_a_noop_and_growth_extends() {
    let (store, archive_dir) = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    let captures_before = tree_bytes(&archive.captures_dir());

    let stats = pull_pi(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0);
    assert_eq!(tree_bytes(&archive.captures_dir()), captures_before);

    // Growth: a new turn extends the leaf, so the previous path stays the
    // active one and the Conversation gains the new pair.
    sleep(Duration::from_millis(25));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(tree_session_path(store.path()))
        .unwrap();
    writeln!(
        file,
        r#"{{"type":"message","id":"d0e1f2a3","parentId":"c9d0e1f2","timestamp":"2026-03-10T15:00:00.000Z","message":{{"role":"user","content":"Also add /ready","timestamp":1773154800000}}}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"type":"message","id":"e1f2a3b4","parentId":"d0e1f2a3","timestamp":"2026-03-10T15:00:05.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"/ready added."}}],"stopReason":"end_turn","timestamp":1773154805000}}}}"#
    )
    .unwrap();
    drop(file);

    let stats = pull_pi(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1);
    assert_eq!(stats.sessions_captured, 1, "richer capture supersedes");
    let db = archive.db().unwrap();
    let tree = db
        .sessions_with_dialog()
        .unwrap()
        .into_iter()
        .find(|s| s.native_id == TREE_ID)
        .unwrap();
    let messages = db.messages(tree.id).unwrap();
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[5].text, "/ready added.");
}

#[test]
fn delete_db_then_rebuild_restores_everything() {
    let (_store, archive_dir) = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();
    render_all(&archive).unwrap();
    let dump_before = archive.db().unwrap().content_dump().unwrap();
    let views_before = tree_bytes(&archive.views_dir());
    assert!(dump_before.contains("bookmark testhost/pi"));

    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!(
            "{}{suffix}",
            archive.db_path().display()
        )));
    }

    let stats = rebuild(&archive).unwrap();
    assert_eq!(stats.captures_found, 2);
    assert_eq!(stats.conversations_extracted, 2);
    render_all(&archive).unwrap();

    assert_eq!(archive.db().unwrap().content_dump().unwrap(), dump_before);
    assert_eq!(tree_bytes(&archive.views_dir()), views_before);
}
