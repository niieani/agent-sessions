#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{archive::Archive, chatgpt, render};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
fn capture(input: &Path, name: &str, url: &str, payload: serde_json::Value) {
    let text = payload.to_string();
    std::fs::write(input.join(format!("{name}.json")), &text).unwrap();
    std::fs::write(input.join(format!("{name}.meta.json")),json!({"accountId":"fixture-account","url":url,"capturedAt":"2026-10-15T00:00:00Z","sha256":format!("{:x}",Sha256::digest(text.as_bytes()))}).to_string()).unwrap();
}
fn files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            result.extend(files(&p));
        } else if p.extension().is_some_and(|e| e == "md") && p.file_name().unwrap() != "INDEX.md" {
            result.push(p);
        }
    }
    result
}
#[test]
fn chatgpt_branch_dialogue_uses_standard_templates_without_repeated_prefix_or_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let user = json!({"id":"u","author":{"role":"user"},"content":{"parts":["Shared prompt"]},"create_time":1770000000});
    let first = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["Selected answer"]},"create_time":1770000010});
    let alternate = json!({"id":"b","author":{"role":"assistant"},"content":{"parts":["Alternate answer"]},"create_time":1770000020});
    capture(
        &input,
        "selected",
        "https://chatgpt.com/backend-api/conversations/c",
        json!({"update_time":1,"messages":[user,first],"page_info":{"has_previous_page":false}}),
    );
    capture(
        &input,
        "versions",
        "https://chatgpt.com/backend-api/conversations/c/versions?message_id=a",
        json!([[alternate]]),
    );
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    let db = archive.db().unwrap();
    let session = &db.sessions_with_dialog().unwrap()[0];
    assert_eq!(db.branches(session.id).unwrap().len(), 3);
    assert_eq!(render::render_all(&archive).unwrap().transcripts, 3);
    let paths = files(&archive.views_dir());
    let all = paths
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect::<String>();
    for text in ["Shared prompt", "Selected answer", "Alternate answer"] {
        assert_eq!(all.matches(text).count(), 1);
    }
    assert!(all.contains("\n\nShared prompt\n\n</user>"));
    assert!(all.contains("<assistant>"));
    assert!(!all.contains("<parent>"));
    assert!(!all.contains("<a id="));
    assert!(all.contains("<branches>"));
    for path in &paths {
        let text = std::fs::read_to_string(path).unwrap();
        for link in text.split("](").skip(1) {
            let target = link.split(')').next().unwrap();
            assert!(!target.contains('#'));
            assert!(
                path.parent()
                    .unwrap()
                    .join(shared::decode_href(target))
                    .is_file(),
                "{target}"
            );
        }
    }
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().branches(session.id).unwrap().len(), 3);
}
#[test]
fn empty_internal_branch_segments_are_forwarded_and_empty_root_has_resolving_links() {
    let hidden = |id: &str| json!({"id":id,"author":{"role":"assistant"},"channel":"analysis","content":{"parts":["internal"]},"create_time":1770000000});
    let dialog = |id: &str| json!({"id":id,"author":{"role":"user"},"content":{"parts":[id]},"create_time":1770000010});
    let graph=chatgpt::Graph::from_evidence("c",&[chatgpt::Evidence{format:"export".into(),origin:"fixture".into(),captured_at:0,sha256:"fixture".into(),payload:json!({"update_time":1,"current_node":"one","mapping":{"root":{"parent":null,"message":null},"bridge":{"parent":"root","message":hidden("bridge")},"hidden":{"parent":"root","message":hidden("hidden")},"one":{"parent":"bridge","message":dialog("one")},"two":{"parent":"bridge","message":dialog("two")}}}).to_string()}]).unwrap();
    let branches = graph.branches().unwrap();
    assert!(
        !branches
            .iter()
            .any(|b| b.id == "bridge" || b.id == "hidden")
    );
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let conversation = graph
        .conversation(agent_sessions::model::Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    let template = archive.root().join("branch-template.j2");
    std::fs::write(
        &template,
        "day: {{ day.index }}\n{% if day.index > 1 %}later{% endif %}\n{{ body }}",
    )
    .unwrap();
    std::fs::write(archive.root().join("config.toml"),format!("[views]\nunit='session-day'\ntimezone='UTC'\ntemplate={}\n[views.layout]\njson='json/{{{{ short_id }}}} {{{{ workday }}}}.json'\n",serde_json::to_string(&template).unwrap())).unwrap();
    render::render_all(&archive).unwrap();
    let contexts = std::fs::read_dir(archive.views_dir().join("json"))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(entry.unwrap().path()).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let root = contexts
        .iter()
        .find(|context| context["branch"]["parent_id"].is_null())
        .unwrap();
    assert_eq!(root["day"]["index"], 1);
    assert_eq!(files(&archive.views_dir()).len(), 2);
    for path in files(&archive.views_dir()) {
        let text = std::fs::read_to_string(&path).unwrap();
        for link in text.split("](").skip(1) {
            assert!(
                path.parent()
                    .unwrap()
                    .join(shared::decode_href(link.split(')').next().unwrap()))
                    .is_file()
            );
        }
    }
}
#[test]
fn branch_continuation_inherits_parent_turn_workday_without_repeating_parent_dialogue() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    std::fs::write(
        archive.root().join("config.toml"),
        "[views]\nunit = 'session-day'\ntimezone = 'UTC'\nday-start = '00:00'\n",
    )
    .unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let ts = |s: &str| agent_sessions::timefmt::parse_iso_ms(s).unwrap() as f64 / 1000.;
    let user = json!({"id":"u","author":{"role":"user"},"content":{"parts":["Parent turn"]},"create_time":ts("2026-01-01T23:50:00Z")});
    let answer = json!({"id":"a","author":{"role":"assistant"},"content":{"parts":["Cross-midnight reply"]},"create_time":ts("2026-01-02T00:30:00Z")});
    let alternate = json!({"id":"b","author":{"role":"user"},"content":{"parts":["New branch turn"]},"create_time":ts("2026-01-02T01:00:00Z")});
    capture(
        &input,
        "selected",
        "https://chatgpt.com/backend-api/conversations/c",
        json!({"update_time":1,"messages":[user,answer],"page_info":{"has_previous_page":false}}),
    );
    capture(
        &input,
        "versions",
        "https://chatgpt.com/backend-api/conversations/c/versions?message_id=a",
        json!([[alternate]]),
    );
    shared::collect(&archive, &input).unwrap();
    render::render_all(&archive).unwrap();
    let text = files(&archive.views_dir())
        .into_iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .find(|text| text.contains("Cross-midnight reply"))
        .unwrap();
    assert!(text.contains("activityDays:\n  - 2026-01-01\n"), "{text}");
    assert!(!text.contains("Parent turn"));
}
