//! Offline Versions coverage requires explicit same-revision export fork proof.
use super::{Evidence, Graph};
use anyhow::Result;
use serde_json::Value;

pub(super) fn cover_from_exports(graph: &mut Graph, records: &[&Evidence]) -> Result<()> {
    if graph.advertised_versions.is_empty() {
        return Ok(());
    }
    let detail_prefix = format!("https://chatgpt.com/backend-api/conversations/{}", graph.id);
    let Some(latest_detail) = records.iter().rev().find(|record| {
        record.format == "browser"
            && record
                .origin
                .strip_prefix(&detail_prefix)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with('?'))
    }) else {
        return Ok(());
    };
    let detail: Value = serde_json::from_str(&latest_detail.payload)?;
    let Some(revision) = detail.get("update_time").and_then(Value::as_number) else {
        return Ok(());
    };
    if !detail.get("messages").is_some_and(Value::is_array) {
        return Ok(());
    }
    for record in records.iter().filter(|record| record.format == "export") {
        let payload: Value = serde_json::from_str(&record.payload)?;
        // Number equality retains arbitrary decimal precision; no f64 or
        // epsilon comparison can falsely certify a subtly newer web revision.
        if payload.get("update_time").and_then(Value::as_number) != Some(revision) {
            continue;
        }
        let mut export = Graph::default();
        export.id = graph.id.clone();
        export.add_export(&payload, record.captured_at)?;
        if export.validate().is_err() {
            continue;
        }
        for target in &graph.advertised_versions {
            let Some(node) = export.nodes.get(target) else {
                continue;
            };
            if node
                .message
                .as_ref()
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                != Some(target.as_str())
            {
                continue;
            }
            if let Some(parent) = &node.parent
                && export.nodes.iter().any(|(id, sibling)| {
                    id != target
                        && sibling.parent.as_ref() == Some(parent)
                        && sibling
                            .message
                            .as_ref()
                            .is_some_and(|m| super::graph::dialog_role(m).is_some())
                })
            {
                graph.export_covered_versions.insert(target.clone());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn hidden_export_siblings_do_not_certify_alternate_versions() {
        let u = json!({"id":"u","author":{"role":"user"},"content":{"parts":["Prompt"]}});
        let a = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["Answer"]},"metadata":{"has_versions":true}});
        let hidden = json!({"id":"hidden","author":{"role":"assistant"},"channel":"analysis","content":{"parts":["internal"]}});
        let records=vec![Evidence{format:"export".into(),origin:"export:fixture".into(),captured_at:1,sha256:"export".into(),payload:json!({"update_time":1,"current_node":"a","mapping":{"u":{"parent":null,"message":u},"a":{"parent":"u","message":a},"hidden":{"parent":"u","message":hidden}}}).to_string()},Evidence{format:"browser".into(),origin:"https://chatgpt.com/backend-api/conversations/c".into(),captured_at:2,sha256:"browser".into(),payload:json!({"update_time":1,"messages":[u,a]}).to_string()}];
        let graph = Graph::from_evidence("c", &records).unwrap();
        assert!(!graph.export_covered_versions.contains("a"));
    }
}
