use agent_sessions::{
    chatgpt::{Evidence, Graph},
    model::Provenance,
};
use serde_json::json;

#[test]
fn hidden_native_content_is_actions_and_audio_keeps_only_transcription() {
    let messages = vec![
        json!({"id":"u","author":{"role":"user"},"content":{"content_type":"text","parts":["question"]}}),
        json!({"id":"t","author":{"role":"assistant"},"content":{"content_type":"thoughts","thoughts":[{"content":"secret"}]}}),
        json!({"id":"r","author":{"role":"assistant"},"content":{"content_type":"reasoning_recap","content":"secret"}}),
        json!({"id":"m","author":{"role":"assistant"},"content":{"content_type":"model_editable_context","model_set_context":"memory"}}),
        json!({"id":"v","author":{"role":"user"},"content":{"content_type":"multimodal_text","parts":[{"content_type":"audio_asset_pointer","asset_pointer":"sediment://audio"},{"content_type":"text","text":"transcribed words"}]}}),
        json!({"id":"a","author":{"role":"assistant"},"content":{"content_type":"text","parts":["{\"actual_reply\":true}"]}}),
    ];
    let graph = Graph::from_evidence(
        "c",
        &[Evidence {
            format: "browser".into(),
            origin: "https://chatgpt.com/backend-api/conversations/c".into(),
            captured_at: 1,
            sha256: "fixture".into(),
            payload: json!({"update_time":1,"messages":messages}).to_string(),
        }],
    )
    .unwrap();
    let conversation = graph
        .conversation(Provenance {
            source: "chatgpt".into(),
            host: "browser-host".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    assert_eq!(
        conversation
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["question", "transcribed words", "{\"actual_reply\":true}"]
    );
    assert_eq!(conversation.messages[1].actions_before, 3);
}

#[test]
fn canonical_text_preserves_native_citations_without_render_aliases() {
    let native = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["Answer citeturn0search0"]},"metadata":{"content_references":[{"type":"webpage","url":"https://example.com","title":"Example","matched_text":"citeturn0search0","start_idx":7,"end_idx":30}]}});
    let graph = Graph::from_evidence(
        "c",
        &[Evidence {
            format: "browser".into(),
            origin: "https://chatgpt.com/backend-api/conversations/c".into(),
            captured_at: 1,
            sha256: "fixture".into(),
            payload: json!({"update_time":1,"messages":[native]}).to_string(),
        }],
    )
    .unwrap();
    let conversation = graph
        .conversation(Provenance {
            source: "chatgpt".into(),
            host: "host".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    assert_eq!(conversation.messages[0].text, "Answer citeturn0search0");
}

#[test]
fn starting_model_uses_first_visible_assistant_evidence() {
    let graph=Graph::from_evidence("c",&[Evidence{format:"browser".into(),origin:"https://chatgpt.com/backend-api/conversations/c".into(),captured_at:1,sha256:"fixture".into(),payload:json!({"update_time":1,"default_model_slug":"current-default","messages":[{"id":"hidden","author":{"role":"assistant"},"content":{"content_type":"thoughts"},"metadata":{"model_slug":"internal"}},{"id":"first","author":{"role":"assistant"},"content":{"parts":["Reply"]},"metadata":{"model_slug":"original-model"}},{"id":"later","author":{"role":"assistant"},"content":{"parts":["Later"]},"metadata":{"model_slug":"later-model"}}]}).to_string()}]).unwrap();
    let conversation = graph
        .conversation(Provenance {
            source: "chatgpt".into(),
            host: "host".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    assert_eq!(
        conversation.transcript_facts.starting_model.as_deref(),
        Some("original-model")
    );
}
