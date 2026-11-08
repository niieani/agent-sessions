mod common;
use agent_sessions::{
    archive::Archive,
    digest::{self, CompletionKind, DigestOptions, ModelClient, ModelRequest},
    render::render_all,
};
use std::{fs, sync::Mutex};

struct Client(Mutex<Vec<serde_json::Value>>);
impl ModelClient for Client {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        let context = if request.kind == CompletionKind::SessionTitle {
            request.user.as_str()
        } else {
            request
                .user
                .strip_prefix("Context:\n")
                .unwrap()
                .split("\n\nSession Day body:")
                .next()
                .unwrap()
        };
        self.0.lock().unwrap().push(serde_json::from_str(context)?);
        Ok(if request.kind == CompletionKind::SessionTitle { r#"{"title":"Floor Plan Viewer"}"# } else { r#"{"topic":"Floor Plans","title":"Build viewer","intent":"Do the work","tldr":null,"summary":"Created the floor plan viewer.","classification":["build"]}"# }.into())
    }
}

#[test]
fn loose_and_mapped_workspace_context_is_identical_in_digest_and_render() {
    for (kind, expected) in [
        ("loose = true", serde_json::Value::Null),
        ("", serde_json::json!("Unit 802 Floor Plans")),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        common::copy_tree(&common::source_fixtures_dir("pi").join("store"), &store);
        let archive = Archive::open(&root.path().join("archive")).unwrap();
        common::pull_pi(&store, archive.root());
        let template = root.path().join("template.md");
        fs::write(&template, "{{ body }}").unwrap();
        let config = format!(
            r#"[views]
timezone = "UTC"
unit = "session-day"
template = {:?}
[views.layout]
json = "json/{{ short_id }}-{{ workday }}.json"
[views.project-names]
"/workspace/dev/myapp" = "Unit 802 Floor Plans"
[views.kind.workspace]
match.source = "pi"
{kind}
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "UNUSED"
"#,
            template.to_str().unwrap()
        );
        fs::write(archive.root().join("config.toml"), config).unwrap();
        let client = Client(Mutex::new(vec![]));
        let stats = digest::run_with_client(
            &archive,
            &client,
            &DigestOptions {
                session: None,
                force: false,
                day: None,
                from: None,
                through: None,
                dry_run: false,
                now_ms: 1_788_480_000_000,
            },
        )
        .unwrap();
        assert_eq!(stats.failed, 0);
        assert!(!client.0.lock().unwrap().is_empty());
        for context in client.0.lock().unwrap().iter() {
            assert_eq!(context.get("project"), Some(&expected), "{context}");
        }
        render_all(&archive).unwrap();
        for entry in fs::read_dir(archive.views_dir().join("json")).unwrap() {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            assert_eq!(value["session"]["project"]["name"], expected);
        }
    }
}

#[test]
fn explicit_assignment_overrides_loose_but_not_omission_and_preserves_digests() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("store");
    common::copy_tree(&common::source_fixtures_dir("pi").join("store"), &store);
    let archive = Archive::open(&root.path().join("archive")).unwrap();
    common::pull_pi(&store, archive.root());
    let config = r#"[views]
timezone = "UTC"
[views.kind.scratch]
match.source = "pi"
loose = true
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "UNUSED"
"#;
    fs::write(archive.root().join("config.toml"), config).unwrap();
    let session = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .remove(0);
    let options = DigestOptions {
        session: None,
        force: false,
        day: None,
        from: None,
        through: None,
        dry_run: false,
        now_ms: 1_788_480_000_000,
    };
    digest::run_with_client(&archive, &Client(Mutex::new(vec![])), &options).unwrap();
    let before = common::tree_bytes(&archive.root().join("digests"));
    agent_sessions::assignments::assign(
        &archive,
        "pi",
        &session.native_id,
        "Unit 802 Floor Plans",
        None,
    )
    .unwrap();
    assert_eq!(
        agent_sessions::render::resolved_projects(&archive).unwrap()[&session.id],
        Some("Unit 802 Floor Plans".into())
    );
    render_all(&archive).unwrap();
    assert_eq!(before, common::tree_bytes(&archive.root().join("digests")));
    assert_eq!(
        digest::run_with_client(&archive, &Client(Mutex::new(vec![])), &options)
            .unwrap()
            .requests,
        0
    );
    fs::write(
        archive.root().join("config.toml"),
        config.replace("loose = true", "omit = true"),
    )
    .unwrap();
    assert_eq!(render_all(&archive).unwrap().transcripts, 0);
    fs::write(archive.root().join("config.toml"), config).unwrap();
    agent_sessions::assignments::unassign(&archive, "pi", &session.native_id).unwrap();
    assert_eq!(
        agent_sessions::render::resolved_projects(&archive).unwrap()[&session.id],
        None
    );
}

#[test]
fn subagents_inherit_assigned_project_across_recorded_workspaces() {
    let root = tempfile::tempdir().unwrap();
    let store = common::temp_store();
    let archive = Archive::open(&root.path().join("archive")).unwrap();
    common::pull(store.path(), archive.root());
    let parent = "33333333-3333-4333-8333-333333333333";
    agent_sessions::assignments::assign(&archive, "claude", parent, "Parent Project", None)
        .unwrap();
    let db = archive.db().unwrap();
    let links = db.subagent_links("claude", parent).unwrap();
    assert!(!links.is_empty());
    let projects = agent_sessions::render::resolved_projects(&archive).unwrap();
    for child in links {
        assert_eq!(
            projects[&child.child_session_id],
            Some("Parent Project".into())
        );
    }
}
