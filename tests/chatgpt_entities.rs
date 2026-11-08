use agent_sessions::chatgpt::presentation::interpret;
use serde_json::{Value, json};

fn native(text: &str, results: Value, components: Value) -> Value {
    json!({"id":"answer","author":{"role":"assistant"},"content":{"parts":[text]},
        "metadata":{"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{},"componentResults":results}}},
            "genui_components":components}})
}
fn reference(index: u64) -> Value {
    json!({"turn_index":17,"ref_type":"sensor","ref_index":index})
}
fn result(index: u64, entity: Value) -> Value {
    json!({"componentName":"AsyncImage","status":"resolved","state":{"images":[{
        "refs":[reference(index)],"default_on_click_action":{"payload":{"extraParams":{"sensor":entity}}}
    }]}})
}

#[test]
fn exact_native_references_recover_names_without_a_model_binding_map() {
    let n = native(
        "Sensor: <Entity category=\"sensor\" ref=\"turn17sensor2\" value={turn17sensor2.title}/>.",
        json!({"image":result(2,json!({"title":"Outdoor thermometer","reading":18.25}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Outdoor thermometer"), "{}", shown.text);
    assert!(!shown.text.contains("Unresolved sensor"));
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}

#[test]
fn declared_binding_fields_take_precedence_and_preserve_field_meaning() {
    let n = native(
        "<Entity category=\"sensor\" ref=\"turn17sensor2\" value={turn17sensor2.title}/> {turn17sensor2.displayed_reading}; {turn17sensor2.displayed_price}",
        json!({"image":result(2,json!({"title":"Raw name","price":"$18","__internal":{"model_data_binding_fields_v1":{
            "title":"Displayed name","displayed_reading":"18 °C"}}}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Displayed name"), "{}", shown.text);
    assert!(shown.text.contains("18 °C"));
    assert!(!shown.text.contains("Raw name"));
    assert!(
        !shown.text.contains("$18"),
        "A price is not necessarily a displayed_price"
    );
    assert!(shown.text.contains("Price not captured"));
}

#[test]
fn component_resolution_identity_connects_scalar_display_state_to_entity() {
    let n = native(
        "<Entity category=\"place\" ref=\"turn9place3\" value={turn9place3.name}/>",
        json!({"label-component":{"status":"resolved","componentName":"Entity","state":{"value":"Golden Gate Park"}}}),
        json!([{"component_resolution_id":"label-component","entities":[{"entity_type":"place","ref":"turn9place3"}]}]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Golden Gate Park"), "{}", shown.text);
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}

#[test]
fn conflicting_exact_reference_values_remain_unresolved() {
    let n = native(
        "<Entity category=\"sensor\" ref=\"turn17sensor2\" value={turn17sensor2.title}/>",
        json!({"a":result(2,json!({"title":"North"})),"b":result(2,json!({"title":"South"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Unresolved sensor"), "{}", shown.text);
    assert!(!shown.text.contains("North"));
    assert!(!shown.text.contains("South"));
    assert!(!shown.issues.is_empty());
}

#[test]
fn duplicate_matching_reference_values_are_safe() {
    let n = native(
        "{turn17sensor2.title}",
        json!({"a":result(2,json!({"title":"North"})),"b":result(2,json!({"title":"North"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.starts_with("North"), "{}", shown.text);
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}

#[test]
fn unresolved_entities_and_expressions_describe_the_missing_value() {
    let n = native(
        "The <Entity category=\"product\" ref=\"turn17product14\" value={turn17product14.title}/> costs {turn17product14.displayed_price}.",
        json!({}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Unresolved product"), "{}", shown.text);
    assert!(shown.text.contains("Price not captured"));
    assert!(!shown.text.contains("{turn17"));
}

#[test]
fn a_component_with_inline_json_uses_its_native_title() {
    let n = native(
        "<Chart content={{\"chartType\":\"bar\",\"meta\":{\"title\":\"Monthly rainfall\"},\"data\":[1,2]}}/>",
        json!({}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Monthly rainfall"), "{}", shown.text);
    assert!(!shown.text.contains("<Chart"));
    assert!(!shown.text.contains("chartType"));
}

#[test]
fn unsupported_components_keep_descriptive_component_names() {
    let n = native(
        "<WeatherMap data={{notExecutable: true}}/>",
        json!({}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Weather map"), "{}", shown.text);
    assert!(!shown.text.contains("notExecutable"));
}

#[test]
fn unjoined_data_and_query_text_never_supply_an_entity_name() {
    let n = native(
        "<Entity category=\"sensor\" ref=\"turn17sensor8\" value={turn17sensor8.title} query=\"Guess this name\"/>",
        json!({"other":result(2,json!({"title":"Different entity"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.contains("Unresolved sensor"), "{}", shown.text);
    assert!(!shown.text.contains("Different entity"));
    assert!(!shown.text.contains("Guess this name"));
}

#[test]
fn expressions_remain_member_lookups_and_never_execute_native_code() {
    let n = native(
        "{turn17sensor2.title.toUpperCase()} {fetch('https://example.com')} <Entity category=\"sensor\" value=\"Exact literal\"/>",
        json!({"sensor":result(2,json!({"title":"Outdoor"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("Expression unavailable"),
        "{}",
        shown.text
    );
    assert!(shown.text.contains("Exact literal"));
    assert!(!shown.text.contains("OUTDOOR"));
    assert!(!shown.text.contains("fetch("));
}

#[test]
fn rich_layout_indentation_becomes_paragraphs_without_blank_line_accumulation() {
    let n = native(
        "Before.\n<box>\n  <row>\n    <AsyncImage ref=\"turn17sensor2\"/>\n    <box>\n      <text bold=\"true\">\n        Outdoor thermometer\n      </text>\n      <text>\n        Measures the garden.\n\n\n\n        Independent calibration.\n      </text>\n    </box>\n  </row>\n</box>\n\nAfter.",
        json!({"image":result(2,json!({"title":"Outdoor thermometer"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        !shown.text.lines().any(|line| line.starts_with("    ")),
        "{}",
        shown.text
    );
    assert!(!shown.text.contains("\n\n\n"), "{}", shown.text);
    assert!(
        shown
            .text
            .contains("Outdoor thermometer\n\nMeasures the garden.\n\nIndependent calibration."),
        "{}",
        shown.text
    );
    assert!(shown.text.starts_with("Before."));
    assert!(shown.text.contains("After."));
}

#[test]
fn layout_normalization_preserves_code_and_list_indentation_outside_components() {
    let outside = "- Existing list\n    - Existing nested item\n\n[Original source](https://example.com/original)\n\n";
    let code = "```rust\n    let untouched = \"<text>{native.expression}</text>\";\n```";
    let n = native(
        &format!(
            "{outside}<box>\n  <text>\n    Before `literal <Entity/>` code.\n\n{code}\n\n    After code.\n  </text>\n</box>\n"
        ),
        json!({}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(shown.text.starts_with(outside), "{}", shown.text);
    assert!(shown.text.contains(code), "{}", shown.text);
    assert!(shown.text.contains("`literal <Entity/>`"));
    assert!(!shown.text.contains("    Before"));
    assert!(!shown.text.contains("    After"), "{}", shown.text);
}

#[test]
fn rich_text_dedenting_preserves_relative_nested_list_indentation() {
    let n = native(
        "<box>\n  <text>\n    - First\n      - Nested\n    - Second\n  </text>\n</box>",
        json!({}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("- First\n  - Nested\n- Second"),
        "{}",
        shown.text
    );
}

#[test]
fn captured_display_labels_remain_literal_markdown_text() {
    let label = "[West] \\ *alpha* _beta_ `code` <tag>";
    let n = native(
        "<Entity category=\"sensor\" ref=\"turn17sensor2\" value={turn17sensor2.title}/>",
        json!({"image":result(2,json!({"title":label}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown
            .text
            .starts_with("[\\[West\\] \\\\ \\*alpha\\* \\_beta\\_ &#96;code&#96; &lt;tag&gt;]("),
        "{}",
        shown.text
    );
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn native_marker_examples_inside_captured_names_are_displayed_literally() {
    let n = native(
        "<Entity category=\"sensor\" ref=\"turn17sensor2\" value={turn17sensor2.title}/>",
        json!({"image":result(2,json!({"title":"Label financeturn1finance0"}))}),
        json!([]),
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown
            .text
            .contains("Label &#xE200;finance&#xE202;turn1finance0&#xE201;"),
        "{}",
        shown.text
    );
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}
