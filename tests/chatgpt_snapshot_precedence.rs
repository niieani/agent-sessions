#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::chatgpt::{Evidence, Graph};
use serde_json::{Value, json};
fn record(format: &str, captured_at: i64, payload: Value) -> Evidence {
    Evidence {
        format: format.into(),
        captured_at,
        payload: payload.to_string(),
        origin: if format == "export" {
            "export:fixture".into()
        } else {
            "https://chatgpt.com/backend-api/conversations/c".into()
        },
        sha256: "fixture".into(),
    }
}
fn message(text: &str) -> Value {
    json!({"id":"a","author":{"role":"assistant"},"content":{"parts":[text]}})
}
#[test]
fn native_revision_precedes_capture_time_across_snapshot_formats() {
    for records in [
        vec![
            record(
                "browser",
                200,
                json!({"update_time":10,"title":"old","messages":[message("old")]}),
            ),
            record(
                "export",
                0,
                json!({"update_time":20,"title":"new","current_node":"a","mapping":{"a":{"parent":null,"message":message("new")}}}),
            ),
        ],
        vec![
            record(
                "export",
                300,
                json!({"update_time":10,"title":"old","current_node":"a","mapping":{"a":{"parent":null,"message":message("old")}}}),
            ),
            record(
                "browser",
                100,
                json!({"update_time":20,"title":"new","messages":[message("new")]}),
            ),
        ],
    ] {
        let graph = Graph::from_evidence("c", &records).unwrap();
        assert_eq!(graph.title.as_deref(), Some("new"));
        assert_eq!(
            graph.nodes["a"].message.as_ref().unwrap()["content"]["parts"][0],
            "new"
        );
    }
}
#[test]
fn snapshot_without_native_revision_is_an_error() {
    for format in ["export", "browser"] {
        assert!(
            Graph::from_evidence(
                "c",
                &[record(format, 1, json!({"messages":[],"mapping":{}}))]
            )
            .is_err()
        );
    }
}

#[test]
fn covered_snapshots_are_removed_and_raw_responses_live_only_in_captures() {
    use agent_sessions::archive::Archive;
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let input = temp.path().join("input");
    std::fs::create_dir(&input).unwrap();
    for revision in [1, 2] {
        let payload = json!({"update_time":revision,"messages":[message("reply")]}).to_string();
        std::fs::write(input.join("c.json"), &payload).unwrap();
        std::fs::write(input.join("c.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":format!("2026-10-19T00:00:0{revision}Z"),"sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
        assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    }
    let capture = archive
        .lookup_capture("c", Some("chatgpt"), None)
        .unwrap()
        .capture;
    use std::io::BufRead;
    let reader = std::io::BufReader::new(
        zstd::stream::read::Decoder::new(std::fs::File::open(capture.body_path).unwrap()).unwrap(),
    );
    let records = reader
        .lines()
        .map(|line| serde_json::from_str::<Evidence>(&line.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.iter().filter(|r| r.format == "browser").count(), 1);
    assert_eq!(
        records
            .iter()
            .filter(|r| r.format == "snapshot-metadata")
            .count(),
        1
    );
    assert!(!archive.root().join("chatgpt-evidence").exists());
}

#[test]
fn pagination_does_not_replace_known_export_topology() {
    let m = |id: &str| json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]}});
    let mut page = record(
        "browser",
        3,
        json!({"messages":[m("a")],"page_info":{"has_previous_page":false}}),
    );
    page.origin.push_str("/messages?before=c");
    let graph=Graph::from_evidence("c", &[
        record("export",1,json!({"update_time":10,"current_node":"c","mapping":{"a":{"parent":null,"message":m("a")},"b":{"parent":"a","message":m("b")},"c":{"parent":"b","message":m("c")}}})),
        record("browser",2,json!({"update_time":10,"messages":[m("c")]})), page
    ]).unwrap();
    assert_eq!(graph.selected_ids().unwrap(), ["a", "b", "c"]);
}
#[test]
fn sub_float_precision_native_revisions_keep_the_newest_text() {
    let payload = |revision: &str, text: &str| {
        serde_json::from_str::<Value>(&format!(r#"{{"update_time":{revision},"messages":[{{"id":"a","author":{{"role":"assistant"}},"content":{{"parts":["{text}"]}}}}]}}"#)).unwrap()
    };
    let graph = Graph::from_evidence(
        "c",
        &[
            record("browser", 200, payload("1790000000.1234567801", "old")),
            record("browser", 100, payload("1790000000.1234567802", "new")),
        ],
    )
    .unwrap();
    assert_eq!(
        graph.nodes["a"].message.as_ref().unwrap()["content"]["parts"][0],
        "new"
    );
}

#[test]
fn newer_export_can_replace_an_old_parent_with_an_explicit_root() {
    let m = |id: &str| json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]}});
    let old = record(
        "export",
        1,
        json!({"update_time":1,"current_node":"b","mapping":{"a":{"parent":null,"message":m("a")},"b":{"parent":"a","message":m("b")}}}),
    );
    let new = record(
        "export",
        2,
        json!({"update_time":2,"current_node":"a","mapping":{"b":{"parent":null,"message":m("b")},"a":{"parent":"b","message":m("a")}}}),
    );
    let graph = Graph::from_evidence("c", &[old, new]).unwrap();
    assert_eq!(graph.selected_ids().unwrap(), ["b", "a"]);
}
