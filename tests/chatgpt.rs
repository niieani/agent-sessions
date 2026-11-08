#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::chatgpt::{Evidence, Graph};
use serde_json::json;

fn evidence(payload: serde_json::Value) -> Evidence {
    Evidence {
        format: "export".into(),
        origin: "fixture.json".into(),
        captured_at: 1,
        sha256: "fixture".into(),
        payload: payload.to_string(),
    }
}
#[test]
fn branching_shares_prefix_and_preserves_every_node() {
    let graph = Graph::from_evidence("c", &[evidence(json!({"update_time":1,"id":"c","title":"Branches","current_node":"b","mapping":{
        "r":{"id":"r","parent":null,"message":null},
        "u":{"id":"u","parent":"r","message":{"id":"u","author":{"role":"user"},"content":{"content_type":"text","parts":["shared"]}}},
        "a":{"id":"a","parent":"u","message":{"id":"a","author":{"role":"assistant"},"content":{"content_type":"text","parts":["first"]}}},
        "b":{"id":"b","parent":"u","message":{"id":"b","author":{"role":"assistant"},"content":{"content_type":"text","parts":["second"]}}}
    }}))]).unwrap();
    let branches = graph.branches().unwrap();
    assert_eq!(branches.len(), 3);
    let all = branches
        .iter()
        .flat_map(|branch| branch.messages.iter().map(|m| m.text.clone()))
        .collect::<String>();
    assert_eq!(all.matches("shared").count(), 1);
    assert_eq!(all.matches("first").count(), 1);
    assert_eq!(all.matches("second").count(), 1);
}
#[test]
fn incomplete_graph_fails_explicitly() {
    let graph=Graph::from_evidence("c", &[evidence(json!({"update_time":1,"id":"c","current_node":"u","mapping":{"u":{"id":"u","parent":"missing","message":null}}}))]).unwrap();
    assert!(graph.validate().is_err());
}
fn browser(url: &str, payload: serde_json::Value, time: i64) -> Evidence {
    Evidence {
        format: "browser".into(),
        origin: url.into(),
        captured_at: time,
        sha256: format!("{time}"),
        payload: payload.to_string(),
    }
}
fn msg(id: &str, role: &str, text: &str) -> serde_json::Value {
    json!({"id":id,"create_time":1770000000,"author":{"role":role},"content":{"content_type":"text","parts":[text]}})
}
#[test]
fn browser_pages_link_backward_and_versions_share_prefix() {
    let graph=Graph::from_evidence("c",&[
        browser("https://chatgpt.com/backend-api/conversations/c",json!({"update_time":1,"messages":[msg("u2","user","new"),msg("a2","assistant","answer")],"page_info":{"has_previous_page":true}}),1),
        browser("https://chatgpt.com/backend-api/conversations/c/messages?before=u2",json!({"update_time":1,"messages":[msg("u1","user","old"),msg("a1","assistant","old answer")],"page_info":{"has_previous_page":false}}),2),
        browser("https://chatgpt.com/backend-api/conversations/c/versions?message_id=u2",json!([[msg("v","user","alternate"),msg("va","assistant","version answer")]]),3)
    ]).unwrap();
    graph.validate().unwrap();
    assert_eq!(graph.selected_ids().unwrap(), ["u1", "a1", "u2", "a2"]);
    assert_eq!(graph.nodes["v"].parent.as_deref(), Some("a1"));
}
#[test]
fn missing_page_does_not_claim_complete() {
    let graph = Graph::from_evidence(
        "c",
        &[browser(
            "https://chatgpt.com/backend-api/conversations/c",
            json!({"update_time":1,"messages":[msg("u","user","hello")],"page_info":{"has_previous_page":true}}),
            1,
        )],
    )
    .unwrap();
    assert!(graph.validate().is_err());
}
#[test]
fn nested_branches_render_every_dialogue_once_and_all_links_resolve() {
    let messages = [
        ("u", None, "shared"),
        ("a", Some("u"), "one"),
        ("b", Some("u"), "two"),
        ("c", Some("a"), "three"),
        ("d", Some("a"), "four"),
    ];
    let mapping = messages
        .into_iter()
        .map(|(id, parent, text)| {
            (
                id.to_owned(),
                json!({"id":id,"parent":parent,"message":msg(id,"user",text)}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let graph = Graph::from_evidence(
        "conv",
        &[evidence(
            json!({"update_time":1,"id":"conv","current_node":"d","mapping":mapping}),
        )],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph, dir.path());
    agent_sessions::render::render_all(&archive).unwrap();
    let joined = shared::text(&archive);
    for path in shared::transcripts(&archive.views_dir()) {
        let text = std::fs::read_to_string(&path).unwrap();
        for link in text.split("](").skip(1) {
            let target = link.split(')').next().unwrap();
            assert!(!target.contains('#'));
            assert!(
                path.parent()
                    .unwrap()
                    .join(shared::decode_href(target))
                    .exists(),
                "{target}"
            );
        }
    }
    for word in ["shared", "one", "two", "three", "four"] {
        assert_eq!(joined.matches(&format!("\n{word}\n")).count(), 1);
    }
}
#[test]
fn browser_ingest_is_idempotent_and_rebuildable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let payload=json!({"conversation_id":"conv","update_time":10.125,"messages":[msg("u","user","hello"),msg("a","assistant","goodbye")],"page_info":{"has_previous_page":false}}).to_string();
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
    std::fs::write(input.join("capture.json"), &payload).unwrap();
    std::fs::write(input.join("capture.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/conv","sha256":digest,"capturedAt":"2026-10-15T00:00:00Z"}).to_string()).unwrap();
    let archive = agent_sessions::archive::Archive::open(&dir.path().join("archive")).unwrap();
    assert_eq!(shared::collect(&archive, &input).unwrap().sessions_new, 1);
    assert_eq!(shared::collect(&archive, &input).unwrap().sessions_found, 0);
    let rebuilt = agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(rebuilt.conversations_extracted, 1);
    assert_eq!(rebuilt.captures_failed, 0);
    let capture = archive
        .lookup_capture("conv", Some("chatgpt"), None)
        .unwrap();
    let status = agent_sessions::chatgpt::status(&archive).unwrap();
    assert_eq!(status["conversations"]["conv"]["updatedAt"], 10.125);
    assert!(capture.capture.body_path.exists());
}
#[test]
fn generated_tool_images_remain_visible_while_ordinary_tool_actions_do_not() {
    let graph=Graph::from_evidence("c",&[browser("https://chatgpt.com/backend-api/conversations/c",json!({"update_time":1,"messages":[msg("u","user","draw"),{"id":"image","author":{"role":"tool","name":"image_gen"},"content":{"content_type":"multimodal_text","parts":[{"content_type":"image_asset_pointer","asset_pointer":"sediment://file_image","metadata":{"generation":{}}}]}},msg("a","assistant","done")],"page_info":{"has_previous_page":false}}),1)]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph, dir.path());
    let session = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .remove(0);
    let messages = archive.db().unwrap().messages(session.id).unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m.text.contains("asessions-media:chatgpt/file_image"))
    );
}
#[test]
fn captured_native_json_is_byte_exact_and_corruption_cannot_advance_index() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let payload = "{ \"update_time\":1, \"messages\": [ { \"id\": \"u\", \"author\":{\"role\":\"user\"}, \"content\":{\"parts\":[\"hello\\nworld\"]} } ], \"page_info\":{\"has_previous_page\":false} }";
    let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
    std::fs::write(input.join("one.json"), payload).unwrap();
    std::fs::write(
        input.join("one.meta.json"),
        json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/conv","sha256":digest,"capturedAt":"2026-10-15T00:00:00Z"})
            .to_string(),
    )
    .unwrap();
    let archive = agent_sessions::archive::Archive::open(&dir.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    let capture = archive
        .lookup_capture("conv", Some("chatgpt"), None)
        .unwrap()
        .capture;
    let mut decoder =
        zstd::stream::read::Decoder::new(std::fs::File::open(&capture.body_path).unwrap()).unwrap();
    let mut text = String::new();
    decoder.read_to_string(&mut text).unwrap();
    let evidence: Evidence = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(evidence.payload, payload);
    std::fs::write(input.join("one.json"), "{}").unwrap();
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed > 0);
    assert_eq!(archive.db().unwrap().session_count().unwrap(), 1);
}
#[test]
fn generated_media_bytes_are_preserved_and_linked() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir_all(input.join("assets")).unwrap();
    let image = b"fixture generated bytes";
    let hash = format!("{:x}", Sha256::digest(image));
    let path = format!("assets/{hash}.png");
    std::fs::write(input.join(&path), image).unwrap();
    std::fs::write(input.join("assets/image.meta.json"),json!({"mime":"image/png","conversationId":"conv","capturedAt":"2026-10-19T00:00:00Z","format":"chatgpt-generated-image-v1","assetId":"file_image","sha256":hash,"bytes":image.len(),"path":path}).to_string()).unwrap();
    let payload=json!({"update_time":1,"messages":[msg("u","user","draw"),{"id":"i","create_time":1770000000,"author":{"role":"tool"},"content":{"parts":[{"asset_pointer":"sediment://file_image","metadata":{"generation":{}}}]} }],"page_info":{"has_previous_page":false}}).to_string();
    std::fs::write(input.join("c.json"), &payload).unwrap();
    std::fs::write(input.join("c.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/conv","sha256":format!("{:x}",Sha256::digest(payload.as_bytes())),"capturedAt":"2026-10-15T00:00:00Z"}).to_string()).unwrap();
    let archive = agent_sessions::archive::Archive::open(&dir.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    agent_sessions::render::render_all(&archive).unwrap();
    assert_eq!(
        std::fs::read(archive.root().join(format!("media/objects/{hash}"))).unwrap(),
        image
    );
    assert!(!archive.views_dir().join("media").exists());
    assert!(shared::text(&archive).contains(&format!(
        "![Generated image](../../media/sha256/{hash}.png)"
    )));
}
#[test]
fn history_join_removes_obsolete_segments() {
    let mut graph=Graph::from_evidence("c",&[evidence(json!({"update_time":1,"id":"c","current_node":"b","mapping":{"a":{"parent":null,"message":msg("a","user","earlier")},"b":{"parent":null,"message":msg("b","assistant","later")}}}))]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph, dir.path());
    agent_sessions::render::render_all(&archive).unwrap();
    let previous = shared::transcripts(&archive.views_dir());
    assert_eq!(previous.len(), 2);
    graph.nodes.get_mut("b").unwrap().parent = Some("a".into());
    let conversation = graph
        .conversation(agent_sessions::model::Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    agent_sessions::render::render_all(&archive).unwrap();
    assert_eq!(shared::transcripts(&archive.views_dir()).len(), 1);
    assert!(previous.iter().any(|p| !p.exists()));
    let all = shared::text(&archive);
    assert!(all.contains("earlier") && all.contains("later"));
}

#[test]
fn identical_generated_bytes_retain_distinct_native_asset_identities() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir_all(input.join("assets")).unwrap();
    let bytes = b"same generated image";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = format!("assets/{hash}.png");
    std::fs::write(input.join(&path), bytes).unwrap();
    for id in ["file_first", "file_second"] {
        std::fs::write(input.join(format!("assets/{id}.meta.json")),json!({"mime":"image/png","conversationId":"conv","capturedAt":"2026-10-19T00:00:00Z","format":"chatgpt-generated-image-v1","assetId":id,"sha256":hash,"bytes":bytes.len(),"path":path}).to_string()).unwrap();
    }
    let payload=json!({"update_time":1,"messages":[{"id":"i","create_time":1770000000,"author":{"role":"tool"},"content":{"parts":[{"asset_pointer":"sediment://file_first","metadata":{"generation":{}}},{"asset_pointer":"sediment://file_second","metadata":{"generation":{}}}]}}],"page_info":{"has_previous_page":false}}).to_string();
    std::fs::write(input.join("c.json"), &payload).unwrap();
    std::fs::write(input.join("c.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/conv","sha256":format!("{:x}",Sha256::digest(payload.as_bytes())),"capturedAt":"2026-10-15T00:00:00Z"}).to_string()).unwrap();
    let archive = agent_sessions::archive::Archive::open(&dir.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    agent_sessions::render::render_all(&archive).unwrap();
    let rendered = shared::text(&archive);
    assert_eq!(
        rendered
            .matches("![Generated image](../../media/sha256/")
            .count(),
        2
    );
    assert!(!rendered.contains("media not captured"));
}
