use agent_sessions::chatgpt::presentation::interpret;
use serde_json::{Value, json};
fn native(text: &str) -> Value {
    json!({"id":"answer","author":{"role":"assistant"},"content":{"parts":[text]},"metadata":{"model_dil_v2":{"appData":{}}}})
}

#[test]
fn dynamic_svg_is_one_native_artifact_reference_without_leaking_template_fragments() {
    let svg = r##"<svg viewBox="0 0 340 245" xmlns="http://www.w3.org/2000/svg">
    {#each [{x:14,label:"Laptop"}] as m}
      <rect x={m.x} y="12" width="108" height="67"/>
      <path d={`M ${m.x+5} 70 h98 l7 8 h-112 z`} fill="#7893AF"/>
      <text x={m.x+54} y="94">{m.label}</text>
    {/each}
    <path d="M 68 100 V120 H127 V141"/>
    <text>Dock</text>
  </svg>"##;
    let n = native(&format!(
        "Before.\n\n<box>\n  {svg}\n  <caption>Diagram caption.</caption>\n</box>\n\nAfter. Press `Cmd+1`."
    ));
    let shown = interpret(&n, "conversation").unwrap();
    assert_eq!(
        shown.text.matches("Illustration unavailable").count(),
        1,
        "{}",
        shown.text
    );
    for leaked in [
        "<svg",
        "m.x",
        "Dock",
        "[Path]",
        "[Rect]",
        "Expression unavailable",
        "{/each}",
    ] {
        assert!(!shown.text.contains(leaked), "{leaked}: {}", shown.text);
    }
    assert!(shown.text.contains("Before."));
    assert!(shown.text.contains("Diagram caption."));
    assert!(shown.text.contains("After. Press `Cmd+1`."));
    assert_eq!(shown.issues.len(), 1, "{:?}", shown.issues);
    assert!(
        !shown
            .artifacts
            .iter()
            .any(|a| a.media_type.as_deref() == Some("image/svg+xml"))
    );
    assert!(
        shown
            .artifacts
            .iter()
            .all(|asset| asset.media_type.as_deref() != Some("application/json"))
    );
}

#[test]
fn valid_svg_text_with_backticks_stays_inside_the_single_image() {
    let n = native(
        "Before. <svg xmlns=\"http://www.w3.org/2000/svg\"><text>Literal `ticks` inside illustration</text></svg> After.",
    );
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("Before. ![Illustration]("),
        "{}",
        shown.text
    );
    assert!(shown.text.contains(") After."), "{}", shown.text);
    assert!(!shown.text.contains("ticks"));
    let svg = shown
        .artifacts
        .iter()
        .find(|a| a.media_type.as_deref() == Some("image/svg+xml"))
        .unwrap();
    assert!(String::from_utf8_lossy(&svg.bytes).contains("Literal `ticks` inside illustration"));
}

#[test]
fn svg_examples_in_fenced_inline_and_indented_code_stay_exact() {
    let svg = "<svg><text>`literal`</text></svg>";
    let examples =
        format!("Fenced:\n```svg\n{svg}\n```\n\nInline ``{svg}``.\n\nIndented:\n\n    {svg}\n");
    let shown = interpret(&native(&examples), "conversation").unwrap();
    assert!(shown.text.starts_with(&examples), "{}", shown.text);
    assert!(
        !shown
            .artifacts
            .iter()
            .any(|a| a.media_type.as_deref() == Some("image/svg+xml"))
    );
    assert!(shown.issues.is_empty(), "{:?}", shown.issues);
}

#[test]
fn svg_strings_in_native_marker_json_are_entity_names_not_illustrations() {
    let n = native("Named entity[\"sensor\",\"Fixture <svg></svg>\",0].");
    let shown = interpret(&n, "conversation").unwrap();
    assert!(
        shown.text.contains("Fixture &lt;svg&gt;&lt;/svg&gt;"),
        "{}",
        shown.text
    );
    assert!(
        !shown
            .artifacts
            .iter()
            .any(|a| a.media_type.as_deref() == Some("image/svg+xml"))
    );
}
