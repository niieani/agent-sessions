use agent_sessions::chatgpt::{self, presentation::interpret};
use serde_json::json;

#[test]
fn visualization_source_numbers_keep_decimal_precision_without_float_conversion() {
    let source = r#"{"id":"rich","author":{"role":"assistant"},"content":{"parts":["Original dialogue"]},"metadata":{"model_dil_v2":{"constants":{"crop":0.22916666666666666,"decimal":0.123456789012345678901234567890,"integer":184467440737095516161234567890}}}}"#;
    let native = serde_json::from_str(source).unwrap();
    let shown = interpret(&native, "conv").unwrap();
    assert!(shown.artifacts.is_empty());
    for (key, expected) in [
        ("crop", "0.22916666666666666"),
        ("decimal", "0.123456789012345678901234567890"),
        ("integer", "184467440737095516161234567890"),
    ] {
        assert_eq!(
            native["metadata"]["model_dil_v2"]["constants"][key].to_string(),
            expected
        );
    }
}
#[test]
fn rich_evidence_preserves_original_dialogue_and_metadata_only_messages() {
    let original = "Original dialogue.\r\nSecond line.";
    let native = json!({"id":"first","author":{"role":"assistant"},"content":{"parts":[original]},"metadata":{"model_dil_v2":{"code":"neverExecute()","fallbackMarkdown":"different fallback"}}});
    let shown = interpret(&native, "conv").unwrap();
    assert_eq!(shown.text.matches(original).count(), 1);
    assert!(!shown.text.contains("different fallback"));
    assert!(shown.artifacts.is_empty());
    let empty = json!({"id":"empty","author":{"role":"assistant"},"content":{"parts":[""]},"metadata":{"model_dil_v2":{"code":"neverExecute()","fallbackMarkdown":"Fallback narrative"}}});
    let shown = interpret(&empty, "conv").unwrap();
    assert!(shown.text.contains("Fallback narrative"));
    assert!(shown.artifacts.is_empty());
}
#[test]
fn rich_assets_obey_source_message_visibility() {
    let chart =
        json!({"type":"client_defined_widget","data":{"content":{"chartType":"bar","data":[1,2]}}});
    let visible = json!({"id":"visible","author":{"role":"assistant"},"content":{"parts":["chart"]},"metadata":{"content_references":[chart]}});
    let hidden = json!({"id":"hidden","author":{"role":"assistant"},"channel":"analysis","content":{"parts":["internal"]},"metadata":{"model_dil_v2":{"code":"internal()"}}});
    let graph = chatgpt::Graph::from_evidence(
        "conv",
        &[chatgpt::Evidence {
            format: "browser".into(),
            origin: "https://chatgpt.com/backend-api/conversations/conv".into(),
            captured_at: 0,
            sha256: "fixture".into(),
            payload: json!({"update_time":1,"messages":[visible,hidden],"page_info":{"has_previous_page":false}})
                .to_string(),
        }],
    )
    .unwrap();
    let archive = tempfile::tempdir().unwrap();
    let material = chatgpt::presentation::material(&graph, archive.path()).unwrap();
    assert!(material.assets.is_empty());
    assert_eq!(material.messages.len(), 1);
    assert_eq!(material.messages[0].original.text, "chart");
}
