//! Line-framed pseudo-XML Dialog. Markdown bodies are not parsed as XML.
use crate::model::{Message, MessageKind, Role};
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::collections::BTreeMap;
#[derive(Debug)]
pub struct ParsedDialog {
    pub messages: Vec<Message>,
    pub trailing_actions: u32,
}
fn opener(line: &str) -> Result<Option<(String, BTreeMap<String, String>)>> {
    if !line.starts_with('<') || line.starts_with("</") {
        return Ok(None);
    }
    if let Some(count) = line
        .strip_prefix("<tool-calls count=")
        .and_then(|s| s.strip_suffix(" />"))
    {
        let count: u32 = count.parse().context("invalid Action count")?;
        return Ok(Some((
            "tool-calls".into(),
            BTreeMap::from([("count".into(), count.to_string())]),
        )));
    }
    let mut reader = Reader::from_str(line);
    let event = match reader.read_event() {
        Ok(Event::Start(node) | Event::Empty(node)) => node,
        _ => return Ok(None),
    };
    let name = event.name().as_ref().to_owned();
    if !matches!(
        name.as_str(),
        "user" | "assistant" | "agent" | "parent" | "tool-calls"
    ) {
        return Ok(None);
    }
    ensure!(
        reader.buffer_position() == line.len() as u64,
        "content following Dialog opener"
    );
    let mut attributes = BTreeMap::new();
    for attr in event.attributes() {
        let attr = attr?;
        let key = attr.key.as_ref().to_owned();
        ensure!(key == "kind", "unknown Dialog attribute {key}");
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
            .into_owned();
        attributes.insert(key, value);
    }
    Ok(Some((name, attributes)))
}
pub fn parse_dialog(text: &str) -> Result<ParsedDialog> {
    let mut result = ParsedDialog {
        messages: Vec::new(),
        trailing_actions: 0,
    };
    let mut lines = text.split_inclusive('\n').peekable();
    let mut pending_actions = 0;
    let mut objective: Option<String> = None;
    while let Some(line) = lines.next() {
        let line = line.trim_end_matches(['\r', '\n']);
        let Some((tag, attrs)) = opener(line)? else {
            continue;
        };
        if tag == "tool-calls" {
            pending_actions = attrs.get("count").context("Action count absent")?.parse()?;
            continue;
        }
        ensure!(
            lines.next() == Some("\n"),
            "blank line required after <{tag}>"
        );
        let mut body = String::new();
        let close = format!("</{tag}>");
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim_end_matches(['\r', '\n']) == close {
                closed = true;
                break;
            }
            body.push_str(line);
        }
        ensure!(closed, "unclosed <{tag}>");
        ensure!(body.ends_with("\n\n"), "blank line required before {close}");
        body.truncate(body.len() - 2);
        let kind = attrs
            .get("kind")
            .map(|v| v.parse::<MessageKind>())
            .transpose()?
            .unwrap_or(MessageKind::Dialog);
        let mut body = match kind {
            MessageKind::Goal | MessageKind::GoalUnresolved => {
                let body = body
                    .strip_prefix("<goal>\n\n")
                    .and_then(|s| s.strip_suffix("\n\n</goal>"))
                    .context("malformed Goal block")?;
                body.strip_prefix("<!-- note: objective file no longer available -->\n")
                    .unwrap_or(body)
                    .to_string()
            }
            MessageKind::GoalContinuation => {
                ensure!(
                    body == "<goal-continuation />",
                    "malformed Goal Continuation"
                );
                objective
                    .clone()
                    .context("Goal Continuation requires preceding objective")?
            }
            MessageKind::Dialog => body,
        };
        if kind != MessageKind::GoalContinuation {
            body = body
                .split_inclusive('\n')
                .map(|line| {
                    if let Some(rest) = line.strip_prefix('\\')
                        && super::is_escaped_content_line(rest.trim_end_matches('\n'))
                    {
                        rest
                    } else {
                        line
                    }
                })
                .collect();
        }
        if matches!(kind, MessageKind::Goal | MessageKind::GoalUnresolved) {
            objective = Some(body.clone());
        }
        let actions = pending_actions;
        pending_actions = 0;
        let role = match tag.as_str() {
            "user" | "parent" => Role::User,
            "agent" => Role::Agent,
            "assistant" => Role::Assistant,
            _ => bail!("unknown role"),
        };
        result.messages.push(Message {
            role,
            kind,
            text: body,
            timestamp_ms: None,
            actions_before: actions,
        });
    }
    result.trailing_actions = pending_actions;
    Ok(result)
}

/// Verify the display projection against canonical archive Messages, then restore
/// their metadata and exact source text (including display-only fence repairs).
pub fn parse_dialog_against(
    text: &str,
    messages: &[Message],
    trailing_actions: u32,
) -> Result<ParsedDialog> {
    let mut parsed = parse_dialog(text)?;
    ensure!(
        parsed.messages.len() == messages.len(),
        "Dialog Message count differs from archive"
    );
    for (shown, source) in parsed.messages.iter().zip(messages) {
        let fragment = super::markdown::Fragment::new(source.text.as_str().into());
        let expected = fragment.text.trim_end();
        ensure!(
            shown.role == source.role && shown.kind == source.kind && shown.text == expected,
            "Dialog Message differs from archive Conversation"
        );
        ensure!(
            shown.actions_before == 0 || shown.actions_before == source.actions_before,
            "Dialog Action count differs from archive"
        );
    }
    ensure!(
        parsed.trailing_actions == 0 || parsed.trailing_actions == trailing_actions,
        "trailing Actions differ from archive"
    );
    parsed.messages = messages.to_vec();
    parsed.trailing_actions = trailing_actions;
    Ok(parsed)
}
