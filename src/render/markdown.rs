//! Close incomplete source fences for display without changing source messages.
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use std::borrow::Cow;

pub(super) const REPAIR_NOTE: &str = "<!-- Unclosed source code fence closed for display. -->";

pub(super) struct Fragment<'a> {
    pub text: Cow<'a, str>,
    pub repaired: bool,
}
impl<'a> Fragment<'a> {
    pub fn new(text: Cow<'a, str>) -> Self {
        let Some(fence) = unterminated(&text) else {
            return Self {
                text,
                repaired: false,
            };
        };
        let mut rendered = text.into_owned();
        if !rendered.ends_with(['\r', '\n']) {
            rendered.push_str(fence.newline)
        }
        rendered.push_str(&fence.delimiter);
        rendered.push_str(fence.newline);
        Self {
            text: Cow::Owned(rendered),
            repaired: true,
        }
    }
}

struct Fence {
    delimiter: String,
    newline: &'static str,
    content_end: usize,
}
fn unterminated(source: &str) -> Option<Fence> {
    let mut depth = 0;
    let mut active: Option<Fence> = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 && matches!(tag, Tag::CodeBlock(CodeBlockKind::Fenced(_))) {
                    let opening = &source[range.start..];
                    let marker = opening.chars().next()?;
                    let width = opening.bytes().take_while(|b| *b == marker as u8).count();
                    let line_end = opening.find(['\r', '\n']).unwrap_or(opening.len());
                    let newline = if opening[line_end..].starts_with("\r\n") {
                        "\r\n"
                    } else if opening[line_end..].starts_with('\r') {
                        "\r"
                    } else {
                        "\n"
                    };
                    let content_end = range.start
                        + line_end
                        + if line_end < opening.len() {
                            newline.len()
                        } else {
                            0
                        };
                    active = Some(Fence {
                        delimiter: marker.to_string().repeat(width),
                        newline,
                        content_end,
                    });
                }
                depth += 1;
            }
            Event::Text(_) => {
                if let Some(fence) = active.as_mut() {
                    fence.content_end = range.end;
                }
            }
            Event::End(tag) => {
                depth -= 1;
                if tag == TagEnd::CodeBlock
                    && depth == 0
                    && let Some(fence) = active.take()
                    && range.end == source.len()
                    && fence.content_end == range.end
                {
                    return Some(fence);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unchanged(source: &str) {
        let shown = Fragment::new(Cow::Borrowed(source));
        assert!(!shown.repaired, "{source:?}");
        assert_eq!(shown.text, source);
        assert!(matches!(shown.text, Cow::Borrowed(_)));
    }
    fn repaired(source: &str, suffix: &str) {
        let shown = Fragment::new(Cow::Borrowed(source));
        assert!(shown.repaired, "{source:?}");
        assert_eq!(shown.text, format!("{source}{suffix}"));
        assert!(!shown.text.contains(REPAIR_NOTE));
        assert!(!Fragment::new(shown.text).repaired);
    }
    #[test]
    fn ordinary_markdown_inline_code_and_balanced_fences_stay_exact() {
        for source in [
            "",
            "Ordinary **prose**. `code`",
            "```inline code```",
            "Some ~~~ inline ~~~ prose",
            "```rust\nx\n```",
            "~~~~js\nx\n~~~~~\n",
            "```\n```",
            "```rust\nx\n```\n  ",
        ] {
            unchanged(source);
        }
    }
    #[test]
    fn top_level_unclosed_fences_append_only_the_matching_closer() {
        repaired("Before.\n\n```rust\nlet x = 1;", "\n```\n");
        repaired("~~~js\nx\n", "~~~\n");
        repaired("`````text\nA shorter ``` is content\n", "`````\n");
        repaired("~~~~\nx\n~~~", "\n~~~~\n");
    }
    #[test]
    fn empty_unclosed_fences_are_closed_too() {
        repaired("```", "\n```\n");
        repaired("~~~text\n", "~~~\n");
        repaired("```\n\n", "```\n");
    }
    #[test]
    fn implicit_container_ends_and_indented_code_are_not_repaired() {
        for source in [
            "> ```rust\n> x",
            "> ```rust\n> x\n\nOutside",
            "- Item\n\n  ```rust\n  x",
            "1. ```rust\n   x",
            "    x\n",
            "    ```rust\n    x",
        ] {
            unchanged(source);
        }
    }
    #[test]
    fn crlf_unicode_and_opening_indentation_keep_original_bytes() {
        repaired("Ω\r\n\r\n  ~~~~rust\r\nlet café = \"☕\";\r\n", "~~~~\r\n");
        unchanged("```rust\r\nlet café = \"☕\";\r\n```\r\n");
    }
    #[test]
    fn owned_input_preserves_ownership_without_a_repair() {
        let fragment = Fragment::new(Cow::Owned("Already balanced".into()));
        assert!(!fragment.repaired);
        assert!(matches!(fragment.text, Cow::Owned(_)));
    }
}
