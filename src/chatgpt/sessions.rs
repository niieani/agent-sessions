//! Opening edits have no shared Dialog: project independent Session identities.
use super::Graph;
use crate::model::{Conversation, Provenance, Role};
use anyhow::Result;
use std::collections::BTreeSet;
impl Graph {
    pub fn sessions(&self, provenance: Provenance) -> Result<Vec<Conversation>> {
        self.validate()?;
        let mut openings = Vec::new();
        for (id, node) in &self.nodes {
            let Some(native) = &node.message else {
                continue;
            };
            if super::graph::dialog(native, &self.id)?.is_none_or(|m| m.role != Role::User) {
                continue;
            }
            let mut parent = node.parent.as_deref();
            let mut earlier = false;
            while let Some(id) = parent {
                let node = &self.nodes[id];
                if let Some(native) = &node.message
                    && super::graph::dialog(native, &self.id)?.is_some()
                {
                    earlier = true;
                    break;
                }
                parent = node.parent.as_deref();
            }
            if !earlier {
                openings.push(id.clone());
            }
        }
        openings.sort_by(|a, b| {
            let time = |id: &String| {
                self.nodes[id]
                    .message
                    .as_ref()
                    .and_then(|m| m["create_time"].as_f64())
            };
            time(a)
                .partial_cmp(&time(b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        if openings.len() < 2 {
            return Ok(vec![self.conversation(provenance)?]);
        }
        let mut sessions = Vec::new();
        for (index, opening) in openings.iter().enumerate() {
            // Test ancestry of every node, retaining complete internal Action paths.
            let ids = self
                .nodes
                .keys()
                .filter(|id| {
                    let mut cursor = Some(id.as_str());
                    while let Some(id) = cursor {
                        if id == opening {
                            return true;
                        }
                        cursor = self.nodes[id].parent.as_deref();
                    }
                    false
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut graph = self.clone();
            graph.nodes.retain(|id, _| ids.contains(id));
            graph.nodes.get_mut(opening).unwrap().parent = None;
            graph.issues.retain(|issue| {
                ids.contains(issue.strip_prefix("history-before:").unwrap_or(issue))
            });
            if graph.current.as_ref().is_none_or(|id| !ids.contains(id)) {
                graph.current = graph
                    .nodes
                    .iter()
                    .max_by(|(a, an), (b, bn)| {
                        let time = |n: &super::Node| {
                            n.message.as_ref().and_then(|m| m["create_time"].as_f64())
                        };
                        time(an)
                            .partial_cmp(&time(bn))
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(a.cmp(b))
                    })
                    .map(|(id, _)| id.clone());
            }
            graph.created = graph.nodes[opening]
                .message
                .as_ref()
                .and_then(|m| m["create_time"].as_f64());
            graph.exported_media.retain(|_, m| {
                m["messageId"].as_str().map_or(index == 0, |id| {
                    ids.contains(id) || (index == 0 && !self.nodes.contains_key(id))
                })
            });
            let mut identity = provenance.clone();
            if index > 0 {
                identity.native_id = format!("{}~{opening}", provenance.native_id);
            }
            sessions.push(graph.conversation(identity)?);
        }
        Ok(sessions)
    }
}
