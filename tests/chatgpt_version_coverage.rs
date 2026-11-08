use agent_sessions::chatgpt::{Evidence, Graph};
use serde_json::{Value, json};
fn message(id: &str) -> Value {
    json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]},"metadata":{"has_versions":true}})
}
fn export() -> Evidence {
    Evidence {format:"export".into(),origin:"export:fixture:c".into(),captured_at:0,sha256:"fixture".into(),payload:json!({"id":"c","update_time":serde_json::from_str::<Value>("1777097446.398972").unwrap(),"current_node":"u","mapping":{"root":{"parent":null,"message":null},"u":{"parent":"root","message":message("u")},"v":{"parent":"root","message":message("v")}}}).to_string()}
}
fn detail(revision: Option<&str>, time: i64) -> Evidence {
    let mut payload = json!({"messages":[message("u")],"page_info":{"has_previous_page":false}});
    if let Some(revision) = revision {
        payload["update_time"] = serde_json::from_str(revision).unwrap();
    }
    Evidence {
        format: "browser".into(),
        origin: "https://chatgpt.com/backend-api/conversations/c?num_turns=10".into(),
        captured_at: time,
        sha256: "fixture".into(),
        payload: payload.to_string(),
    }
}
#[test]
fn complete_export_fork_covers_same_revision_advertised_versions() {
    let graph =
        Graph::from_evidence("c", &[export(), detail(Some("1777097446.398972"), 1)]).unwrap();
    assert!(graph.export_covered_versions.contains("u"));
    assert!(!graph.fetched_versions.contains("u"));
}
#[test]
fn missing_or_different_latest_revision_never_assumes_export_version_coverage() {
    for revision in [
        None,
        Some("1777097446.3989720000000001"),
        Some("1777097446.398971"),
    ] {
        let graph = Graph::from_evidence(
            "c",
            &[
                export(),
                detail(Some("1777097446.398972"), 1),
                detail(revision, 2),
            ],
        );
        if revision.is_none() {
            assert!(graph.is_err());
        } else {
            let graph = graph.unwrap();
            assert_eq!(
                graph.export_covered_versions.is_empty(),
                revision != Some("1777097446.398971")
            );
        }
    }
}
#[test]
fn incomplete_export_or_missing_exported_fork_does_not_cover_versions() {
    for variant in ["incomplete", "no-fork", "target-absent"] {
        let mut record = export();
        let mut payload: Value = serde_json::from_str(&record.payload).unwrap();
        match variant {
            "incomplete" => payload["mapping"]["root"]["parent"] = json!("missing"),
            "no-fork" => {
                payload["mapping"].as_object_mut().unwrap().remove("v");
            }
            "target-absent" => {
                payload["mapping"].as_object_mut().unwrap().remove("u");
                payload["current_node"] = json!("v");
            }
            _ => unreachable!(),
        }
        record.payload = payload.to_string();
        let graph =
            Graph::from_evidence("c", &[record, detail(Some("1777097446.398972"), 1)]).unwrap();
        assert!(graph.export_covered_versions.is_empty(), "{variant}");
    }
}
