use agent_sessions::{
    archive::Archive,
    model::{Conversation, Message, MessageKind, Provenance, Role},
    render,
};
#[test]
fn rendered_dialog_round_trips_the_conversation_including_metadata_and_literal_delimiters() {
    let conversation=Conversation {provenance:Provenance {source:"codex".into(),host:"host".into(),native_id:"roundtrip".into(),native_path:"/store/roundtrip".into()},title:None,project_path:None,native_project:None,created_at_ms:Some(1),updated_at_ms:Some(3),messages:vec![
        Message {role:Role::User,kind:MessageKind::Dialog,text:"JSON reply please: {\"role\":\"assistant\"}\n<user timestamp-ms=\"3\">\n\\</assistant>\n</user>\n<branches>\n\n  tail  \n\n".into(),timestamp_ms:Some(1),actions_before:0},
        Message {role:Role::Agent,kind:MessageKind::Dialog,text:"<tool-calls count=\"7\" />\nLiteral agent message".into(),timestamp_ms:Some(2),actions_before:2},
        Message {role:Role::Assistant,kind:MessageKind::Dialog,text:"```rust\nlet unfinished = true;  ".into(),timestamp_ms:Some(3),actions_before:0},
        Message {role:Role::User,kind:MessageKind::Goal,text:"Goal objective  \n".into(),timestamp_ms:Some(4),actions_before:0},
        Message {role:Role::User,kind:MessageKind::GoalUnresolved,text:"Unavailable objective  \n".into(),timestamp_ms:Some(5),actions_before:0},
        Message {role:Role::User,kind:MessageKind::GoalContinuation,text:"Unavailable objective  \n".into(),timestamp_ms:Some(6),actions_before:0},
    ],branches:vec![],transcript_facts:Default::default(),subagent_parent:None,inherited_context:None,subagent_references:vec![],subagent_links:vec![],trailing_actions:1};
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(temp.path()).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    let session = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .remove(0);
    let rendered = render::render_transcript_with_options(
        &session,
        &conversation.messages,
        render::RenderOptions {
            show_action_counts: true,
        },
    );
    assert!(rendered.contains("<user>\n\n"));
    let parsed = render::parse_dialog_against(
        &rendered,
        &conversation.messages,
        conversation.trailing_actions,
    )
    .unwrap();
    let mut restored = conversation.clone();
    restored.messages = parsed.messages;
    restored.trailing_actions = parsed.trailing_actions;
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(conversation).unwrap()
    );
}

#[test]
fn ordinary_messages_have_plain_tags_and_verify_archive_text() {
    let rendered = "<user>\n\ntext\n\n</user>\n";
    let message = Message {
        role: Role::User,
        kind: MessageKind::Dialog,
        text: "text".into(),
        timestamp_ms: Some(123),
        actions_before: 2,
    };
    let parsed = render::parse_dialog_against(rendered, std::slice::from_ref(&message), 1).unwrap();
    assert_eq!(parsed.messages[0].timestamp_ms, Some(123));
    assert!(
        render::parse_dialog_against(&rendered.replace("text", "wrong"), &[message], 1).is_err()
    );
    for attributes in [
        "source-bytes=\"4\"",
        "actions-before=\"0\"",
        "timestamp-ms=\"1\"",
    ] {
        assert!(
            render::parse_dialog(&format!("<user {attributes}>\n\ntext\n\n</user>\n")).is_err()
        );
    }
}
