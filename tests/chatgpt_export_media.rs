#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{archive::Archive, chatgpt};
use std::path::Path;
#[test]
fn export_extracts_only_explicitly_generated_images_and_reports_missing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let zip =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chatgpt/generated-media.zip");
    chatgpt::ingest_export(&archive, &zip, "fixture-account").unwrap();
    let status = chatgpt::status(&archive).unwrap();
    let known = &status["conversations"]["conv"];
    assert_eq!(
        known["generatedAssetIds"],
        serde_json::json!(["file_generated", "file_missing"])
    );
    assert_eq!(
        known["savedGeneratedAssetIds"],
        serde_json::json!(["file_generated"])
    );
    assert_eq!(
        known["missingGeneratedAssetIds"],
        serde_json::json!(["file_missing"])
    );
    assert_eq!(known["mediaComplete"], false);
    use agent_sessions::adapters::Adapter;
    let capture = archive
        .lookup_capture("conv", Some("chatgpt"), None)
        .unwrap()
        .capture;
    let mut fixture = chatgpt::ChatGptAdapter.extract(&capture).unwrap().unwrap();
    for message in &mut fixture.messages {
        message.timestamp_ms = Some(1_770_000_000_000);
    }
    archive
        .db()
        .unwrap()
        .upsert_conversation_from_capture(&fixture, &capture.meta)
        .unwrap();
    agent_sessions::render::render_all(&archive).unwrap();
    let text = shared::text(&archive);
    assert!(text.contains("Generated images"));
    assert!(text.contains("![Generated image](../../media/sha256/"));
    assert!(text.contains("file_missing"));
    assert!(!text.contains("file_uploaded"));
    let before = std::fs::read_dir(archive.root().join("media/objects"))
        .unwrap()
        .count();
    chatgpt::ingest_export(&archive, &zip, "fixture-account").unwrap();
    assert_eq!(
        before,
        std::fs::read_dir(archive.root().join("media/objects"))
            .unwrap()
            .count()
    );
    let rebuilt = agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(rebuilt.captures_failed, 0);
    assert_eq!(
        chatgpt::status(&archive).unwrap()["conversations"]["conv"]["generatedAssetIds"],
        known["generatedAssetIds"]
    );
}
#[test]
fn media_metadata_alone_cannot_claim_conversation_history_complete() {
    let row = serde_json::json!({"file_id":"file_image","origination_thread_id":"conv","origination_message_id":"missing","image_gen_generation_id":"gen"});
    let graph = chatgpt::Graph::from_evidence(
        "conv",
        &[chatgpt::Evidence {
            format: "export-media-index".into(),
            origin: "fixture".into(),
            captured_at: 0,
            sha256: "unused".into(),
            payload: row.to_string(),
        }],
    )
    .unwrap();
    assert!(graph.validate().is_err());
}
#[test]
fn observed_preview_is_preserved_but_does_not_establish_original_media_completeness() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir_all(input.join("assets")).unwrap();
    let bytes = b"observed preview";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = format!("assets/{hash}.png");
    std::fs::write(input.join(&path), bytes).unwrap();
    std::fs::write(input.join("assets/preview.meta.json"),serde_json::json!({"mime":"image/png","conversationId":"conv","capturedAt":"2026-10-19T00:00:00Z","format":"chatgpt-generated-image-v1","assetId":"file_image","sha256":hash,"bytes":bytes.len(),"path":path,"quality":"observed"}).to_string()).unwrap();
    let payload=serde_json::json!({"update_time":1,"messages":[{"id":"image","author":{"role":"tool"},"content":{"parts":[{"content_type":"image_asset_pointer","asset_pointer":"sediment://file_image","metadata":{"generation":{}}}]}}],"page_info":{"has_previous_page":false}}).to_string();
    std::fs::write(input.join("c.json"), &payload).unwrap();
    std::fs::write(input.join("c.meta.json"),serde_json::json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/conv","sha256":format!("{:x}",Sha256::digest(payload.as_bytes())),"capturedAt":"2026-10-15T00:00:00Z"}).to_string()).unwrap();
    shared::collect(&archive, &input).unwrap();
    let status = chatgpt::status(&archive).unwrap();
    assert_eq!(status["conversations"]["conv"]["mediaComplete"], false);
    assert_eq!(
        status["conversations"]["conv"]["missingGeneratedAssetIds"],
        serde_json::json!(["file_image"])
    );
}

#[test]
fn orphan_library_rows_survive_import_without_sessions_or_checkpoint_blockers() {
    use serde_json::json;
    use std::{fs, process::Command};
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("conversations.json"), "[]").unwrap();
    let rows = json!([
        {"file_id":"file_orphan","origination_thread_id":null,"image_gen_generation_id":"generation"},
        {"file_id":"file_missing_chat","origination_thread_id":"absent","origination_message_id":"missing","image_gen_generation_id":"generation"}
    ]);
    fs::write(temp.path().join("library_files.json"), rows.to_string()).unwrap();
    let zip = temp.path().join("orphan.zip");
    assert!(
        Command::new("/usr/bin/zip")
            .current_dir(temp.path())
            .args(["-q"])
            .arg(&zip)
            .args(["conversations.json", "library_files.json"])
            .status()
            .unwrap()
            .success()
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let stats = chatgpt::ingest_export(&archive, &zip, "account").unwrap();
    assert!(stats.failed.is_empty(), "{:?}", stats.failed);
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        chatgpt::status(&archive).unwrap()["conversations"],
        json!({})
    );
    assert_eq!(
        agent_sessions::archive::rebuild(&archive)
            .unwrap()
            .captures_failed,
        0
    );
    let mut payloads = String::new();
    for p in fs::read_dir(
        archive
            .captures_dir()
            .join(agent_sessions::pull::local_hostname())
            .join("chatgpt"),
    )
    .unwrap()
    .map(|e| e.unwrap().path())
    .filter(|p| p.extension().is_some_and(|s| s == "zst"))
    {
        payloads.push_str(
            &String::from_utf8(zstd::stream::decode_all(fs::File::open(p).unwrap()).unwrap())
                .unwrap(),
        );
    }
    for asset in ["file_orphan", "file_missing_chat"] {
        assert!(payloads.contains(asset));
    }
}
