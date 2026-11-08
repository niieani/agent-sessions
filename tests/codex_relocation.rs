//! Codex moves a rollout file between `sessions/YYYY/MM/DD/` and the flat
//! `archived_sessions/` when a thread is archived or unarchived. The bytes
//! do not change, so the Capture stands; only its recorded native location
//! moves — and once recorded, the enumerate gate skips the Session again.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use agent_sessions::adapters::CaptureMeta;
use agent_sessions::archive::Archive;

use common::{HOST, copy_tree, pin_utc_workday, pull_codex, source_fixtures_dir, tree_bytes};

const MAIN_ID: &str = "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa";
const SEGMENT_ID: &str = "33333333-3333-7333-8333-333333333333";

fn sidecar(archive_dir: &Path, native_id: &str) -> CaptureMeta {
    let path = archive_dir
        .join("captures")
        .join(HOST)
        .join("codex")
        .join(format!("{native_id}.meta.json"));
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn body_bytes(archive_dir: &Path, native_id: &str) -> Vec<u8> {
    fs::read(
        archive_dir
            .join("captures")
            .join(HOST)
            .join("codex")
            .join(format!("{native_id}.jsonl.zst")),
    )
    .unwrap()
}

/// Move `from` to `to` the way Codex does: a rename, which keeps the mtime.
fn move_rollout(from: &Path, to: &Path) -> PathBuf {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::rename(from, to).unwrap();
    fs::canonicalize(to).unwrap()
}

#[test]
fn archiving_and_unarchiving_records_the_new_location_without_recapturing() {
    let store = tempfile::tempdir().unwrap();
    copy_tree(&source_fixtures_dir("codex").join("store"), store.path());
    let archive_dir = tempfile::tempdir().unwrap();
    pin_utc_workday(archive_dir.path());
    pull_codex(store.path(), archive_dir.path());
    assert_eq!(
        pull_codex(store.path(), archive_dir.path()).sessions_found,
        0
    );
    let body_before = body_bytes(archive_dir.path(), MAIN_ID);

    let filename = format!("rollout-2026-03-05T09-00-00-{MAIN_ID}.jsonl");
    let dated = store.path().join("sessions/2026/03/05").join(&filename);
    let archived = move_rollout(
        &dated,
        &store.path().join("archived_sessions").join(&filename),
    );

    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1, "the moved file is re-checked once");
    assert_eq!(
        stats.sessions_unchanged, 1,
        "same bytes: a move is not growth"
    );
    assert_eq!(stats.sessions_captured, 0);
    assert_eq!(stats.sessions_failed, 0);
    assert_eq!(
        sidecar(archive_dir.path(), MAIN_ID).native_path,
        archived.to_string_lossy()
    );
    assert_eq!(body_bytes(archive_dir.path(), MAIN_ID), body_before);

    let archive = Archive::open(archive_dir.path()).unwrap();
    let captures_after_move = tree_bytes(&archive.captures_dir());
    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 0, "the recorded location is skipped");
    assert_eq!(tree_bytes(&archive.captures_dir()), captures_after_move);

    // Unarchive: back into the dated tree.
    let restored = move_rollout(&archived, &dated);
    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1);
    assert_eq!(stats.sessions_unchanged, 1);
    assert_eq!(stats.sessions_captured, 0);
    assert_eq!(
        sidecar(archive_dir.path(), MAIN_ID).native_path,
        restored.to_string_lossy()
    );
    assert_eq!(
        pull_codex(store.path(), archive_dir.path()).sessions_found,
        0
    );
}

fn rollout_line(timestamp: &str, ordinal: u32, body: &str) -> String {
    format!(r#"{{"timestamp":"{timestamp}","ordinal":{ordinal},{body}}}"#)
}

#[test]
fn archiving_a_rollover_group_records_every_segment_location_without_recapturing() {
    let store = tempfile::tempdir().unwrap();
    let day = store.path().join("sessions/2026/03/07");
    fs::create_dir_all(&day).unwrap();
    let primary_name = format!("rollout-2026-03-07T09-00-00-{MAIN_ID}.jsonl");
    let segment_name = format!("rollout-2026-03-07T10-00-00-{MAIN_ID}_{SEGMENT_ID}.jsonl");
    let meta = format!(
        r#""type":"session_meta","payload":{{"id":"{MAIN_ID}","cwd":"/workspace/Projects/rollover"}}"#
    );
    fs::write(
        day.join(&primary_name),
        [
            rollout_line("2026-03-07T09:00:00.000Z", 0, &meta),
            rollout_line(
                "2026-03-07T09:00:01.000Z",
                1,
                r#""type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Before rollover."}]}"#,
            ),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    fs::write(
        day.join(&segment_name),
        [
            rollout_line("2026-03-07T10:00:00.000Z", 2, &meta),
            rollout_line(
                "2026-03-07T10:00:01.000Z",
                3,
                r#""type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"After rollover."}]}"#,
            ),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();

    let archive_dir = tempfile::tempdir().unwrap();
    pin_utc_workday(archive_dir.path());
    assert_eq!(
        pull_codex(store.path(), archive_dir.path()).sessions_captured,
        1
    );
    assert_eq!(
        pull_codex(store.path(), archive_dir.path()).sessions_found,
        0
    );
    let body_before = body_bytes(archive_dir.path(), MAIN_ID);

    let archived_dir = store.path().join("archived_sessions");
    let moved: Vec<String> = [&primary_name, &segment_name]
        .into_iter()
        .map(|name| {
            move_rollout(&day.join(name), &archived_dir.join(name))
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let stats = pull_codex(store.path(), archive_dir.path());
    assert_eq!(stats.sessions_found, 1);
    assert_eq!(stats.sessions_unchanged, 1);
    assert_eq!(stats.sessions_captured, 0);
    let recorded = sidecar(archive_dir.path(), MAIN_ID);
    assert_eq!(recorded.native_path, moved[0]);
    assert_eq!(
        recorded
            .source_files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        moved
    );
    assert_eq!(body_bytes(archive_dir.path(), MAIN_ID), body_before);
    assert_eq!(
        pull_codex(store.path(), archive_dir.path()).sessions_found,
        0
    );
}
