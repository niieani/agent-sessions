use agent_sessions::{chatgpt::presentation::interpret, model::ReferenceTarget};
use serde_json::{Value, json};
fn native(text: &str) -> Value {
    json!({"id":"answer","author":{"role":"assistant"},"content":{"parts":[text]},"metadata":{}})
}
#[test]
fn native_entity_formats_retain_supplied_names_from_multiple_categories() {
    let n = native(
        "Use entity[\"financial_account\",\"Health Savings Account\",0] and entity_metadata[\"local_business\",\"one-line\",\"Neighborhood Café\",0,{\"location\":[\"SF\"]}].",
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("Health Savings Account"),
        "{}",
        shown.text
    );
    assert!(shown.text.contains("Neighborhood Café"));
    assert!(!shown.text.contains(''));
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}
#[test]
fn product_names_are_read_from_the_native_format_not_inferred_from_refs() {
    let n = native(
        "product_entity[\"turn0product4\",\"Lamp A\"] products{\"selections\":[[\"turn0product2\",\"Lamp B\"],[\"turn0product9\",\"Lamp C\"]]} product{\"product_name\":\"Lamp D\",\"merchant_name\":\"Some store\",\"price\":\"$15\"} product[\"turn0product12\",\"Lamp E\",{\"render_as\":\"hero\"}]",
    );
    let shown = interpret(&n, "conversation").unwrap();
    for name in ["Lamp A", "Lamp B", "Lamp C", "Lamp D", "Lamp E"] {
        assert!(shown.text.contains(name), "{}", shown.text);
    }
    assert!(!shown.text.contains("Some store"));
    assert!(!shown.text.contains("turn0product"));
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}
#[test]
fn explicit_navigation_video_and_summary_titles_are_recovered() {
    let n = native(
        "navlistLocal newsturn0news3,turn0news5 videoHow to calibrateturn0search1 summaryTrain timetableturn0search6route",
    );
    let shown = interpret(&n, "conversation").unwrap();
    for title in ["Local news", "How to calibrate", "Train timetable"] {
        assert!(shown.text.contains(title), "{}", shown.text);
    }
    assert!(!shown.text.contains("turn0"));
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}
#[test]
fn url_marker_uses_only_its_explicit_safe_web_address() {
    let n = native(
        "urlRepository [docs]https://example.com/docs urlUnsafe sourcejavascript:neverExecute()",
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("Repository \\[docs\\]"),
        "{}",
        shown.text
    );
    assert!(
        shown
            .references
            .iter()
            .any(|r| r.target == ReferenceTarget::Url("https://example.com/docs".into()))
    );
    assert!(!shown.text.contains("javascript:"));
    assert!(
        shown.references.iter().all(
            |r| !matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("javascript:"))
        )
    );
    assert!(shown.text.contains("Unsafe source"));
    assert!(!shown.issues.is_empty());
}
#[test]
fn opaque_widgets_have_descriptive_types_and_native_map_points_keep_names() {
    let n = native(
        "image_group{\"query\":[\"Not an image title\"]} financeturn0finance0 forecastturn9forecast0 memcite genui{\"map_widget\":{\"points\":[{\"ref\":\"turn4business1\",\"name\":\"Central Library\"},{\"ref\":\"turn4business8\",\"name\":\"Riverside Park\"}]}}",
    );
    let shown = interpret(&n, "conversation").unwrap();
    for name in [
        "Image gallery",
        "Financial chart",
        "Weather forecast",
        "Memory citation",
        "Central Library",
        "Riverside Park",
    ] {
        assert!(shown.text.contains(name), "{}", shown.text);
    }
    assert!(!shown.text.contains("Not an image title"));
    assert!(!shown.text.contains("Unresolved source reference"));
}
#[test]
fn malformed_native_payloads_remain_findable_without_guessing_or_execution() {
    let n = native(
        "entity[notJSON,run()] products{\"selections\":[[\"ref-only\"]]} future_widget{\"execute\":\"run()\"}",
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Entity unavailable"), "{}", shown.text);
    assert!(shown.text.contains("Product selection unavailable"));
    assert!(shown.text.contains("Future widget"));
    assert!(!shown.text.contains("run()"));
    assert!(!shown.text.contains("ref-only"));
    assert!(!shown.issues.is_empty());
}
#[test]
fn marker_examples_in_code_and_user_messages_stay_exact() {
    let example = "entity[\"person\",\"Ada\",0]";
    let code = format!("Example `{example}`.\n```text\n{example}\n```");
    let n = native(&code);
    assert_eq!(interpret(&n, "conversation").unwrap().text, code);
    let mut user = native(example);
    user["author"]["role"] = json!("user");
    assert_eq!(interpret(&user, "conversation").unwrap().text, example);
}

#[test]
fn native_json_markers_are_not_treated_as_jsx_expressions_in_mixed_rich_messages() {
    let mut n =
        native("<text>Options:</text> products{\"selections\":[[\"ref-a\",\"Fixture <box>\"]]}");
    n["metadata"]["model_dil_v2"] = json!({"appData":{"opGenui":{"modelDataBindings":{}}}});
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Fixture &lt;box&gt;"), "{}", shown.text);
    assert!(!shown.text.contains("Expression unavailable"));
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}

#[test]
fn fallback_markdown_uses_the_same_native_marker_interpretation_as_message_text() {
    let mut n = native("");
    n["metadata"]["model_dil_v2"] = json!({"fallbackMarkdown":"Choose entity[\"place\",\"Riverside Park\",0].","appData":{"opGenui":{"modelDataBindings":{}}}});
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Riverside Park"), "{}", shown.text);
    assert!(!shown.text.contains(''), "{}", shown.text);
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn legacy_highlight_controls_leave_words_citations_and_literal_code_intact() {
    let n = native(
        "Claim A. Claim B. citeturn0search1\n\n`literal`\n\n```text\ncode\n```\n\n    indented",
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown
            .text
            .starts_with("Claim A. Claim B. [Unresolved citation]"),
        "{}",
        shown.text
    );
    assert!(shown.text.contains("`literal`"));
    assert!(shown.text.contains("```text\ncode\n```"));
    assert!(shown.text.contains("    indented"));
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn source_authored_user_highlight_characters_remain_verbatim() {
    let mut n = native("User example: text");
    n["author"]["role"] = json!("user");
    let shown = interpret(&n, "conversation").unwrap();
    assert_eq!(shown.text, "User example: text");
    assert!(shown.artifacts.is_empty());
}
