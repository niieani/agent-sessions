//! Native Project identities and observations, reconstructed from preserved evidence.
use super::ingest::{captures, read_evidence};
use crate::{adapters::ExtractionContext, archive::Archive, model::NativeProject};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Default)]
struct Registry {
    verified: BTreeSet<String>,
    names: BTreeMap<String, (i64, String, Option<String>)>,
    memberships: BTreeMap<String, (i64, u8, String, Option<String>)>,
}
impl Registry {
    fn membership(
        &mut self,
        conversation: &str,
        project: Option<String>,
        at: i64,
        rank: u8,
        tie: &str,
    ) {
        let next = (at, rank, tie.to_owned(), project);
        if self.memberships.get(conversation).is_none_or(|old| {
            next.0 > old.0 || (next.0 == old.0 && (next.1, &next.2) > (old.1, &old.2))
        }) {
            self.memberships.insert(conversation.into(), next);
        }
    }
    fn explicit(
        &mut self,
        conversation: &str,
        row: &Value,
        at: i64,
        rank: u8,
        tie: &str,
    ) -> Result<()> {
        if let Some(value) = row.get("gizmo_id") {
            ensure!(
                value.is_null() || value.is_string(),
                "native gizmo_id must be a string or null"
            );
            if let Some(id) = value.as_str().filter(|id| native_project_id(id)) {
                self.verified.insert(id.into());
            }
            self.membership(
                conversation,
                value.as_str().map(str::to_owned),
                at,
                rank,
                tie,
            );
        }
        Ok(())
    }
    fn members(&mut self, project: &str, payload: &Value, at: i64, tie: &str) -> Result<()> {
        let items = payload["items"]
            .as_array()
            .context("Project listing items missing")?;
        for item in items {
            let id = item["id"]
                .as_str()
                .context("Project member identity missing")?;
            self.membership(id, Some(project.into()), at, 1, tie);
        }
        Ok(())
    }
    fn observe(&mut self, url: &str, payload: &Value, at: i64, tie: &str) -> Result<()> {
        let path = url.split('?').next().unwrap_or(url);
        if path == "https://chatgpt.com/backend-api/gizmos/snorlax/sidebar" {
            for item in payload["items"]
                .as_array()
                .context("Project directory items missing")?
            {
                let project = &item["gizmo"]["gizmo"];
                let Some(id) = project["id"].as_str().filter(|id| native_project_id(id)) else {
                    continue;
                };
                self.verified.insert(id.into());
                let name = project
                    .pointer("/display/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let next = (at, tie.to_string(), name);
                if self
                    .names
                    .get(id)
                    .is_none_or(|old| (next.0, &next.1) > (old.0, &old.1))
                {
                    self.names.insert(id.into(), next);
                }
                self.members(id, &item["conversations"], at, tie)?;
            }
        } else if let Some(id) = path
            .strip_prefix("https://chatgpt.com/backend-api/gizmos/")
            .and_then(|tail| tail.strip_suffix("/conversations"))
            .filter(|id| native_project_id(id))
        {
            self.verified.insert(id.into());
            self.members(id, payload, at, tie)?;
        } else if path == "https://chatgpt.com/backend-api/conversations" {
            for row in payload["items"]
                .as_array()
                .context("Conversation listing items missing")?
            {
                let id = row["id"]
                    .as_str()
                    .context("Conversation listing identity missing")?;
                self.explicit(id, row, at, 2, tie)?;
            }
        } else if let Some(id) = path
            .strip_prefix("https://chatgpt.com/backend-api/conversations/")
            .filter(|tail| !tail.contains('/') && !tail.is_empty())
        {
            self.explicit(id, payload, at, 3, tie)?;
        }
        Ok(())
    }
    fn context(self) -> Result<ExtractionContext> {
        for (_, _, _, project) in self.memberships.values() {
            if let Some(id) = project.as_ref().filter(|id| self.verified.contains(*id)) {
                ensure!(
                    self.names
                        .get(id)
                        .and_then(|(_, _, name)| name.as_deref())
                        .is_some_and(|name| !name.trim().is_empty()),
                    "ChatGPT Project facts missing for {id}; collect the native Project directory into Captures before extraction"
                );
            }
        }
        Ok(ExtractionContext {
            native_projects: self
                .memberships
                .into_iter()
                .map(|(conversation, (_, _, _, id))| {
                    let project =
                        id.filter(|id| self.verified.contains(id))
                            .map(|id| NativeProject {
                                source: "chatgpt".into(),
                                name: self.names.get(&id).and_then(|(_, _, name)| name.clone()),
                                native_id: id,
                            });
                    (conversation, project)
                })
                .collect(),
            ..Default::default()
        })
    }
}

pub(crate) fn native_project_id(id: &str) -> bool {
    id.strip_prefix("g-p-").is_some_and(|tail| {
        tail.len() == 32
            && tail
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn prepare(root: &Path) -> Result<ExtractionContext> {
    let mut registry = Registry::default();
    let archive = Archive::open_existing(root)?;
    let captures = captures(&archive)?;
    for capture in &captures {
        for evidence in read_evidence(capture)? {
            if evidence.format == "export"
                || (evidence.format == "snapshot-metadata"
                    && serde_json::from_str::<Value>(&evidence.payload)?["sourceFormat"]
                        == "export")
            {
                registry.explicit(
                    &capture.meta.native_id,
                    &serde_json::from_str(&evidence.payload)?,
                    (serde_json::from_str::<Value>(&evidence.payload)?["update_time"]
                        .as_f64()
                        .context("export update_time missing")?
                        * 1000.) as i64,
                    0,
                    &evidence.sha256,
                )?;
            } else if matches!(
                evidence.format.as_str(),
                "browser" | "catalog" | "snapshot-metadata"
            ) {
                registry.observe(
                    &evidence.origin,
                    &serde_json::from_str(&evidence.payload)?,
                    evidence.captured_at,
                    &evidence.sha256,
                )?;
            }
        }
    }
    let mut context = registry.context()?;
    for capture in captures {
        context
            .aggregate_captures
            .entry(capture.meta.native_id.clone())
            .or_default()
            .push(capture);
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn referenced_project_without_directory_facts_is_a_hard_error() {
        let temp = tempfile::tempdir().unwrap();
        let archive = Archive::open(temp.path()).unwrap();
        let payload=serde_json::json!({"update_time":1,"id":"conversation","gizmo_id":"g-p-0123456789abcdef0123456789abcdef","current_node":"u","mapping":{"u":{"parent":null,"message":{"id":"u","author":{"role":"user"},"content":{"content_type":"text","parts":["Preserved"]}}}}}).to_string();
        super::super::ingest::preserve(
            &archive,
            "conversation",
            super::super::Evidence {
                format: "export".into(),
                origin: "export:fixture:conversation".into(),
                captured_at: 0,
                sha256: super::super::ingest::hash(payload.as_bytes()),
                payload,
            },
            "fixture-account",
        )
        .unwrap();
        let error = super::super::ingest::extract_import(&archive)
            .err()
            .expect("missing Project facts must fail");
        assert!(format!("{error:#}").contains("Project facts missing"));
    }
    #[test]
    fn malformed_project_ids_are_never_verified_by_catalog_paths() {
        let mut registry = Registry::default();
        registry.observe("https://chatgpt.com/backend-api/gizmos/snorlax/sidebar",&serde_json::json!({"items":[{"gizmo":{"gizmo":{"id":"g-p-malformed","display":{"name":"Invalid"}}},"conversations":{"items":[{"id":"c"}]}}]}),1,"sidebar").unwrap();
        registry
            .observe(
                "https://chatgpt.com/backend-api/gizmos/g-p-also-malformed/conversations",
                &serde_json::json!({"items":[{"id":"d"}]}),
                2,
                "listing",
            )
            .unwrap();
        assert!(registry.context().unwrap().native_projects.is_empty());
    }
}
