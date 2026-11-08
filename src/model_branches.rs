//! Source-neutral continuation segments; each owns its dialogue exactly once.
use crate::model::Message;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationBranch {
    pub id: String,
    pub parent_id: Option<String>,
    pub fork_message_id: Option<String>,
    pub selected: bool,
    pub messages: Vec<Message>,
    pub trailing_actions: u32,
}
#[derive(Debug, Clone)]
pub struct RenderAsset {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
    pub references: Vec<String>,
    pub media_type: Option<String>,
}

/// Pure graph projection shared by Render and Digest planning.
#[derive(Debug, Clone)]
pub struct BranchPath {
    pub segment: ConversationBranch,
    pub ancestors: Vec<Message>,
    pub path_id: Option<String>,
    pub number: usize,
    pub shared: bool,
}
pub fn branch_paths(branches: &[ConversationBranch]) -> anyhow::Result<Vec<BranchPath>> {
    use anyhow::{Context, ensure};
    use std::collections::{BTreeMap, BTreeSet};
    let by_id = branches
        .iter()
        .map(|b| (b.id.as_str(), b))
        .collect::<BTreeMap<_, _>>();
    let mut children: BTreeMap<&str, Vec<&ConversationBranch>> = BTreeMap::new();
    for branch in branches {
        if let Some(parent) = branch.parent_id.as_deref() {
            ensure!(by_id.contains_key(parent), "canonical branch parent absent");
            children.entry(parent).or_default().push(branch);
        }
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|b| (b.messages.iter().find_map(|m| m.timestamp_ms), &b.id));
    }
    let mut paths = Vec::new();
    for branch in branches {
        let mut chain = vec![branch];
        let mut cursor = branch;
        let mut seen = BTreeSet::new();
        while let Some(parent) = cursor.parent_id.as_deref() {
            ensure!(seen.insert(parent), "canonical branch cycle");
            cursor = by_id
                .get(parent)
                .context("canonical branch parent absent")?;
            chain.push(cursor);
        }
        chain.reverse();
        let mut path_id = None;
        for pair in chain.windows(2) {
            if children[pair[0].id.as_str()][0].id != pair[1].id {
                path_id = Some(pair[1].id.clone());
            }
        }
        paths.push(BranchPath {
            segment: branch.clone(),
            ancestors: chain
                .iter()
                .take(chain.len() - 1)
                .flat_map(|b| b.messages.clone())
                .collect(),
            path_id,
            number: 1,
            shared: children.contains_key(branch.id.as_str()),
        });
    }
    let mut origins = paths
        .iter()
        .filter_map(|p| p.path_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    origins.sort_by_key(|id| {
        (
            by_id[id.as_str()]
                .messages
                .iter()
                .find_map(|m| m.timestamp_ms),
            id.clone(),
        )
    });
    for path in &mut paths {
        if let Some(id) = &path.path_id {
            path.number = origins.iter().position(|v| v == id).unwrap() + 2;
        }
    }
    Ok(paths)
}
