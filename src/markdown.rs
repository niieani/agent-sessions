//! Transform prose while retaining the exact original bytes of Markdown code.
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

pub(crate) fn map_prose(text: &str, transform: impl FnMut(&str) -> String) -> String {
    map(text, true, transform)
}

/// Native JSX uses indentation for layout; it becomes Markdown only after dedenting.
pub(crate) fn map_layout_prose(text: &str, transform: impl FnMut(&str) -> String) -> String {
    map(text, false, transform)
}

fn map(text: &str, indented: bool, mut transform: impl FnMut(&str) -> String) -> String {
    let mut protected = Vec::new();
    let mut block = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                if indented || matches!(kind, CodeBlockKind::Fenced(_)) {
                    block = Some(range.start);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = block.take() {
                    protected.push(start..range.end);
                }
            }
            Event::Code(_) => protected.push(range),
            _ => {}
        }
    }
    protected.sort_by_key(|r| r.start);
    let mut out = String::new();
    let mut cursor = 0;
    for range in protected {
        if range.start < cursor {
            continue;
        }
        out.push_str(&transform(&text[cursor..range.start]));
        out.push_str(&text[range.clone()]);
        cursor = range.end;
    }
    out.push_str(&transform(&text[cursor..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indented_code_and_inline_ticks_are_literal_but_later_prose_transforms() {
        let original = "    literal TOKEN\n\n```TOKEN``` followed by TOKEN\n\nMore TOKEN.";
        assert_eq!(
            map_prose(original, |text| text.replace("TOKEN", "changed")),
            "    literal TOKEN\n\n```TOKEN``` followed by changed\n\nMore changed."
        );
    }
    #[test]
    fn escaped_ticks_do_not_hide_prose_and_nested_code_is_preserved() {
        let original = "Literal \\`TOKEN\\` is prose.\n\n- List:\n\n      code TOKEN\n\n  more TOKEN\n\n> ```text\n> TOKEN\n> ```\n\nTOKEN";
        let result = map_prose(original, |text| text.replace("TOKEN", "changed"));
        assert!(
            result.starts_with("Literal \\`changed\\` is prose."),
            "{result}"
        );
        assert!(result.contains("      code TOKEN"), "{result}");
        assert!(result.contains("  more changed"), "{result}");
        assert!(result.contains("> TOKEN\n> ```"), "{result}");
        assert!(result.ends_with("changed"), "{result}");
    }
}
