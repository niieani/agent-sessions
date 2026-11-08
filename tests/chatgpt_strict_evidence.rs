#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    adapters::{Adapter, EnumerateGate},
    archive::Archive,
    chatgpt::ChatGptAdapter,
};
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn browser_evidence_requires_capture_time_and_hash() {
    for missing in ["capturedAt", "sha256"] {
        let temp = tempfile::tempdir().unwrap();

        let input = temp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        let payload=json!({"update_time":1,"messages":[{"id":"u","author":{"role":"user"},"content":{"parts":["hello"]}}]}).to_string();
        let mut meta = json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))});
        meta.as_object_mut().unwrap().remove(missing);
        std::fs::write(input.join("c.json"), payload).unwrap();
        std::fs::write(input.join("c.meta.json"), meta.to_string()).unwrap();
        let error = ChatGptAdapter
            .enumerate(&input, &EnumerateGate::default())
            .unwrap_err();
        assert!(format!("{error:#}").contains(missing), "{error:#}");
    }
}

#[test]
fn invalid_graph_is_rejected_before_capture_publication() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let input = temp.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let payload = json!({"update_time":1,"messages":[{"content":{"parts":["missing identity"]}}]})
        .to_string();
    std::fs::write(input.join("c.json"), &payload).unwrap();
    std::fs::write(input.join("c.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
    let result = shared::collect(&archive, &input).unwrap();
    assert!(result.sessions_failed > 0);
    assert!(archive.lookup_capture("c", Some("chatgpt"), None).is_err());
}
