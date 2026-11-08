//! Remove JSX layout margins, keeping Markdown lists and protected code intact.
use serde_json::Value;

struct Frame {
    name: String,
    margin: Option<usize>,
}
#[derive(Default)]
struct Layout {
    frames: Vec<Frame>,
    line_start: bool,
    source_indent: usize,
    after_close: bool,
    leading: bool,
}
fn block(name: &str) -> bool {
    matches!(
        name,
        "grid" | "grid-item" | "title" | "box" | "row" | "text" | "caption" | "div" | "p"
    )
}
fn tag(token: &str) -> (&str, bool) {
    let body = token.trim_start_matches('<');
    let close = body.starts_with('/');
    let body = body.trim_start_matches('/');
    let end = body
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        .count();
    (&body[..end], close)
}
impl Layout {
    fn prose(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut cursor = 0;
        while cursor < text.len() {
            let tail = &text[cursor..];
            if self.after_close {
                let whitespace = tail.len() - tail.trim_start_matches(char::is_whitespace).len();
                if whitespace > 0 {
                    if tail[..whitespace].contains('\n') {
                        self.line_start = true;
                        self.source_indent = 0;
                    }
                    cursor += whitespace;
                    continue;
                }
                self.after_close = false;
            }
            if self.line_start {
                let indent = tail
                    .bytes()
                    .take_while(|b| matches!(b, b' ' | b'\t'))
                    .count();
                self.source_indent = indent;
                let next = &tail[indent..];
                let markup = next.starts_with('<') && super::tag_end(next).is_some();
                let layout_tag = markup && block(tag(next).0);
                if let Some(frame) = self.frames.last_mut() {
                    if !markup && !next.starts_with('\n') && !next.is_empty() {
                        let margin = *frame.margin.get_or_insert(indent);
                        if !self.leading {
                            out.push_str(&tail[..indent.saturating_sub(margin)]);
                        }
                    }
                } else if !layout_tag {
                    out.push_str(&tail[..indent]);
                }
                cursor += indent;
                self.line_start = false;
                if cursor == text.len() {
                    break;
                }
            }
            let tail = &text[cursor..];
            if !tail.starts_with(char::is_whitespace) {
                self.leading = false;
            }
            if tail.starts_with('')
                && let Some(end) = tail.find('').map(|end| end + ''.len_utf8())
            {
                out.push_str(&tail[..end]);
                self.line_start = tail[..end].ends_with('\n');
                cursor += end;
                continue;
            }
            if tail.starts_with("<svg") {
                // SVG text/layout semantics belong to the SVG interpreter.
                let end = tail.find("</svg>").map_or(tail.len(), |end| end + 6);
                let svg = &tail[..end];
                out.push_str(svg);
                self.line_start = svg.ends_with('\n');
                cursor += end;
                continue;
            }
            if tail.starts_with('<')
                && let Some(end) = super::tag_end(tail)
            {
                let token = &tail[..end];
                let (name, close) = tag(token);
                if block(name) {
                    if close {
                        if let Some(index) = self.frames.iter().rposition(|f| f.name == name) {
                            self.frames.truncate(index);
                        }
                        self.after_close = true;
                    } else if !token.trim_end_matches('>').trim_end().ends_with('/') {
                        self.frames.push(Frame {
                            name: name.into(),
                            margin: (!tail[end..].trim_start_matches(' ').starts_with('\n'))
                                .then_some(self.source_indent),
                        });
                        self.leading = true;
                    }
                }
                out.push_str(token);
                cursor += end;
                continue;
            }
            let ch = tail.chars().next().unwrap();
            if ch == '\n' {
                if !self.leading && (self.frames.is_empty() || !out.ends_with("\n\n")) {
                    out.push(ch);
                }
                self.line_start = true;
                self.source_indent = 0;
            } else {
                out.push(ch);
            }
            cursor += ch.len_utf8();
        }
        out
    }
}

pub(super) fn normalize(text: &str, native: &Value) -> String {
    let metadata = &native["metadata"];
    let rich = metadata.get("model_dil_v2").is_some_and(|v| !v.is_null())
        || metadata["genui_components"]
            .as_array()
            .is_some_and(|a| !a.is_empty());
    if !rich {
        return text.into();
    }
    let mut layout = Layout {
        line_start: true,
        ..Layout::default()
    };
    crate::markdown::map_layout_prose(text, |prose| layout.prose(prose))
}
