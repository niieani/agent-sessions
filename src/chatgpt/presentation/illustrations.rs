//! Project complete illustrations before Markdown can split native templates.
use super::{Presentation, components, svg};
use crate::model::{ReferenceKind, RenderAsset};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::ops::Range;

fn protected(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut block = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => block = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = block.take() {
                    ranges.push(start..range.end)
                }
            }
            Event::Code(_) => ranges.push(range),
            _ => {}
        }
    }
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find('').map(|offset| cursor + offset) {
        let Some(end) = text[start..]
            .find('')
            .map(|offset| start + offset + ''.len_utf8())
        else {
            break;
        };
        ranges.push(start..end);
        cursor = end;
    }
    ranges
}
fn svg_tag(text: &str, close: bool) -> bool {
    let prefix = if close { "</svg" } else { "<svg" };
    text.strip_prefix(prefix).is_some_and(|tail| {
        tail.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))
    })
}
fn end(text: &str) -> Option<usize> {
    let mut depth = 0;
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find('<').map(|offset| cursor + offset) {
        let tail = &text[start..];
        let end = components::tag_end(tail)?;
        let token = &tail[..end];
        if svg_tag(token, false) {
            if !token.trim_end_matches('>').trim_end().ends_with('/') {
                depth += 1;
            } else if depth == 0 {
                return Some(start + end);
            }
        } else if svg_tag(token, true) {
            depth -= 1;
            if depth == 0 {
                return Some(start + end);
            }
        }
        cursor = start + end;
    }
    None
}
fn unavailable(p: &mut Presentation, artifact: &str, reason: &str) -> String {
    p.issues.push(format!("unsupported-svg:{reason}"));
    p.source_link("Illustration unavailable", artifact, ReferenceKind::Label)
}

pub(super) fn project(text: &str, artifact: &str, base: &str, p: &mut Presentation) -> String {
    let protected = protected(text);
    let mut out = String::new();
    let mut cursor = 0;
    let mut retained = 0;
    while let Some(start) = text[cursor..].find('<').map(|offset| cursor + offset) {
        if let Some(range) = protected.iter().find(|range| range.contains(&start)) {
            cursor = range.end;
            continue;
        }
        let tail = &text[start..];
        if !svg_tag(tail, false) {
            // SVG-looking text inside another component's attributes is data.
            cursor = start + components::tag_end(tail).unwrap_or(1);
            continue;
        }
        out.push_str(&text[retained..start]);
        if let Some(length) = end(tail) {
            match svg::sanitize(&tail[..length]) {
                Ok(bytes) => {
                    let ordinal = p
                        .artifacts
                        .iter()
                        .filter(|a| a.media_type.as_deref() == Some("image/svg+xml"))
                        .count()
                        + 1;
                    let path = format!("{base}-{ordinal}.svg");
                    out.push_str(&format!("![Illustration]({path})"));
                    p.artifacts.push(RenderAsset {
                        relative_path: path.into(),
                        bytes,
                        references: vec![],
                        media_type: Some("image/svg+xml".into()),
                    });
                }
                Err(error) => out.push_str(&unavailable(p, artifact, &error.to_string())),
            }
            cursor = start + length;
        } else {
            out.push_str(&unavailable(p, artifact, "incomplete-illustration"));
            cursor = text.len();
        }
        retained = cursor;
    }
    out.push_str(&text[retained..]);
    out
}
