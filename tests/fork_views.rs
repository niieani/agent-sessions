#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    chatgpt::{Evidence, Graph},
    render::render_all,
};
use serde_json::json;
use std::collections::BTreeMap;
fn graph(fork: bool) -> Graph {
    let mut mapping = json!({"u":{"parent":null,"message":{"id":"u","create_time":1770000000,"author":{"role":"user"},"content":{"parts":["Shared prompt"]}}},"a":{"parent":"u","message":{"id":"a","create_time":1770000010,"author":{"role":"assistant"},"content":{"parts":["Original tail"]}}}});
    if fork {
        mapping["b"] = json!({"parent":"u","message":{"id":"b","create_time":1770000020,"author":{"role":"assistant"},"content":{"parts":["New tail"]}}});
    }
    Graph::from_evidence("c",&[Evidence {format:"export".into(),origin:"fixture".into(),captured_at:1800000000000,sha256:"fixture".into(),payload:json!({"title":"Session title","create_time":1770000000,"update_time":1,"current_node":if fork {"b"}else{"a"},"mapping":mapping}).to_string()}]).unwrap()
}
fn files(root: &std::path::Path) -> BTreeMap<String, String> {
    shared::transcripts(root)
        .into_iter()
        .map(|p| {
            (
                p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(p).unwrap(),
            )
        })
        .collect()
}
#[test]
fn fork_moves_shared_dialogue_and_existing_file_keeps_its_tail() {
    let temp = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph(false), temp.path());
    render_all(&archive).unwrap();
    let before = files(&archive.views_dir());
    let path = before.keys().next().unwrap().clone();
    let mut conversation = graph(true)
        .conversation(agent_sessions::model::Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    conversation.created_at_ms = Some(1770000000000);
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    render_all(&archive).unwrap();
    let after = files(&archive.views_dir());
    assert_eq!(after.len(), 3);
    assert!(after[&path].contains("Original tail"));
    assert!(!after[&path].contains("Shared prompt"));
    let shared = after
        .values()
        .find(|t| t.contains("Shared prompt"))
        .unwrap();
    assert!(shared.contains("<branches>\n\n"));
    assert!(shared.contains("\n\n</branches>"));
    assert!(
        after
            .values()
            .all(|t| t.contains("<user>") || t.contains("<assistant>"))
    );
    assert!(
        after
            .values()
            .filter(|t| t.contains("tail"))
            .all(|t| t.contains("continues:"))
    );
    let fresh = tempfile::tempdir().unwrap();
    let imported = shared::graph_archive(&graph(true), fresh.path());
    render_all(&imported).unwrap();
    assert_eq!(after, files(&imported.views_dir()));
}

#[derive(Default)]
struct Client(std::sync::Mutex<Vec<agent_sessions::digest::ModelRequest>>);
impl agent_sessions::digest::ModelClient for Client {
    fn complete(&self, request: &agent_sessions::digest::ModelRequest) -> anyhow::Result<String> {
        self.0.lock().unwrap().push(request.clone());
        Ok(
            if request.kind == agent_sessions::digest::CompletionKind::SessionTitle {
                r#"{"title":"Session title"}"#.into()
            } else {
                {
                    let mut value = json!({"topic":"Session title","title":"Discussion","intent":"Answer the prompt","summary":"Conversation path.","tldr":null,"classification":["research"]});
                    if request.kind != agent_sessions::digest::CompletionKind::FirstSessionDay {
                        value.as_object_mut().unwrap().remove("topic");
                    }
                    value.to_string()
                }
            },
        )
    }
}
#[test]
fn only_path_ends_get_digests_and_navigation_does_not_redigest_an_existing_path() {
    use agent_sessions::digest::{self, DigestOptions};
    let temp = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph(false), temp.path());
    std::fs::write(archive.root().join("config.toml"),"[views]\nunit='session-day'\ntimezone='UTC'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='http://127.0.0.1:1/v1'\nmodel='fixture'\napi-key-env='UNUSED'\n").unwrap();
    let options = DigestOptions {
        day: None,
        from: None,
        through: None,
        session: None,
        force: false,
        dry_run: false,
        now_ms: 1900000000000,
    };
    let client = Client::default();
    let initial = digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!(initial.day_written, 1);
    let conversation = graph(true)
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
    let forked = digest::run_with_client(&archive, &client, &options).unwrap();
    assert_eq!(
        forked.day_written, 1,
        "only the new Path End needs a Digest"
    );
    assert_eq!(forked.failed, 0);
    let requests = client.0.lock().unwrap();
    let days = requests
        .iter()
        .filter(|r| r.kind != digest::CompletionKind::SessionTitle)
        .collect::<Vec<_>>();
    assert_eq!(days.len(), 2);
    assert!(days[1].user.contains("Shared prompt"));
    assert!(days[1].user.contains("New tail"));
    drop(requests);
    assert_eq!(
        digest::run_with_client(&archive, &client, &options)
            .unwrap()
            .candidates,
        0
    );
    render_all(&archive).unwrap();
    let rendered = files(&archive.views_dir());
    let shared = rendered
        .values()
        .find(|v| v.contains("Shared prompt"))
        .unwrap();
    assert!(!shared.contains("AI-derived"));
}

fn multiday_archive(
    root: &std::path::Path,
    unit: &str,
    long: bool,
) -> agent_sessions::archive::Archive {
    let archive = shared::graph_archive(&graph(true), root);
    let mut conversation = graph(true)
        .conversation(agent_sessions::model::Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    for branch in &mut conversation.branches {
        for message in &mut branch.messages {
            if message.text.contains("tail") {
                message.role = agent_sessions::model::Role::User;
                message.timestamp_ms = Some(1770086400000);
            } else if long {
                message.text = "Shared prompt ".repeat(4000);
            }
        }
    }
    for branch in &mut conversation.branches {
        if branch
            .messages
            .last()
            .is_some_and(|m| m.text.contains("tail"))
        {
            let mut end = branch.messages.last().unwrap().clone();
            end.role = agent_sessions::model::Role::Assistant;
            end.text = "Done".into();
            branch.messages.push(end);
        }
    }
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    std::fs::write(archive.root().join("template.j2"),"{% if day and day.digest %}{{ day.digest.summary }}{% elif session.digest is defined %}{{ session.digest.summary }}{% endif %}\n{% if branch.continues %}continues: {{ branch.continues.path }}{% endif %}\n{{ body }}").unwrap();
    let template = archive
        .root()
        .join("template.j2")
        .to_string_lossy()
        .into_owned();
    std::fs::write(archive.root().join("config.toml"),format!("[views]\nunit='{unit}'\ntimezone='UTC'\ntemplate={template:?}\n[views.layout]\nloose='{{{{ title }}}} {{{{ workday }}}}.md'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='http://127.0.0.1:1/v1'\nmodel='fixture'\napi-key-env='UNUSED'\nmax-input-tokens=1800\n")).unwrap();
    archive
}
fn options() -> agent_sessions::digest::DigestOptions {
    agent_sessions::digest::DigestOptions {
        day: None,
        from: None,
        through: None,
        session: None,
        force: false,
        dry_run: false,
        now_ms: 1900000000000,
    }
}
#[test]
fn multiday_shared_files_and_path_end_digests_survive_both_view_units() {
    for unit in ["session", "session-day"] {
        let temp = tempfile::tempdir().unwrap();
        let archive = multiday_archive(temp.path(), unit, false);
        let result =
            agent_sessions::digest::run_with_client(&archive, &Client::default(), &options())
                .unwrap();
        assert_eq!(result.failed, 0);
        assert_eq!(result.day_written, 2);
        render_all(&archive).unwrap();
        let rendered = files(&archive.views_dir());
        assert_eq!(rendered.len(), 3, "{unit}: {rendered:?}");
        assert_eq!(
            rendered
                .values()
                .map(|t| t.matches("Shared prompt").count())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            rendered
                .values()
                .filter(|t| t.contains("tail"))
                .filter(|t| t.contains("Conversation path.") && t.contains("continues:"))
                .count(),
            2
        );
        let conn = rusqlite::Connection::open(archive.db_path()).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM digest_cache", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        let repaired = agent_sessions::digest::repair_with_client(
            &archive,
            &Client::default(),
            &agent_sessions::digest::RepairOptions {
                mode: agent_sessions::digest::RepairMode::Loose,
                dry_run: false,
                force: true,
                now_ms: 1900000000000,
            },
        )
        .unwrap();
        assert_eq!(repaired.failed, 0, "{:?}", repaired.errors);
        assert_eq!(repaired.repaired, 1);
    }
}
#[test]
fn preceding_shared_segments_share_the_input_budget_and_keep_the_tail() {
    let temp = tempfile::tempdir().unwrap();
    let archive = multiday_archive(temp.path(), "session-day", true);
    let client = Client::default();
    let result = agent_sessions::digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!(result.failed, 0);
    assert_eq!(result.day_written, 2);
    assert!(
        result
            .previews
            .iter()
            .all(|p| p.truncated && p.input_tokens <= 1800)
    );
    assert!(
        client
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.kind != agent_sessions::digest::CompletionKind::SessionTitle)
            .all(|r| r.user.contains("tail"))
    );
}

#[test]
fn opening_edits_preserve_orphaned_media_on_the_oldest_session() {
    let payload = json!({"update_time":1,"mapping":{"u":{"parent":null,"message":{"id":"u","create_time":1,"author":{"role":"user"},"content":{"parts":["First"]}}},"v":{"parent":null,"message":{"id":"v","create_time":2,"author":{"role":"user"},"content":{"parts":["Edited"]}}}}});
    let mut graph = Graph::from_evidence(
        "c",
        &[Evidence {
            format: "export".into(),
            origin: "fixture".into(),
            captured_at: 3,
            sha256: "fixture".into(),
            payload: payload.to_string(),
        }],
    )
    .unwrap();
    graph.exported_media.insert(
        "orphan".into(),
        json!({"assetId":"orphan","messageId":"absent-from-mapping","mime":"image/png"}),
    );
    let sessions = graph
        .sessions(agent_sessions::model::Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: "c".into(),
            native_path: "fixture".into(),
        })
        .unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .flat_map(|s| &s.messages)
            .filter(|m| m.text.contains("orphan"))
            .count(),
        1
    );
}
