//! Partition native graph dialogue into canonical nonrepeating continuations.
use super::Graph;
use super::graph::dialog;
use crate::model::ConversationBranch;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
impl Graph {
    pub fn branches(&self) -> Result<Vec<ConversationBranch>> {
        self.validate()?;
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut roots = Vec::new();
        for (id, node) in &self.nodes {
            if let Some(parent) = &node.parent {
                children.entry(parent.clone()).or_default().push(id.clone());
            } else {
                roots.push(id.clone());
            }
        }
        if roots.len() == 1 && children.values().all(|kids| kids.len() < 2) {
            return Ok(Vec::new());
        }
        let mut starts = roots.iter().cloned().collect::<BTreeSet<_>>();
        for kids in children.values().filter(|kids| kids.len() > 1) {
            starts.extend(kids.iter().cloned());
        }
        let selected = self.selected_ids()?.into_iter().collect::<BTreeSet<_>>();
        let mut owners = BTreeMap::new();
        let mut sequences = Vec::new();
        for start in &starts {
            let mut ids = Vec::new();
            let mut cursor = start.clone();
            loop {
                owners.insert(cursor.clone(), start.clone());
                ids.push(cursor.clone());
                if let Some(kids) = children.get(&cursor)
                    && kids.len() == 1
                    && !starts.contains(&kids[0])
                {
                    cursor = kids[0].clone();
                } else {
                    break;
                }
            }
            sequences.push((start.clone(), ids));
        }
        let generated = super::presentation::generated_messages(self)?;
        let mut branches = Vec::new();
        if roots.len() != 1 {
            branches.push(ConversationBranch {
                id: "@root".into(),
                parent_id: None,
                fork_message_id: None,
                selected: true,
                messages: vec![],
                trailing_actions: 0,
            });
        }
        for (start, ids) in sequences {
            let parent_id = self.nodes[&start]
                .parent
                .as_ref()
                .map(|parent| owners[parent].clone())
                .or_else(|| (roots.len() != 1).then(|| "@root".into()));
            let mut fork_message_id = None;
            let mut messages = Vec::new();
            let mut actions = 0;
            for id in ids {
                if let Some(native) = &self.nodes[&id].message {
                    if let Some(mut message) = dialog(native, &self.id)? {
                        fork_message_id = Some(id.clone());
                        message.actions_before = actions;
                        messages.push(message);
                        actions = 0;
                    } else {
                        actions += 1;
                    }
                    if let Some(media) = generated.get(&id) {
                        fork_message_id = Some(id.clone());
                        messages.push(media.clone());
                    }
                }
            }
            branches.push(ConversationBranch {
                id: start.clone(),
                parent_id,
                fork_message_id,
                selected: selected.contains(&start),
                messages,
                trailing_actions: actions,
            });
        }
        if let Some(root) = branches
            .iter_mut()
            .find(|branch| branch.parent_id.is_none())
        {
            root.messages
                .extend(super::presentation::orphaned_media_messages(self)?);
        }
        while let Some(index) = branches.iter().position(|b| b.messages.is_empty()) {
            let empty = branches.remove(index);
            for child in &mut branches {
                if child.parent_id.as_deref() == Some(&empty.id) {
                    child.parent_id = empty.parent_id.clone();
                    if let Some(first) = child.messages.first_mut() {
                        first.actions_before += empty.trailing_actions;
                    } else {
                        child.trailing_actions += empty.trailing_actions;
                    }
                }
            }
        }
        loop {
            let single = branches.iter().find_map(|parent| {
                let children = branches
                    .iter()
                    .filter(|b| b.parent_id.as_deref() == Some(&parent.id))
                    .collect::<Vec<_>>();
                (children.len() == 1).then(|| (parent.id.clone(), children[0].id.clone()))
            });
            let Some((parent, child)) = single else { break };
            let index = branches.iter().position(|b| b.id == child).unwrap();
            let child = branches.remove(index);
            for descendant in &mut branches {
                if descendant.parent_id.as_deref() == Some(&child.id) {
                    descendant.parent_id = Some(parent.clone());
                }
            }
            let parent = branches.iter_mut().find(|b| b.id == parent).unwrap();
            let mut messages = child.messages;
            if let Some(first) = messages.first_mut() {
                first.actions_before += parent.trailing_actions;
            }
            parent.messages.extend(messages);
            parent.fork_message_id = child.fork_message_id;
            parent.trailing_actions = child.trailing_actions;
        }
        Ok(if branches.len() == 1 {
            vec![]
        } else {
            branches
        })
    }
}
