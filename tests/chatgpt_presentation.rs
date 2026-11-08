use agent_sessions::chatgpt::presentation::{Presentation, interpret};
use agent_sessions::model::ReferenceTarget;
use serde_json::{Value, json};

fn message(text: &str, metadata: Value) -> Value {
    json!({"id":"answer","author":{"role":"assistant"},"content":{"parts":[text]},"metadata":metadata})
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
fn citations_use_saved_sources_without_exposing_native_markers() {
    let native = message(
        "Read this. citeturn12view0turn12search1",
        json!({"content_references":[
            {"type":"grouped_webpages","items":[
                {"title":"First source","url":"https://example.com/one","refs":[{"turn_index":12,"ref_type":"view","ref_index":0}]},
                {"title":"Second source","url":"https://example.com/two","refs":[{"turn_index":12,"ref_type":"search","ref_index":1}]}
            ]}
        ]}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(shown.text.contains(&source_link(
        &shown,
        "First source",
        "https://example.com/one"
    )));
    assert!(shown.text.contains(&source_link(
        &shown,
        "Second source",
        "https://example.com/two"
    )));
    assert!(!shown.text.contains(''));
    assert!(shown.issues.is_empty());
}

#[test]
fn export_rebased_citations_require_matching_reference_shapes_and_order() {
    let native = message(
        "A citeturn349222view0. B citeturn349222view1.",
        json!({"content_references":[
            {"type":"grouped_webpages","items":[{"title":"A","url":"https://example.com/a","refs":[{"turn_index":2,"ref_type":"view","ref_index":0}]}]},
            {"type":"grouped_webpages","items":[{"title":"B","url":"https://example.com/b","refs":[{"turn_index":2,"ref_type":"view","ref_index":1}]}]}
        ]}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(
        shown
            .text
            .contains(&source_link(&shown, "A", "https://example.com/a"))
    );
    assert!(
        shown
            .text
            .contains(&source_link(&shown, "B", "https://example.com/b"))
    );
    let wrong = message("A citeturn349222view8.", native["metadata"].clone());
    let shown = interpret(&wrong, "conversation").unwrap();
    assert!(
        !shown
            .references
            .iter()
            .any(|r| r.target == ReferenceTarget::Url("https://example.com/a".into()))
    );
    assert!(!shown.issues.is_empty());
    assert!(shown.references.iter().any(
        |r| matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("asessions-capture:"))
    ));
}

#[test]
fn rich_bindings_are_generic_and_unknown_values_link_to_native_evidence() {
    let text = "<text>Sensor: <Entity category=\"sensor\" ref=\"sensor7\" value={sensor7.label}/>. Reading: {sensor7.reading}.</text>";
    let native = message(
        text,
        json!({"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{"sensor7":{"label":"Outdoor","reading":18.25}}}}}}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(
        shown
            .text
            .contains("Sensor: [Outdoor](asessions-reference:")
    );
    assert!(shown.text.contains("Reading: 18.25."));
    assert!(!shown.text.contains("<Entity"));
    assert!(!shown.text.contains("{sensor7"));
    let missing = message(
        text,
        json!({"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{}}}}}),
    );
    let shown = interpret(&missing, "conversation").unwrap();
    assert!(!shown.text.contains("{sensor7"));
    assert!(!shown.issues.is_empty());
    assert!(shown.artifacts.is_empty());
    assert!(shown.references.iter().any(
        |r| matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("asessions-capture:"))
    ));
}

#[test]
fn code_examples_and_user_messages_are_not_interpreted_as_components() {
    let text = "Example:\n```tsx\n<Entity value={sensor7.label}/>\n```\nInline `{sensor7.reading}` stays.\n~~~svg\n<svg onload=\"example()\"></svg>\n~~~";
    let native = message(text, json!({"model_dil_v2":{"appData":{}}}));
    let shown = interpret(&native, "conversation").unwrap();
    assert!(shown.text.starts_with(text));
    assert!(
        !shown
            .artifacts
            .iter()
            .any(|a| a.relative_path.extension().is_some_and(|x| x == "svg"))
    );
    let mut user = native;
    user["author"]["role"] = json!("user");
    user["content"]["parts"] = json!(["<Entity value={sensor7.label}/> citeunknown"]);
    assert_eq!(
        interpret(&user, "conversation").unwrap().text,
        user["content"]["parts"][0].as_str().unwrap()
    );
}

#[test]
fn svg_illustration_becomes_local_image_and_preserves_original_in_native_json() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50" onload="neverExecute()"><script>neverExecute()</script><foreignObject><div>bad</div></foreignObject><text x="3" y="20" fontSize="12" textAnchor="middle">Desk</text><image href="https://example.com/tracker"/></svg>"##;
    let native = message(
        &format!("Diagram:\n{svg}\nDone."),
        json!({"model_dil_v2":{"code":"neverExecute()"}}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(shown.text.contains("![Illustration]("));
    assert!(!shown.text.contains("<svg"));
    let safe = shown
        .artifacts
        .iter()
        .find(|a| a.relative_path.extension().is_some_and(|x| x == "svg"))
        .unwrap();
    let safe = String::from_utf8(safe.bytes.clone()).unwrap();
    assert!(safe.contains("font-size=\"12\""));
    assert!(safe.contains("text-anchor=\"middle\""));
    for forbidden in [
        "onload",
        "script",
        "foreignObject",
        "https://example.com/tracker",
    ] {
        assert!(!safe.contains(forbidden), "{safe}");
    }
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn unsupported_components_remain_available_without_executing_generated_code() {
    let native = message(
        "Before. <UnknownWidget data={{arbitrary: true}}/> After.",
        json!({"model_dil_v2":{"code":"throw new Error('do not execute')","constants":{"x":1}}}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(shown.text.contains("Before."));
    assert!(shown.text.contains("After."));
    assert!(!shown.text.contains("<UnknownWidget"));
    assert!(!shown.issues.is_empty());
    assert!(shown.artifacts.is_empty());
    assert!(shown.references.iter().any(
        |r| matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("asessions-capture:"))
    ));
}

#[test]
fn structured_native_parts_link_out_instead_of_dumping_json_into_dialogue() {
    let mut native = message("", json!({}));
    native["content"]["parts"] = json!(["Here is the result.",{"content_type":"future_widget","data":{"arbitrary":[1,2,3]}}]);
    let shown = interpret(&native, "conversation").unwrap();
    assert!(shown.text.contains("Here is the result."));
    assert!(shown.text.contains("[Native future\\_widget]("));
    assert!(!shown.text.contains("arbitrary"));
    assert!(shown.references.iter().any(
        |r| matches!(&r.target,ReferenceTarget::Url(url) if url.starts_with("asessions-capture:"))
    ));
}

#[test]
fn svg_keeps_text_entities_but_rejects_multiple_css_resource_references() {
    let native = message(
        r##"<svg xmlns="http://www.w3.org/2000/svg"><text>Desk &amp; speakers</text><rect fill="url(#local);url(\68 ttps://example.com/image)"/></svg>"##,
        json!({}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    let safe = shown
        .artifacts
        .iter()
        .find(|a| a.media_type.as_deref() == Some("image/svg+xml"))
        .unwrap();
    let text = String::from_utf8(safe.bytes.clone()).unwrap();
    assert!(text.contains("Desk &amp; speakers"));
    assert!(!text.contains("url("));
}

#[test]
fn incomplete_svg_is_preserved_as_native_content_not_emitted_as_live_markup() {
    let native = message(
        "Diagram: <svg onload=\"neverExecute()\"><path d=\"M0 0\"/>",
        json!({}),
    );
    let shown = interpret(&native, "conversation").unwrap();
    assert!(!shown.text.contains("<svg"));
    assert!(shown.text.contains("[Illustration unavailable]("));
    assert!(!shown.issues.is_empty());
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn validated_rebased_groups_beat_stale_exact_metadata() {
    let native = message(
        "A citeturn99view0.",
        json!({"stale":{"title":"Stale","url":"https://example.com/stale","ref_id":"turn99view0"},"content_references":[{"type":"grouped_webpages","items":[{"title":"Current","url":"https://example.com/current","refs":[{"turn_index":2,"ref_type":"view","ref_index":0}]}]}]}),
    );
    let shown = interpret(&native, "c").unwrap();
    assert!(
        shown
            .references
            .iter()
            .any(|r| r.target == ReferenceTarget::Url("https://example.com/current".into()))
    );
    assert!(
        !shown
            .references
            .iter()
            .any(|r| r.target == ReferenceTarget::Url("https://example.com/stale".into()))
    );
}
