//! Capture-backed `info` lookup and selection behavior.

mod common;

use std::fs;

use agent_sessions::archive::Archive;
use agent_sessions::info::{format_human, inspect_session};

use common::{HOST, copy_tree, pull_codex, source_fixtures_dir};

const ID: &str = "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa";

fn pulled_archive() -> tempfile::TempDir {
    let store = tempfile::tempdir().unwrap();
    copy_tree(&source_fixtures_dir("codex").join("store"), store.path());
    let archive_dir = tempfile::tempdir().unwrap();
    pull_codex(store.path(), archive_dir.path());
    archive_dir
}

#[test]
fn inspects_codex_capture_with_transcript_and_source_details() {
    let archive_dir = pulled_archive();
    let archive = Archive::open(archive_dir.path()).unwrap();

    let info = inspect_session(&archive, ID, None, None).unwrap();
    assert_eq!(info.source, "codex");
    assert_eq!(info.capture_host, HOST);
    assert_eq!(info.title.as_deref(), Some("Dark mode settings"));
    let facts = info.transcript_facts.as_ref().unwrap();
    assert_eq!(facts.starting_model.as_deref(), Some("gpt-5"));
    assert_eq!(facts.starting_reasoning_effort.as_deref(), Some("high"));
    assert_eq!(facts.patched_files, ["/tmp/generated.md", "src/theme.ts"]);
    assert_eq!(info.source_details["initial"]["originator"], "codex_cli_rs");
    assert_eq!(
        info.source_details["configurationTimeline"][0]["model"],
        "gpt-5"
    );
    assert_eq!(info.source_details["latestUsage"]["type"], "token_count");

    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["transcriptFacts"]["startingModel"], "gpt-5");
    assert!(format_human(&info).contains("sourceDetails:"));
}

#[test]
fn lookup_requires_source_for_cross_source_collisions() {
    let archive_dir = pulled_archive();
    let codex_dir = archive_dir.path().join("captures").join(HOST).join("codex");
    let pi_dir = archive_dir.path().join("captures").join(HOST).join("pi");
    fs::create_dir_all(&pi_dir).unwrap();
    fs::copy(
        codex_dir.join(format!("{ID}.jsonl.zst")),
        pi_dir.join(format!("{ID}.jsonl.zst")),
    )
    .unwrap();
    let mut copied_meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(codex_dir.join(format!("{ID}.meta.json"))).unwrap(),
    )
    .unwrap();
    copied_meta["source"] = "pi".into();
    copied_meta["host"] = "other".into();
    fs::write(
        pi_dir.join(format!("{ID}.meta.json")),
        serde_json::to_string(&copied_meta).unwrap(),
    )
    .unwrap();

    let archive = Archive::open(archive_dir.path()).unwrap();
    let error = inspect_session(&archive, ID, None, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ambiguous across sources: codex, pi")
    );
    assert!(
        inspect_session(&archive, ID, None, Some(HOST))
            .unwrap_err()
            .to_string()
            .contains("ambiguous across sources: codex, pi")
    );
    assert_eq!(
        inspect_session(&archive, ID, Some("codex"), None)
            .unwrap()
            .source,
        "codex"
    );
}

#[test]
fn lookup_defaults_to_winner_and_honors_host_selection() {
    let archive_dir = pulled_archive();
    let original_dir = archive_dir.path().join("captures").join(HOST).join("codex");
    let richer_dir = archive_dir
        .path()
        .join("captures")
        .join("secondary")
        .join("codex");
    fs::create_dir_all(&richer_dir).unwrap();
    fs::copy(
        original_dir.join(format!("{ID}.jsonl.zst")),
        richer_dir.join(format!("{ID}.jsonl.zst")),
    )
    .unwrap();
    let mut richer_meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(original_dir.join(format!("{ID}.meta.json"))).unwrap(),
    )
    .unwrap();
    richer_meta["host"] = "secondary".into();
    richer_meta["sourceSize"] = (richer_meta["sourceSize"].as_u64().unwrap() + 1).into();
    fs::write(
        richer_dir.join(format!("{ID}.meta.json")),
        serde_json::to_string(&richer_meta).unwrap(),
    )
    .unwrap();

    let archive = Archive::open(archive_dir.path()).unwrap();
    let winner = inspect_session(&archive, ID, Some("codex"), None).unwrap();
    assert_eq!(winner.capture_host, "secondary");
    assert_eq!(winner.available_hosts, ["secondary", HOST]);
    assert_eq!(
        inspect_session(&archive, ID, Some("codex"), Some(HOST))
            .unwrap()
            .capture_host,
        HOST
    );
    assert!(inspect_session(&archive, ID, Some("codex"), Some("missing")).is_err());
}
