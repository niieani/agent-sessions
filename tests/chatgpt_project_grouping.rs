#[path = "support/chatgpt_projects.rs"]
mod projects;
#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{adapters::Adapter, archive::Archive, chatgpt::ChatGptAdapter, render};
use serde_json::json;
use std::fs;
#[test]
fn native_condu_shares_codex_and_claude_directory_and_generated_name_tables() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let native = "g-p-0123456789abcdef0123456789abcdef";
    projects::directory(&input, native, "condu", 14);
    projects::detail(&input, "native", json!(native), 14);
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    assert_eq!(
        shared::collect(&archive, &input).unwrap().sessions_failed,
        0
    );
    let capture = archive
        .lookup_capture("native", Some("chatgpt"), None)
        .unwrap()
        .capture;
    for source in ["codex", "claude"] {
        let mut conversation = ChatGptAdapter.extract(&capture).unwrap().unwrap();
        conversation.provenance.source = source.into();
        conversation.provenance.native_id = source.into();
        conversation.project_path = Some("/work/condu".into());
        archive
            .db()
            .unwrap()
            .upsert_conversation(&conversation)
            .unwrap();
    }
    render::render_all(&archive).unwrap();
    assert_eq!(
        fs::read_dir(archive.views_dir().join("projects"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        shared::transcripts(&archive.views_dir().join("projects/condu")).len(),
        3
    );
    fs::write(archive.root().join("names.toml"),format!("[project-names]\n\"chatgpt:{native}\"=\"Vault Project\"\n\"/work/condu\"=\"Vault Project\"\n")).unwrap();
    fs::write(
        archive.root().join("config.toml"),
        "[views]\nname-tables=[\"names.toml\"]\n",
    )
    .unwrap();
    render::render_all(&archive).unwrap();
    assert_eq!(
        shared::transcripts(&archive.views_dir().join("projects/Vault Project")).len(),
        3
    );
    assert!(!archive.views_dir().join("projects/condu").exists());
}
