//! The `asessions-chatgpt-status-v1` per-conversation record is the collector's
//! `known` input. It must expose only what the collector consumes, and nothing
//! in it may be read as a list of Versions request targets.
#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{archive::Archive, chatgpt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

fn message(id: &str, role: &str, advertised: bool) -> Value {
    json!({"id":id,"create_time":1770000000,"author":{"role":role},"content":{"parts":[id]},
        "status":"finished_successfully","metadata":{"has_versions":advertised}})
}
fn observe(store: &Path, name: &str, url: &str, payload: &Value, second: u8) {
    let body = payload.to_string();
    std::fs::write(store.join(format!("{name}.json")), &body).unwrap();
    std::fs::write(
        store.join(format!("{name}.meta.json")),
        json!({"accountId":"fixture-account","url":url,
            "sha256":format!("{:x}",Sha256::digest(body.as_bytes())),
            "capturedAt":format!("2026-10-15T00:00:0{second}Z")})
        .to_string(),
    )
    .unwrap();
}
fn known(archive: &Archive, store: &Path) -> Value {
    assert_eq!(shared::collect(archive, store).unwrap().sessions_failed, 0);
    chatgpt::status(archive).unwrap()["conversations"]["c"].clone()
}

#[test]
fn status_reports_only_unfetched_advertised_versions_and_no_sibling_targets() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let detail = "https://chatgpt.com/backend-api/conversations/c";
    // Selected path u -> a; the server advertises alternate Versions on `a`.
    observe(
        &store,
        "detail",
        detail,
        &json!({"update_time":1,"messages":[message("u","user",false),message("a","assistant",true)],
            "page_info":{"has_previous_page":false}}),
        1,
    );
    let before = known(&archive, &store);
    assert_eq!(before["missingVersions"], json!(["a"]));
    assert_eq!(before["checkpointEligible"], false);

    // The Versions response reveals deselected sibling `b`. It is preserved
    // evidence, never a Versions target: the server rejects off-path IDs.
    observe(
        &store,
        "versions",
        &format!("{detail}/versions?message_id=a"),
        &json!([
            [message("a", "assistant", true)],
            [message("b", "assistant", false)]
        ]),
        2,
    );
    let after = known(&archive, &store);
    assert_eq!(after["missingVersions"], json!([]));
    assert_eq!(after["checkpointEligible"], true);
    assert_eq!(after["messageIds"], json!(["a", "b", "u"]));

    let fields = after
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fields,
        BTreeSet::from([
            "updatedAt",
            "messageIds",
            "complete",
            "missingVersions",
            "exportCoveredVersionIds",
            "inProgressMessageIds",
            "checkpointEligible",
            "title",
            "generatedAssetIds",
            "generatedAssets",
            "savedGeneratedAssetIds",
            "missingGeneratedAssetIds",
            "mediaComplete",
        ])
    );
}
