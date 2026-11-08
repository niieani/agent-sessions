//! Tokenize native rich-content markup, never evaluate its JavaScript.
use super::{
    Presentation,
    bindings::{Bindings, field_label, humanize},
    citations::{self, Citations},
};
use crate::model::ReferenceKind;
use serde_json::Value;
use std::collections::BTreeMap;
mod layout;

pub(super) fn normalize_layout(text: &str, native: &Value) -> String {
    layout::normalize(text, native)
}

fn paragraph(out: &mut String) {
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    if !out.is_empty() {
        out.push_str("\n\n");
    }
}

fn balanced_end(text: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, byte) in text.as_bytes()[start..].iter().copied().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == q {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'"' | b'\'') {
            quote = Some(byte);
        } else if byte == open {
            depth += 1;
        } else if byte == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset + 1);
            }
        }
    }
    None
}
pub(super) fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut braces = 0;
    let mut escaped = false;
    for (i, b) in text.bytes().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'{' => braces += 1,
            b'}' => braces -= 1,
            b'>' if braces == 0 => return Some(i + 1),
            _ => {}
        }
    }
    None
}
fn attributes(mut text: &str) -> BTreeMap<String, (String, bool)> {
    let mut out = BTreeMap::new();
    while !text.trim_start().is_empty() {
        text = text.trim_start();
        let n = text
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            .count();
        if n == 0 {
            break;
        }
        let key = &text[..n];
        text = text[n..].trim_start();
        if !text.starts_with('=') {
            break;
        }
        text = text[1..].trim_start();
        match text.as_bytes().first().copied() {
            Some(q @ (b'"' | b'\'')) => {
                let Some(n) = text[1..].find(q as char).map(|i| i + 1) else {
                    break;
                };
                out.insert(key.into(), (text[1..n].into(), false));
                text = &text[n + 1..];
            }
            Some(b'{') => {
                let Some(n) = balanced_end(text, 0, b'{', b'}') else {
                    break;
                };
                out.insert(key.into(), (text[1..n - 1].into(), true));
                text = &text[n..];
            }
            _ => break,
        }
    }
    out
}
fn unresolved(p: &mut Presentation, artifact: &str, kind: &str, label: &str) -> String {
    p.issues.push(kind.into());
    p.source_link(label, artifact, ReferenceKind::Label)
}

fn component_label(name: &str, attrs: &BTreeMap<String, (String, bool)>) -> String {
    for key in ["title", "label"] {
        if let Some((value, false)) = attrs.get(key) {
            return value.clone();
        }
    }
    for key in ["content", "data"] {
        if let Some((value, true)) = attrs.get(key)
            && let Ok(data) = serde_json::from_str::<Value>(value)
            && let Some(label) = data
                .pointer("/meta/title")
                .or_else(|| data.get("title"))
                .or_else(|| data.get("label"))
                .and_then(Value::as_str)
        {
            return label.into();
        }
    }
    humanize(name)
}

pub(super) fn convert(
    text: &str,
    native: &Value,
    artifact: &str,
    citations: &Citations,
    p: &mut Presentation,
) -> String {
    let bindings = Bindings::new(native);
    let rich = native
        .pointer("/metadata/model_dil_v2")
        .is_some_and(|v| !v.is_null())
        || native["metadata"]["genui_components"]
            .as_array()
            .is_some_and(|v| !v.is_empty());
    let mut out = String::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let tail = &text[cursor..];
        if tail.starts_with('')
            && let Some(end) = tail.find('').map(|end| end + ''.len_utf8())
        {
            // JSON inside a native marker belongs to its marker interpreter.
            out.push_str(&tail[..end]);
            cursor += end;
            continue;
        }
        if tail.starts_with('<')
            && let Some(end) = tag_end(tail)
        {
            let token = &tail[1..end - 1];
            let close = token.starts_with('/');
            let trimmed = token.trim_start_matches('/');
            let n = trimmed
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                .count();
            let name = &trimmed[..n];
            if rich && n > 0 {
                let attrs = attributes(trimmed[n..].trim_end_matches('/'));
                match name {
                    "Entity" if !close => {
                        let id = attrs.get("ref").map(|(v, _)| v.as_str());
                        let resolved = match attrs.get("value") {
                            Some((value, false)) => Some(value.clone()),
                            value => bindings.entity(value.map(|(v, _)| v.as_str()), id),
                        };
                        out.push_str(&if let Some(value) = resolved {
                            p.source_link(&value, artifact, ReferenceKind::Label)
                        } else {
                            let category = attrs
                                .get("category")
                                .map(|(v, _)| v.as_str())
                                .unwrap_or("entity");
                            unresolved(
                                p,
                                artifact,
                                "unresolved-component-binding",
                                &format!("Unresolved {category}"),
                            )
                        });
                    }
                    "Link" if !close => {
                        let value = |key: &str| {
                            attrs.get(key).and_then(|(v, expression)| {
                                if *expression {
                                    bindings.expression(v)
                                } else {
                                    Some(v.clone())
                                }
                            })
                        };
                        let label = value("title").unwrap_or_else(|| "Link".into());
                        out.push_str(&if let Some(url) =
                            value("url").and_then(|v| citations::url(&v))
                        {
                            p.source_link(&label, &url, ReferenceKind::Label)
                        } else {
                            unresolved(p, artifact, "unresolved-component-link", &label)
                        });
                    }
                    "icon" | "Icon" if !close => {
                        let icon = attrs.get("name").map(|(v, _)| v.as_str()).unwrap_or("");
                        out.push_str(match icon {
                            "check" | "check-circle" | "check-circle-2" | "checkmark" | "Check" => {
                                "✓"
                            }
                            "x" | "x-circle" | "close" | "X" => "✗",
                            "warning" | "alert-triangle" => "⚠",
                            _ => "",
                        });
                        if !matches!(
                            icon,
                            "check"
                                | "check-circle"
                                | "check-circle-2"
                                | "checkmark"
                                | "Check"
                                | "x"
                                | "x-circle"
                                | "close"
                                | "X"
                                | "warning"
                                | "alert-triangle"
                        ) {
                            out.push_str(&unresolved(
                                p,
                                artifact,
                                "unsupported-icon",
                                &format!("Unsupported symbol: {icon}"),
                            ));
                        }
                    }
                    "AsyncImage" if !close => {
                        let label = attrs
                            .get("ref")
                            .and_then(|(id, _)| bindings.name(id))
                            .map(|name| format!("Image: {name}"))
                            .unwrap_or_else(|| "Image reference".into());
                        out.push_str(&p.source_link(&label, artifact, ReferenceKind::Label));
                    }
                    "Cite" | "MemoryCite" if !close => {
                        let ids = attrs.get("ref").map(|(v, _)| vec![v.clone()]).or_else(|| {
                            attrs
                                .get("refs")
                                .and_then(|(v, _)| serde_json::from_str::<Vec<String>>(v).ok())
                        });
                        let resolved = ids.and_then(|ids| citations.resolve(&ids, p));
                        out.push_str(&resolved.unwrap_or_else(|| {
                            unresolved(
                                p,
                                artifact,
                                "unresolved-component-citation",
                                "Sources unavailable",
                            )
                        }));
                    }
                    "grid" | "grid-item" | "title" | "box" | "row" | "text" | "caption"
                    | "badge" | "span" | "div" | "p" => {
                        if matches!(
                            name,
                            "grid"
                                | "grid-item"
                                | "title"
                                | "box"
                                | "row"
                                | "text"
                                | "caption"
                                | "div"
                                | "p"
                        ) {
                            paragraph(&mut out);
                        }
                    }
                    "bold" | "strong" => out.push_str("**"),
                    "italic" | "em" => out.push('*'),
                    "divider" | "br" => out.push_str("\n\n"),
                    _ if close => {}
                    _ => out.push_str(&unresolved(
                        p,
                        artifact,
                        &format!("unsupported-component:{name}"),
                        &component_label(name, &attrs),
                    )),
                }
                cursor += end;
                continue;
            }
        }
        if rich
            && tail.starts_with('{')
            && let Some(end) = balanced_end(tail, 0, b'{', b'}')
        {
            let expression = &tail[1..end - 1];
            out.push_str(&bindings.expression(expression).unwrap_or_else(|| {
                unresolved(
                    p,
                    artifact,
                    "unresolved-expression",
                    &field_label(expression),
                )
            }));
            cursor += end;
            continue;
        }
        let ch = tail.chars().next().unwrap();
        out.push(ch);
        cursor += ch.len_utf8();
    }
    citations.replace(&out, p, artifact)
}
