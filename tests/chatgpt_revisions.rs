#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    archive::Archive,
    chatgpt::{self, Evidence, Graph},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn message(id: &str, text: &str, status: &str) -> Value {
    json!({"id":id,"create_time":1770000000,"author":{"role":"assistant"},"channel":"commentary","recipient":"all","content":{"parts":[text]},"status":status})
}
fn evidence(payload: Value, time: i64, versions: bool) -> Evidence {
    Evidence {
        format: "browser".into(),
        origin: format!(
            "https://chatgpt.com/backend-api/conversations/c{}",
            if versions {
                "/versions?message_id=u"
            } else {
                ""
            }
        ),
        captured_at: time,
        sha256: "fixture".into(),
        payload: payload.to_string(),
    }
}
#[test]
fn newer_native_message_wins_across_selected_and_version_responses() {
    for (selected_time, version_time, expected) in
        [(3, 2, "selected"), (2, 3, "version"), (3, 3, "selected")]
    {
        let user = json!({"id":"u","author":{"role":"user"},"content":{"parts":["prompt"]}});
        let records = [
            evidence(
                json!([[user.clone(), message("a", "version", "in_progress")]]),
                version_time,
                true,
            ),
            evidence(
                json!({"update_time":1,"messages":[user,message("a","selected","finished_successfully")],"page_info":{"has_previous_page":false}}),
                selected_time,
                false,
            ),
        ];
        let graph = Graph::from_evidence("c", &records).unwrap();
        assert_eq!(
            graph.nodes["a"].message.as_ref().unwrap()["content"]["parts"][0],
            expected
        );
        assert_eq!(graph.selected_ids().unwrap(), ["u", "a"]);
    }
}
#[test]
fn current_hidden_thought_or_tool_stream_also_blocks_checkpoint() {
    for role in ["assistant", "tool"] {
        let mut pending = message("active", "Native partial content", "in_progress");
        pending["author"]["role"] = json!(role);
        pending["channel"] = json!("analysis");
        pending["metadata"] = json!({"is_visually_hidden_from_conversation":true});
        let graph = Graph::from_evidence(
            "c",
            &[evidence(
                json!({"update_time":1,"messages":[pending],"page_info":{"has_previous_page":false}}),
                1,
                false,
            )],
        )
        .unwrap();
        graph.validate().unwrap();
        assert_eq!(graph.in_progress_message_ids(), ["active"]);
    }
}
#[test]
fn only_current_streaming_message_blocks_checkpoint_without_discarding_text() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    for (time, status, expected) in [
        (1, "in_progress", false),
        (2, "finished_successfully", true),
    ] {
        let payload=json!({"update_time":1,"messages":[message("old","Historical commentary","in_progress"),message("current","Preserved text",status)],"page_info":{"has_previous_page":false}}).to_string();
        std::fs::write(input.join("capture.json"), &payload).unwrap();
        std::fs::write(input.join("capture.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/c","sha256":format!("{:x}",Sha256::digest(payload.as_bytes())),"capturedAt":format!("2026-10-15T00:00:0{time}Z")}).to_string()).unwrap();
        assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
        let status = chatgpt::status(&archive).unwrap();
        let known = &status["conversations"]["c"];
        assert_eq!(known["complete"], true);
        assert_eq!(known["checkpointEligible"], expected);
        assert_eq!(
            known["inProgressMessageIds"],
            if expected {
                json!([])
            } else {
                json!(["current"])
            }
        );
        agent_sessions::render::render_all(&archive).unwrap();
        assert!(shared::text(&archive).contains("Preserved text"));
    }
}
