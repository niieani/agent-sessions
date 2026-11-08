//! Generated source references become file-local Markdown footnotes.
use crate::model::{Message, ReferenceKind, ReferenceTarget, RenderReference, Role};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[((?:\\.|[^\[\]\\\n])*)\]\((asessions-reference:[a-f0-9]+)\)").unwrap()
});
static NOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^\]\n]+)\]").unwrap());
#[derive(Default)]
pub(super) struct Footnotes(Vec<(String, String, String)>);

impl Footnotes {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn prepare(
        references: Option<&BTreeMap<String, RenderReference>>,
        view: &Path,
        messages: &mut [Message],
    ) -> Self {
        let Some(references) = references else {
            return Self::default();
        };
        let mut occupied = messages
            .iter()
            .flat_map(|m| NOTE.captures_iter(&m.text).map(|c| c[1].to_owned()))
            .collect::<BTreeSet<_>>();
        let mut targets = BTreeMap::<ReferenceTarget, String>::new();
        let mut notes = Self::default();
        let mut number = 1;
        for message in messages {
            if message.role == Role::User {
                continue;
            }
            message.text = crate::markdown::map_prose(&message.text, |prose| {
                LINK.replace_all(prose, |c: &regex::Captures<'_>| {
                    let Some(reference) = references.get(&c[2]) else {
                        return c[0].to_owned();
                    };
                    let id = targets.entry(reference.target.clone()).or_insert_with(|| {
                        let id = loop {
                            let candidate = format!("asessions-{number}");
                            number += 1;
                            if occupied.insert(candidate.clone()) {
                                break candidate;
                            }
                        };
                        let href = match &reference.target {
                            ReferenceTarget::Url(url) => url.clone(),
                            ReferenceTarget::Asset(path) => super::relative_href(
                                view.parent().unwrap_or_else(|| Path::new("")),
                                path,
                            ),
                        };
                        notes.0.push((
                            id.clone(),
                            reference
                                .label
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" "),
                            href,
                        ));
                        id
                    });
                    let displayed = if reference.kind == ReferenceKind::Label {
                        &c[1]
                    } else {
                        ""
                    };
                    format!("{displayed}[^{id}]")
                })
                .into_owned()
            });
        }
        notes
    }
    pub fn context(&self) -> Value {
        json!(
            self.0
                .iter()
                .map(|(id, title, href)| json!({"id":id,"title":title,"href":href}))
                .collect::<Vec<_>>()
        )
    }
    pub fn append(&self, mut rendered: String) -> String {
        // These IDs were allocated only for emitted references outside code,
        // with all original footnote IDs reserved. A different Message may have
        // an unclosed source fence; it cannot erase this file's definitions.
        let used = NOTE
            .captures_iter(&rendered)
            .map(|c| c[1].to_owned())
            .collect::<BTreeSet<_>>();
        for (id, title, href) in &self.0 {
            if used.contains(id) {
                if !rendered.ends_with('\n') {
                    rendered.push('\n');
                }
                rendered.push_str(&format!(
                    "\n[^{id}]: [{}]({href})\n",
                    RenderReference::escape_label(title)
                ));
            }
        }
        rendered
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MessageKind;
    #[test]
    fn deduplicates_targets_across_kinds_preserves_code_and_reserves_existing_ids() {
        let a = RenderReference::new(
            ReferenceTarget::Url("https://example.com".into()),
            "A [source]".into(),
            ReferenceKind::Citation,
        );
        let b = RenderReference::new(a.target.clone(), "A source".into(), ReferenceKind::Label);
        let text = format!(
            "Existing[^asessions-1] {} {} `{}`\n```md\n{}\n```",
            a.markdown("citation"),
            b.markdown("sensor"),
            a.markdown("literal"),
            a.markdown("code")
        );
        let mut messages = vec![Message {
            role: Role::Assistant,
            kind: MessageKind::Dialog,
            text,
            timestamp_ms: None,
            actions_before: 0,
        }];
        let refs = [a, b].into_iter().map(|r| (r.alias.clone(), r)).collect();
        let notes = Footnotes::prepare(Some(&refs), Path::new("x.md"), &mut messages);
        assert!(
            messages[0]
                .text
                .contains("Existing[^asessions-1] [^asessions-2] sensor[^asessions-2]")
        );
        assert!(messages[0].text.contains("[literal](asessions-reference:"));
        assert_eq!(notes.0.len(), 1);
        assert!(
            notes
                .append(messages[0].text.clone())
                .contains("[^asessions-2]: [A \\[source\\]](https://example.com)")
        );
        assert!(
            notes
                .append("Template omitted messages".into())
                .ends_with("messages")
        );
    }
}
