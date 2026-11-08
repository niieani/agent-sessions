mod common;
use agent_sessions::{
    archive::Archive,
    model::{Conversation, Message, Provenance, Role},
    render::render_all,
};
use serde_json::Value;
use std::fs;
fn add(archive: &Archive, id: &str, text: &str) {
    archive
        .db()
        .unwrap()
        .upsert_conversation(&Conversation {
            provenance: Provenance {
                source: "codex".into(),
                host: "host".into(),
                native_id: id.into(),
                native_path: "fixture".into(),
            },
            title: None,
            project_path: None,
            native_project: None,
            created_at_ms: Some(1770000000000),
            updated_at_ms: Some(1770000000000),
            messages: vec![Message {
                role: Role::User,
                kind: Default::default(),
                text: text.into(),
                timestamp_ms: Some(1770000000000),
                actions_before: 0,
            }],
            branches: vec![],
            transcript_facts: Default::default(),
            subagent_parent: None,
            inherited_context: None,
            subagent_references: vec![],
            subagent_links: vec![],
            trailing_actions: 0,
        })
        .unwrap();
}
fn config(archive: &Archive, template: &str) {
    fs::write(archive.root().join("template.j2"), template).unwrap();
    fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views]\nunit='session-day'\ntimezone='UTC'\ntemplate={:?}\n",
            archive.root().join("template.j2").to_str().unwrap()
        ),
    )
    .unwrap();
}
#[test]
fn json_reports_form_changes_separately_from_dialog_content_in_a_mixed_run() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(temp.path()).unwrap();
    add(&archive, "one", "first");
    add(&archive, "two", "second");
    config(&archive, "{{ body }}");
    render_all(&archive).unwrap();
    config(&archive, "<!-- new template -->\n{{ body }}");
    add(&archive, "two", "changed request");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["render", "--json", "--archive"])
        .arg(archive.root())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stats: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stats["format"], "asessions-render-v1");
    let changes = stats["files"]
        .as_array()
        .expect("per-file changes required");
    assert!(changes.iter().any(|f| f["form_only"] == true));
    assert!(changes.iter().any(|f| f["form_only"] == false));
    assert!(changes.iter().all(|f| f["path"].is_string()));
}
#[test]
fn layout_renames_are_form_changes() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(temp.path()).unwrap();
    add(&archive, "one", "first");
    config(&archive, "{{ body }}");
    render_all(&archive).unwrap();
    fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views]\nunit='session-day'\ntimezone='UTC'\ntemplate={:?}\n[views.layout]\nloose='renamed/{{{{ short_id }}}}.md'\n",
            archive.root().join("template.j2").to_str().unwrap()
        ),
    )
    .unwrap();
    let stats = serde_json::to_value(render_all(&archive).unwrap()).unwrap();
    assert!(
        stats["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["form_only"] == true)
    );
}

#[test]
fn digest_updates_and_external_view_edits_are_content_changes() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(temp.path()).unwrap();
    add(&archive, "one", "first");
    config(
        &archive,
        "{% if day.digest %}{{ day.digest.summary }}{% endif %}\n{{ body }}",
    );
    let dir = agent_sessions::digest::digest_dir(archive.root(), "codex", "one").unwrap();
    fs::create_dir_all(&dir).unwrap();
    let mut digest = serde_json::json!({"schemaVersion":1,"source":"codex","nativeId":"one","workday":"2026-02-02","covered":{"firstIdx":0,"lastIdx":0},"topic":"Topic","title":"Day","lockedTitle":"Day","tldr":null,"summary":"First summary","classification":["research"],"truncated":false,"model":"fixture","promptVersion":1,"digestedAtMs":0});
    fs::write(dir.join("2026-02-02.json"), digest.to_string()).unwrap();
    render_all(&archive).unwrap();
    digest["summary"] = serde_json::json!("Changed summary");
    fs::write(dir.join("2026-02-02.json"), digest.to_string()).unwrap();
    let result = render_all(&archive).unwrap();
    assert!(result.files.iter().any(|f| !f.form_only));
    let note = common::tree_bytes(&archive.views_dir())
        .into_iter()
        .find(|(p, _)| p.ends_with(".md") && !p.ends_with("INDEX.md"))
        .unwrap()
        .0;
    fs::write(archive.views_dir().join(note), "external change").unwrap();
    assert!(
        render_all(&archive)
            .unwrap()
            .files
            .iter()
            .any(|f| !f.form_only)
    );
}
