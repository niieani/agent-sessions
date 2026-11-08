use agent_sessions::chatgpt::presentation::interpret;
use serde_json::json;
#[test]
fn layout_title_icons_and_product_fields_render_content_without_widget_labels() {
    let product = json!({"title":"Clamp","price":"$569.99","merchants":"Store","url":"https://example.com/clamp"});
    let state = json!({"refs":["turn1product0"],"default_on_click_action":{"payload":{"extraParams":{"product":product}}}});
    let metadata =
        json!({"model_dil_v2":{"appData":{"opGenui":{"componentResults":{"r":{"state":state}}}}}});
    let native = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["<grid><grid-item><title>{turn1product0.displayed_price}</title><text>{turn1product0.merchant}</text><Link url={turn1product0.product_page.uri} title=\"Buy\"/></grid-item></grid>\n| Ready | <icon name=\"check\"/> |"]},"metadata":metadata});
    let shown = interpret(&native, "c").unwrap();
    assert!(shown.text.contains("$569.99"), "{}", shown.text);
    assert!(shown.text.contains("Store"));
    assert!(shown.text.contains("✓"));
    for label in ["Grid", "Title", "Icon", "Displayed price", "unavailable"] {
        assert!(!shown.text.contains(label), "{}", shown.text);
    }
    assert!(
        shown
            .references
            .iter()
            .any(|r| format!("{:?}", r.target).contains("https://example.com/clamp"))
    );
}
#[test]
fn old_citation_and_image_markers_are_interpreted_but_code_stays_literal() {
    let native = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["Claim【web†source】 【6:0†document.md】 【12345†screenshot】 【7†source】 and detail【8†L1-L3】. 【9†embed_image】 【{\"image_fetch\":\"Clamp\"}】\n`【7†source】`"]},"metadata":{}});
    let shown = interpret(&native, "c").unwrap();
    assert_eq!(shown.text.matches('【').count(), 1, "{}", shown.text);
    assert!(shown.text.contains("`【7†source】`"));
    assert!(shown.text.contains("Unresolved citation"));
    assert!(shown.text.contains("Image"));
}

#[test]
fn missing_product_price_is_an_explicit_content_gap_without_schema_labels() {
    let native = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["{product.displayed_price}"]},"metadata":{"model_dil_v2":{}}});
    let shown = interpret(&native, "c").unwrap();
    assert!(!shown.text.contains("Displayed price"));
    assert!(shown.text.contains("Price not captured"));
}
