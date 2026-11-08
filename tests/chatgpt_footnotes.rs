#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{archive::Archive, render};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
fn capture(input: &Path, payload: serde_json::Value) {
    let text = payload.to_string();
    std::fs::write(input.join("page.json"), &text).unwrap();
    std::fs::write(input.join("page.meta.json"),json!({"accountId":"fixture-account","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-16T00:00:00Z","sha256":format!("{:x}",Sha256::digest(text.as_bytes()))}).to_string()).unwrap();
}
fn markdown(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .flat_map(|e| {
            let p = e.unwrap().path();
            if p.is_dir() {
                markdown(&p)
            } else if p.extension().is_some_and(|x| x == "md")
                && p.file_name().unwrap() != "INDEX.md"
            {
                vec![p]
            } else {
                vec![]
            }
        })
        .collect()
}
#[test]
fn generated_sources_become_deduplicated_file_footnotes_without_rewriting_original_links_or_code() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let metadata = json!({"content_references":[{"type":"grouped_webpages","items":[{"title":"Source title","url":"https://example.com/source","refs":[{"turn_index":1,"ref_type":"search","ref_index":0}]}]}]});
    let user = "Original [link](https://example.com/source) and `citeturn1search0` stay.";
    capture(
        &input,
        json!({"update_time":1,"messages":[
      {"id":"u","author":{"role":"user"},"content":{"parts":[user]},"create_time":1770000000},
      {"id":"a","author":{"role":"assistant"},"content":{"parts":["First. citeturn1search0 Again. citeturn1search0\n\nLiteral [manual link](https://example.com/source).\n```md\nciteturn1search0\n```"]},"metadata":metadata,"create_time":1770000010},
      {"id":"b","author":{"role":"assistant"},"content":{"parts":["Another response. citeturn1search0"]},"metadata":metadata,"create_time":1770000020}
    ],"page_info":{"has_previous_page":false}}),
    );
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    render::render_all(&archive).unwrap();
    let files = markdown(&archive.views_dir());
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains(user));
    assert!(text.contains("[manual link](https://example.com/source)"));
    assert!(text.contains("```md\nciteturn1search0\n```"));
    assert_eq!(
        text.matches(": [Source title](https://example.com/source)")
            .count(),
        1,
        "{text}"
    );
    let id = text
        .lines()
        .find(|l| l.starts_with("[^asessions-"))
        .unwrap()
        .split("]:")
        .next()
        .unwrap();
    assert_eq!(
        text.split(&format!("{id}]:"))
            .next()
            .unwrap()
            .matches(&format!("{id}]"))
            .count(),
        3
    );
    assert!(text.rfind("</assistant>").unwrap() < text.find(&format!("{id}]:")).unwrap());
    assert!(!text.contains("asessions-reference:"));
    assert_eq!(render::render_all(&archive).unwrap().changed, 0);
}
#[test]
fn rich_source_footnotes_are_relative_and_exist_with_a_custom_template() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let template = dir.path().join("template.j2");
    std::fs::write(&template, "{{ body }}").unwrap();
    std::fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views]\ntemplate={}\n[views.layout]\nloose='deep/nested/{{{{ short_id }}}}.md'\n",
            serde_json::to_string(&template).unwrap()
        ),
    )
    .unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"content":{"parts":["<Entity value={sensor.name}/> and <Entity value={sensor.name}/>."]},"metadata":{"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{"sensor":{"name":"Temperature sensor"}}}}}},"create_time":1770000010}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let p = markdown(&archive.views_dir()).pop().unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("Temperature sensor"));
    assert!(!text.contains("Rich content"));
    let definitions = text
        .lines()
        .filter(|l| l.starts_with("[^asessions-"))
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{text}");
    let href = definitions[0]
        .split("](")
        .nth(1)
        .unwrap()
        .trim_end_matches(')');
    assert!(href.starts_with("asessions-capture:chatgpt/c/"), "{href}");
    assert!(!archive.views_dir().join("assets").exists());
}

#[test]
fn multiline_markdown_names_and_source_titles_render_as_literal_footnotes() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let name = "Sensor [west]\n`calibrated` <name>";
    let source = "Manual [edition]\n`revision` <source>";
    capture(
        &input,
        json!({"update_time":1,"messages":[{
        "id":"a","author":{"role":"assistant"},"create_time":1770000010,
        "content":{"parts":["<Entity value={sensor.name}/> citeturn1view0 Again: <Entity value={sensor.name}/> citeturn1view0"]},
        "metadata":{"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{"sensor":{"name":name}}}}},
          "content_references":[{"type":"grouped_webpages","items":[{"title":source,"url":"https://example.com/manual","refs":[{"turn_index":1,"ref_type":"view","ref_index":0}]}]}]}
    }],"page_info":{"has_previous_page":false}}),
    );
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    render::render_all(&archive).unwrap();
    let path = markdown(&archive.views_dir()).pop().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("asessions-reference:"), "{text}");
    assert!(
        text.contains("Sensor \\[west\\] &#96;calibrated&#96; &lt;name&gt;[^asessions-"),
        "{text}"
    );
    assert!(
        text.contains(
            ": [Manual \\[edition\\] &#96;revision&#96; &lt;source&gt;](https://example.com/manual)"
        ),
        "{text}"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("[^asessions-"))
            .count(),
        2,
        "{text}"
    );
    assert_eq!(render::render_all(&archive).unwrap().changed, 0);
}

#[test]
fn source_footnote_url_parentheses_do_not_unbalance_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Definition. citeturn1view0"]},"metadata":{"content_references":[{"url":"https://en.wikipedia.org/wiki/Function_(mathematics)","title":"Function","refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    assert!(
        text.contains(": [Function](https://en.wikipedia.org/wiki/Function_%28mathematics%29)"),
        "{text}"
    );
}

#[test]
fn relative_and_backslash_citation_destinations_render_as_complete_links() {
    for (url, expected) in [
        ("/c/a)b", "https://chatgpt.com/c/a%29b"),
        (
            "https://example.com/trailing\\",
            "https://example.com/trailing%5C",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let input = dir.path().join("input");
        std::fs::create_dir(&input).unwrap();
        capture(
            &input,
            json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Source. citeturn1view0"]},"metadata":{"content_references":[{"url":url,"title":"Source","refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
        );
        shared::collect(&archive, &input).unwrap();
        render::render_all(&archive).unwrap();
        let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
        let links =
            pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::ENABLE_FOOTNOTES)
                .filter_map(|event| match event {
                    pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                        dest_url, ..
                    }) => Some(dest_url.into_string()),
                    _ => None,
                })
                .collect::<Vec<_>>();
        assert_eq!(links, vec![expected], "{text}");
    }
}

#[test]
fn captured_names_keep_literal_pipes_and_tildes_inside_markdown_tables() {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,
       "content":{"parts":["| Sensor | State |\n| --- | --- |\n| <Entity value={sensor.name}/> | Ready |"]},
       "metadata":{"model_dil_v2":{"appData":{"opGenui":{"modelDataBindings":{"sensor":{"name":"West | ~~Do not strike~~"}}}}}}
    }],"page_info":{"has_previous_page":false}}),
    );
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    render::render_all(&archive).unwrap();
    let rendered = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    let mut cells = Vec::new();
    let mut current = None;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES;
    let dialogue = rendered.split_once("<assistant>").unwrap().1;
    let (body, notes) = dialogue.split_once("\n</assistant>").unwrap();
    let markdown = format!("{body}\n\n{notes}");
    for event in Parser::new_ext(&markdown, options) {
        match event {
            Event::Start(Tag::TableCell) => current = Some(String::new()),
            Event::End(TagEnd::TableCell) => cells.push(current.take().unwrap()),
            Event::Text(text) if current.is_some() => current.as_mut().unwrap().push_str(&text),
            Event::Start(Tag::Strikethrough) => {
                panic!("Captured name became strikethrough: {rendered}")
            }
            _ => {}
        }
    }
    assert_eq!(
        cells,
        vec!["Sensor", "State", "West | ~~Do not strike~~", "Ready"],
        "{rendered}"
    );
    assert!(!rendered.contains("asessions-reference:"));
}

#[test]
fn one_citation_group_deduplicates_same_url_with_different_captured_titles() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Evidence. citeturn1view0turn1view1"]},"metadata":{"content_references":[{"url":"https://example.com/source","title":"Original title","refs":["turn1view0"]},{"url":"https://example.com/source","title":"Another captured title","refs":["turn1view1"]}]}}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    assert!(
        text.contains("Evidence. [^asessions-1]\n\n</assistant>"),
        "{text}"
    );
    assert_eq!(
        text.matches(": [Original title](https://example.com/source)")
            .count(),
        1
    );
}

#[test]
fn blank_citation_titles_use_captured_attribution_or_source_url() {
    for (attribution, expected) in [
        ("  ", "https://example.com/source"),
        ("Source publisher", "Source publisher"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let input = dir.path().join("input");
        std::fs::create_dir(&input).unwrap();
        capture(
            &input,
            json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Evidence. citeturn1view0"]},"metadata":{"content_references":[{"url":"https://example.com/source","title":" \n ","attribution":attribution,"refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
        );
        shared::collect(&archive, &input).unwrap();
        render::render_all(&archive).unwrap();
        let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
        assert!(
            text.contains(&format!(": [{expected}](https://example.com/source)")),
            "{text}"
        );
    }
}

#[test]
fn original_unmatched_brackets_are_preserved_before_generated_citations() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Keep this [ unmatched bracket and this sentence. citeturn1view0"]},"metadata":{"content_references":[{"url":"https://example.com/source","title":"Source","refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    assert!(
        text.contains("Keep this [ unmatched bracket and this sentence. [^asessions-1]"),
        "{text}"
    );
}

#[test]
fn an_unclosed_source_fence_in_an_earlier_message_does_not_remove_later_footnote_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"a","author":{"role":"assistant"},"create_time":1770000010,"content":{"parts":["Example:\n\n```text\nOriginal unclosed code example"]}},{"id":"b","author":{"role":"assistant"},"create_time":1770000020,"content":{"parts":["Later evidence. citeturn1view0"]},"metadata":{"content_references":[{"url":"https://example.com/source","title":"Source","refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    assert!(text.contains("Example:\n\n```text\nOriginal unclosed code example"));
    let mut inside = false;
    let mut code = String::new();
    let mut definitions = 0;
    for event in pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::ENABLE_FOOTNOTES) {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_)) => inside = true,
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::CodeBlock) => inside = false,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::FootnoteDefinition(_)) => {
                definitions += 1
            }
            pulldown_cmark::Event::Text(value) if inside => code.push_str(&value),
            _ => {}
        }
    }
    assert!(!code.contains("</assistant>"), "{text}");
    assert_eq!(definitions, 1, "{text}");
    assert!(text.contains("<!-- Unclosed source code fence closed for display. -->"));
    let db = archive.db().unwrap();
    let session = db.sessions_with_dialog().unwrap().remove(0);
    assert_eq!(
        db.messages(session.id).unwrap()[0].text,
        "Example:\n\n```text\nOriginal unclosed code example"
    );
    assert!(text.contains("Later evidence. [^asessions-1]"));
    assert!(
        text.contains("[^asessions-1]: [Source](https://example.com/source)"),
        "{text}"
    );
}

#[test]
fn role_wrappers_preserve_balanced_code_blocks_and_later_markdown() {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let original = "```text\nfirst\n\nsecond\n```";
    capture(
        &input,
        json!({"update_time":1,"messages":[{"id":"u","author":{"role":"user"},"create_time":1770000010,"content":{"parts":[original]}},{"id":"a","author":{"role":"assistant"},"create_time":1770000020,"content":{"parts":["Later evidence. citeturn1view0"]},"metadata":{"content_references":[{"url":"https://example.com/source","title":"Source","refs":["turn1view0"]}]}}],"page_info":{"has_previous_page":false}}),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = std::fs::read_to_string(markdown(&archive.views_dir()).pop().unwrap()).unwrap();
    assert!(text.contains(original));
    let mut code = String::new();
    let mut inside = false;
    let mut later = false;
    let mut definitions = 0;
    for event in Parser::new_ext(&text, Options::ENABLE_FOOTNOTES) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => inside = true,
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => inside = false,
            Event::Start(Tag::FootnoteDefinition(_)) => definitions += 1,
            Event::Text(t) if inside => code.push_str(&t),
            Event::Text(t) => later |= t.contains("Later evidence."),
            _ => {}
        }
    }
    assert_eq!(code, "first\n\nsecond\n", "{text}");
    assert!(later, "{text}");
    assert_eq!(definitions, 1, "{text}");
}
