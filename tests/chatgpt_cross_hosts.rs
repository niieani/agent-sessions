#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    archive::{Archive, rebuild},
    chatgpt::{self},
    pull::{StoreRoots, pull_localhost},
    render,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
fn store(root: &Path, revision: i64, tail: &str) {
    fs::create_dir_all(root).unwrap();
    let message = |id: &str| json!({"id":id,"create_time":1770000000,"author":{"role":"user"},"content":{"parts":[id]}});
    let payload=json!({"update_time":revision,"messages":[message("u"),message(tail)],"page_info":{"has_previous_page":false}}).to_string();
    fs::write(root.join("c.json"), &payload).unwrap();
    fs::write(root.join("c.meta.json"),json!({"accountId":"same-account","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":format!("2026-10-19T00:00:0{revision}Z"),"sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
}
#[test]
fn contributing_hosts_preserve_all_branches_in_database_status_and_render() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    for (host, revision, tail) in [("a", 1, "old"), ("z", 2, "new")] {
        let input = temp.path().join(host);
        store(&input, revision, tail);
        let missing = temp.path().join("missing");
        let roots = StoreRoots {
            chatgpt: input,
            claude: missing.clone(),
            codex: missing.clone(),
            pi: missing.clone(),
            cursor: missing.clone(),
            t3: missing,
            t3_explicit: false,
        };
        assert_eq!(
            pull_localhost(&archive, &roots, host)
                .unwrap()
                .sessions_failed,
            0
        );
    }
    let status = chatgpt::status(&archive).unwrap();
    assert_eq!(
        status["conversations"]["c"]["messageIds"],
        json!(["new", "old", "u"])
    );
    render::render_all(&archive).unwrap();
    let before = shared::text(&archive);
    assert!(before.contains("\nold\n") && before.contains("\nnew\n"));
    assert_eq!(rebuild(&archive).unwrap().captures_failed, 0);
    render::render_all(&archive).unwrap();
    assert_eq!(shared::text(&archive), before);
    // Captures stay independently attributable; aggregation never rewrites them.
    for host in ["a", "z"] {
        let meta = archive
            .captures_dir()
            .join(host)
            .join("chatgpt/c.meta.json");
        assert!(meta.is_file());
    }
}
