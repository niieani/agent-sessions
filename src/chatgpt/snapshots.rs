//! Native revision ordering and coverage of response pages (never whole-tree assumptions).
use super::Evidence;
use super::revision::Revision;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;

fn snapshot(record: &Evidence) -> bool {
    matches!(record.format.as_str(), "export" | "snapshot-metadata")
        || (record.format == "browser"
            && !record.origin.contains("/messages?")
            && !record.origin.contains("/versions?"))
}
fn revision(record: &Evidence) -> Result<Revision> {
    let payload: Value = serde_json::from_str(&record.payload)?;
    Revision::new(
        payload
            .get("update_time")
            .context("ChatGPT snapshot update_time missing")?,
    )
}
pub(super) fn ordered(records: &[Evidence]) -> Result<Vec<&Evidence>> {
    let snapshots = records
        .iter()
        .filter(|r| snapshot(r))
        .map(|r| Ok((r, revision(r)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut ranked = Vec::new();
    for record in records {
        let time = if snapshot(record) {
            revision(record)?
        } else if record.format == "browser" {
            // Page/Versions payloads omit update_time. Their owning detail response
            // supplies it; prefer the latest observation preceding the page.
            snapshots
                .iter()
                .filter(|(r, _)| {
                    (r.format == "browser"
                        || (r.format == "snapshot-metadata" && !r.origin.starts_with("export:")))
                        && r.captured_at <= record.captured_at
                })
                .max_by_key(|(r, _)| r.captured_at)
                .or_else(|| snapshots.iter().min_by_key(|(r, _)| r.captured_at))
                .context("ChatGPT page has no snapshot update_time")?
                .1
                .clone()
        } else {
            Revision::new(&serde_json::json!(0))?
        };
        ranked.push((record, time));
    }
    ranked.sort_by(|(a, at), (b, bt)| {
        at.cmp(bt)
            .then(a.captured_at.cmp(&b.captured_at))
            .then(a.sha256.cmp(&b.sha256))
    });
    Ok(ranked.into_iter().map(|(r, _)| r).collect())
}
fn message_ids(record: &Evidence) -> Result<Option<BTreeSet<String>>> {
    if !matches!(record.format.as_str(), "export" | "browser")
        || record.origin.contains("/versions?")
    {
        return Ok(None);
    }
    let payload: Value = serde_json::from_str(&record.payload)?;
    let messages: Vec<&Value> = if record.format == "export" {
        payload["mapping"]
            .as_object()
            .context("export mapping missing")?
            .values()
            .filter_map(|n| n.get("message").filter(|m| !m.is_null()))
            .collect()
    } else {
        payload["messages"]
            .as_array()
            .context("browser messages missing")?
            .iter()
            .collect()
    };
    Ok(Some(
        messages
            .into_iter()
            .map(|m| {
                m["id"]
                    .as_str()
                    .context("message id missing")
                    .map(str::to_owned)
            })
            .collect::<Result<_>>()?,
    ))
}
pub(super) fn compact(records: Vec<Evidence>) -> Result<Vec<Evidence>> {
    let ordered = ordered(&records)?;
    let ids = ordered
        .iter()
        .map(|r| message_ids(r))
        .collect::<Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for (index, record) in ordered.iter().enumerate() {
        let covered = ids[index].as_ref().is_some_and(|old| {
            ids.iter()
                .skip(index + 1)
                .flatten()
                .any(|new| old.is_subset(new))
        });
        if !covered {
            result.push((*record).clone());
            continue;
        }
        let payload: Value = serde_json::from_str(&record.payload)?;
        if snapshot(record) {
            let mut metadata = payload;
            let object = metadata
                .as_object_mut()
                .context("snapshot must be object")?;
            object.remove("messages");
            object.remove("mapping");
            object.insert("sourceFormat".into(), Value::String(record.format.clone()));
            let payload = metadata.to_string();
            result.push(Evidence {
                format: "snapshot-metadata".into(),
                origin: record.origin.clone(),
                captured_at: record.captured_at,
                sha256: super::ingest::hash(payload.as_bytes()),
                payload,
            });
        }
    }
    result.sort_by(|a, b| {
        (&a.format, &a.origin, a.captured_at, &a.sha256).cmp(&(
            &b.format,
            &b.origin,
            b.captured_at,
            &b.sha256,
        ))
    });
    result.dedup_by(|a, b| {
        a.format == b.format
            && a.origin == b.origin
            && a.captured_at == b.captured_at
            && a.sha256 == b.sha256
    });
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn page(ids: &[&str], at: i64) -> Evidence {
        Evidence {format:"browser".into(),origin:"https://chatgpt.com/backend-api/conversations/c".into(),captured_at:at,sha256:at.to_string(),payload:json!({"update_time":at,"messages":ids.iter().map(|id| json!({"id":id})).collect::<Vec<_>>()}).to_string()}
    }
    #[test]
    fn newer_pages_remove_only_fully_covered_snapshots() {
        let records = compact(vec![
            page(&["a", "b"], 1),
            page(&["b", "c"], 2),
            page(&["b", "c", "d"], 3),
        ])
        .unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|r| r.format == "browser")
                .map(|r| r.captured_at)
                .collect::<Vec<_>>(),
            [1, 3]
        );
    }
    #[test]
    fn covered_details_keep_revision_for_pages_and_versions() {
        let detail = page(&["a", "b"], 1);
        let mut pagination = page(&["a", "b"], 2);
        pagination.origin.push_str("/messages?cursor=x");
        let mut payload: Value = serde_json::from_str(&pagination.payload).unwrap();
        payload.as_object_mut().unwrap().remove("update_time");
        pagination.payload = payload.to_string();
        let compacted = compact(vec![detail, pagination]).unwrap();
        assert!(ordered(&compacted).is_ok());
        assert_eq!(
            compacted.iter().filter(|r| r.format == "browser").count(),
            1
        );
    }
}
