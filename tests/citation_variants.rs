use agent_sessions::chatgpt::presentation::{Presentation, interpret};
use agent_sessions::model::ReferenceTarget;
use serde_json::{Value, json};
fn native(text: &str, groups: Value) -> Value {
    json!({"id":"answer","author":{"role":"assistant"},"content":{"parts":[text]},"metadata":{"content_references":groups}})
}
fn item(kind: &str, index: u64, title: &str) -> Value {
    json!({"title":title,"url":format!("https://example.com/{title}"),"refs":[{"turn_index":1,"ref_type":kind,"ref_index":index}]})
}
fn source_link(shown: &Presentation, title: &str, url: &str) -> String {
    let reference = shown
        .references
        .iter()
        .find(|r| r.target == ReferenceTarget::Url(url.into()))
        .expect("Resolved source must be registered for rendering");
    assert_eq!(reference.label, title);
    reference.markdown("")
}
#[test]
fn every_positional_reference_of_one_metadata_item_maps_to_its_source() {
    let source = json!({"title":"Source","url":"https://example.com/source","refs":[{"turn_index":1,"ref_type":"view","ref_index":1},{"turn_index":1,"ref_type":"view","ref_index":0}]});
    let message = native(
        "See citeturn247680view1turn247680view0.",
        json!([{"type":"grouped_webpages","items":[source]}]),
    );
    let shown = interpret(&message, "conversation").unwrap();
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert_eq!(
        shown
            .text
            .matches(&source_link(&shown, "Source", "https://example.com/source"))
            .count(),
        1
    );
}
#[test]
fn multiple_item_groups_keep_each_reference_attached_to_its_item() {
    let first = json!({"title":"First","url":"https://example.com/first","refs":[{"turn_index":1,"ref_type":"view","ref_index":1},{"turn_index":1,"ref_type":"view","ref_index":0}]});
    let message = native(
        "See citeturn999view1turn999view0turn888search2.",
        json!([{"type":"grouped_webpages","items":[first,item("search",2,"Second")]}]),
    );
    let shown = interpret(&message, "conversation").unwrap();
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert!(shown.text.contains(&format!(
        "{}, {}",
        source_link(&shown, "First", "https://example.com/first"),
        source_link(&shown, "Second", "https://example.com/Second")
    )));
}
#[test]
fn line_locators_resolve_sources_without_mutating_saved_native_message() {
    let mut message = native(
        "Read citeturn11view0L326-L327 and citeturn11view0L328-L330.",
        json!([{"type":"grouped_webpages","items":[{"title":"Manual","url":"https://example.com/manual","refs":[{"turn_index":11,"ref_type":"view","ref_index":0}]}]}]),
    );
    message["metadata"]["model_dil_v2"] = json!({"code":"preserved evidence"});
    let shown = interpret(&message, "conversation").unwrap();
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert_eq!(
        shown
            .text
            .matches(&source_link(&shown, "Manual", "https://example.com/manual"))
            .count(),
        2
    );
    assert!(shown.artifacts.is_empty());
}
#[test]
fn line_locators_also_support_verified_export_turn_renumbering() {
    let message = native(
        "Read citeturn999view0L326-L327.",
        json!([{"type":"grouped_webpages","items":[item("view",0,"Manual")]}]),
    );
    let shown = interpret(&message, "conversation").unwrap();
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert!(
        shown
            .text
            .contains(&source_link(&shown, "Manual", "https://example.com/Manual"))
    );
}
#[test]
fn code_example_markers_do_not_participate_in_positional_group_alignment() {
    let fenced = "```text\nciteturn111view9\n```";
    let inline = "`citeturn222search7`";
    let message = native(
        &format!("{fenced}\nLiteral {inline}.\nActual citeturn999view0."),
        json!([{"type":"grouped_webpages","items":[item("view",0,"Manual")]}]),
    );
    let shown = interpret(&message, "conversation").unwrap();
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
    assert!(shown.text.contains(fenced));
    assert!(shown.text.contains(inline));
    assert!(
        shown
            .text
            .contains(&source_link(&shown, "Manual", "https://example.com/Manual"))
    );
}
#[test]
fn renumbered_mapping_rejects_wrong_group_order_kind_or_reference_index() {
    for text in [
        "citeturn999search1 citeturn999view0",
        "citeturn999view0 citeturn999search8",
    ] {
        let message = native(
            text,
            json!([{"type":"grouped_webpages","items":[item("view",0,"First")]},{"type":"grouped_webpages","items":[item("search",1,"Second")]}]),
        );
        let shown = interpret(&message, "conversation").unwrap();
        assert!(!shown.issues.is_empty());
        assert!(!shown.references.iter().any(|r| matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("https://example.com/"))));
    }
}
