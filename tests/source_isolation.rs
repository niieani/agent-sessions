mod common;
use agent_sessions::{
    archive::{Archive, rebuild},
    render::render_all,
};
use serde_json::json;
use std::fs;
#[test]
fn unrelated_unreadable_sidecar_does_not_break_render_and_bad_source_does_not_abort_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let store = common::temp_store();
    common::pull(store.path(), archive.root());
    let root = archive.captures_dir().join("bad/chatgpt");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("unreadable.meta.json"), "not JSON").unwrap();
    assert!(render_all(&archive).is_ok());
    fs::write(root.join("catalog.jsonl.zst"),zstd::stream::encode_all(json!({"format":"catalog","origin":"https://chatgpt.com/backend-api/conversations","captured_at":1,"sha256":"bad","payload":"{}"}).to_string().as_bytes(),3).unwrap()).unwrap();
    let mut meta: serde_json::Value = serde_json::from_slice(
        &common::tree_bytes(&archive.captures_dir())
            .into_iter()
            .find(|(p, _)| p.ends_with(".meta.json") && p.starts_with("testhost/"))
            .unwrap()
            .1,
    )
    .unwrap();
    meta["source"] = json!("chatgpt");
    meta["nativeId"] = json!("catalog");
    meta["host"] = json!("bad");
    fs::write(root.join("unreadable.meta.json"), meta.to_string()).unwrap();
    fs::rename(
        root.join("unreadable.meta.json"),
        root.join("catalog.meta.json"),
    )
    .unwrap();
    let result = rebuild(&archive).expect("one Source preparation failure must be isolated");
    assert!(result.captures_failed > 0);
    assert!(result.conversations_extracted > 0);
}
