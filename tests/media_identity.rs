#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    archive::Archive,
    media::MediaStore,
    pull::{StoreRoots, pull_localhost},
    render::render_all,
};
use base64::Engine;
use serde_json::json;
#[test]
fn same_timestamp_image_messages_keep_distinct_bytes_and_missing_objects_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let store = temp.path().join("claude/project");
    std::fs::create_dir_all(&store).unwrap();
    let mut rows = String::new();
    let mut ids = Vec::new();
    for (index, bytes) in [b"first".as_slice(), b"second".as_slice()]
        .into_iter()
        .enumerate()
    {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        rows.push_str(&json!({"type":"user","uuid":format!("m{index}"),"sessionId":"11111111-1111-4111-8111-111111111111","timestamp":"2026-02-02T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Image"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":encoded}}]}}).to_string());
        rows.push('\n');
        ids.push(
            MediaStore::new(archive.root())
                .put(bytes, "image/png")
                .unwrap(),
        );
    }
    std::fs::write(
        store.join("11111111-1111-4111-8111-111111111111.jsonl"),
        rows,
    )
    .unwrap();
    let empty = temp.path().join("empty");
    let roots = StoreRoots {
        claude: temp.path().join("claude"),
        codex: empty.clone(),
        pi: empty.clone(),
        cursor: empty.clone(),
        t3: empty.clone(),
        chatgpt: empty,
        t3_explicit: false,
    };
    assert_eq!(
        pull_localhost(&archive, &roots, "host")
            .unwrap()
            .sessions_failed,
        0
    );
    render_all(&archive).unwrap();
    let text = shared::text(&archive);
    for id in &ids {
        assert_eq!(text.matches(&id.sha256).count(), 1, "{text}");
    }
    let object = MediaStore::new(archive.root()).object(&ids[1]);
    std::fs::rename(&object, temp.path().join("missing-object")).unwrap();
    render_all(&archive).unwrap();
    let text = shared::text(&archive);
    assert!(text.contains("Missing image"));
    assert!(!text.contains(&ids[1].sha256));
}
