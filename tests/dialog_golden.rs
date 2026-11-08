use agent_sessions::{archive::Archive, model::Conversation, render};
use serde_json::{Value, json};
#[test]
fn shared_dialog_matches_0_2_3_for_claude_codex_t3_and_pi_except_wrapper_blank_lines()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let destination = temp.path();
    let mut outputs = serde_json::Map::new();
    for source in ["claude", "codex", "t3", "pi"] {
        let conversation: Conversation = serde_json::from_value(
            json!({"provenance":{"source":source,"host":"testhost","native_id":"golden","native_path":"/store/golden"},"title":"Golden","project_path":"/work/example","native_project":null,"created_at_ms":1770000000000i64,"updated_at_ms":1770014400000i64,"messages":[
   {"role":"user","kind":"dialog","text":"Keep this code:\n\n```rust\nlet value = 1;\n```\n\nJSON: {\"role\":\"assistant\"}","timestamp_ms":1770000000000i64,"actions_before":0},
   {"role":"agent","kind":"dialog","text":"Delegated instruction","timestamp_ms":1770000060000i64,"actions_before":2},
   {"role":"assistant","kind":"dialog","text":"First line.\n\n**Answer** with `code` and [a link](https://example.com).  \n\n","timestamp_ms":1770014400000i64,"actions_before":3}
  ],"transcript_facts":{},"subagent_parent":null,"inherited_context":null,"subagent_references":[],"branches":[],"subagent_links":[],"trailing_actions":1}),
        )?;
        let root = destination.join(source);
        let archive = Archive::open(&root)?;
        archive.db()?.upsert_conversation(&conversation)?;
        let session = archive.db()?.sessions_with_dialog()?.remove(0);
        let text = render::render_transcript_with_options(
            &session,
            &conversation.messages,
            render::RenderOptions {
                show_action_counts: true,
            },
        );
        let (_, body) = text
            .strip_prefix("---\n")
            .unwrap()
            .split_once("\n---\n")
            .unwrap();
        outputs.insert(source.into(), Value::String(body.into()));
    }
    let expected: serde_json::Map<String, Value> =
        serde_json::from_str(include_str!("goldens/dialog_0_2_3.json"))?;
    let open = regex::Regex::new(r"(?m)^(<(?:user|assistant|agent|parent)>)\n")?;
    let close = regex::Regex::new(r"(?m)\n(</(?:user|assistant|agent|parent)>)$")?;
    for (source, old) in expected {
        let normalized = open.replace_all(old.as_str().unwrap(), "$1\n\n");
        let normalized = close.replace_all(&normalized, "\n\n$1");
        assert_eq!(
            outputs[&source].as_str().unwrap(),
            normalized,
            "{source} changed beyond wrapper blank lines"
        );
    }
    Ok(())
}
