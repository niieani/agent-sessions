//! Name Table files: `[views] name-tables` loads generated Name Tables and
//! merges them with inline entries; repository keys may be written as remote
//! URLs; any duplicate key is a hard configuration error.

use agent_sessions::{archive::Archive, fleet::FleetConfig, model::*, render::render_all};
use std::{fs, path::Path};

fn add(archive: &Archive, id: &str, path: &str, url: Option<&str>) {
    let timestamp_ms = 1_786_080_000_000;
    let conversation = Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "testhost".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}.jsonl"),
        },
        title: Some(format!("Session {id}")),
        project_path: Some(path.into()),
        native_project: None,
        created_at_ms: Some(timestamp_ms),
        updated_at_ms: Some(timestamp_ms),
        messages: vec![Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: "hello".into(),
            timestamp_ms: Some(timestamp_ms),
            actions_before: 0,
        }],
        transcript_facts: TranscriptFacts {
            git: url.map(|url| GitSnapshot {
                repository_url: Some(url.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    };
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
}

fn project_dirs(archive: &Archive) -> Vec<String> {
    let mut dirs = fs::read_dir(archive.views_dir().join("projects"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn load_error(root: &Path) -> String {
    format!("{:#}", FleetConfig::load(root).unwrap_err())
}

#[test]
fn name_table_file_names_projects_and_hosts_alongside_inline_entries() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(
        &a,
        "infra",
        "/work/servers",
        Some("git@example.github.com:example-org/infrastructure.git"),
    );
    add(&a, "loose-checkout", "/old/Servers", None);
    add(&a, "inline", "/work/other", None);
    fs::create_dir(d.path().join("generated")).unwrap();
    fs::write(
        d.path().join("generated/vault-names.toml"),
        r#"
[host-names]
testhost = "Test Host"

[project-names]
"https://github.com/example-org/infrastructure" = "Example Infrastructure"
"/old/Servers" = "Example Infrastructure"
"#,
    )
    .unwrap();
    fs::write(
        d.path().join("config.toml"),
        r#"
[views]
name-tables = ["generated/vault-names.toml"]

[views.project-names]
"/work/other" = "Hand Kept"
"#,
    )
    .unwrap();

    let config = FleetConfig::load(d.path()).unwrap().unwrap();
    assert_eq!(config.views.host_names["testhost"], "Test Host");
    assert_eq!(
        config.views.project_names["example-org/infrastructure"],
        "Example Infrastructure"
    );

    render_all(&a).unwrap();
    assert_eq!(project_dirs(&a), ["Example Infrastructure", "Hand Kept"]);
}

#[test]
fn inline_repository_url_keys_are_normalized() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(
        &a,
        "one",
        "/w/example-assistant",
        Some("git@github.com:example-org/assistant.git"),
    );
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'https://github.com/example-org/assistant.git' = 'Example Assistant Service'\n",
    )
    .unwrap();
    render_all(&a).unwrap();
    assert_eq!(project_dirs(&a), ["Example Assistant Service"]);
}

#[test]
fn duplicate_keys_across_inline_and_file_fail_naming_both_origins() {
    let d = tempfile::tempdir().unwrap();
    fs::write(
        d.path().join("names.toml"),
        "[project-names]\n'https://github.com/example-org/assistant' = 'Example Assistant Service'\n",
    )
    .unwrap();
    fs::write(
        d.path().join("config.toml"),
        "[views]\nname-tables = ['names.toml']\n[views.project-names]\n'example-org/assistant' = 'Example Assistant Service'\n",
    )
    .unwrap();
    let error = load_error(d.path());
    assert!(error.contains("example-org/assistant"), "{error}");
    assert!(error.contains("names.toml"), "{error}");
    assert!(error.contains("config.toml"), "{error}");
}

#[test]
fn duplicate_keys_between_two_files_fail() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a.toml"), "[host-names]\nh = 'One'\n").unwrap();
    fs::write(d.path().join("b.toml"), "[host-names]\nh = 'Two'\n").unwrap();
    fs::write(
        d.path().join("config.toml"),
        "[views]\nname-tables = ['a.toml', 'b.toml']\n",
    )
    .unwrap();
    let error = load_error(d.path());
    assert!(
        error.contains("host-names") && error.contains("\"h\""),
        "{error}"
    );
}

#[test]
fn inline_keys_normalizing_to_the_same_repository_fail() {
    let error = FleetConfig::parse(
        "[views.project-names]\n'example-org/assistant' = 'A'\n'git@github.com:example-org/assistant.git' = 'B'\n",
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("example-org/assistant"),
        "{error:#}"
    );
}

#[test]
fn project_key_that_is_neither_path_nor_repository_fails() {
    let error = FleetConfig::parse(
        "[views.project-names]\nexample-assistant = 'Example Assistant Service'\n",
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("example-assistant"),
        "{error:#}"
    );
}

#[test]
fn missing_or_malformed_name_table_files_fail() {
    let d = tempfile::tempdir().unwrap();
    fs::write(
        d.path().join("config.toml"),
        "[views]\nname-tables = ['absent.toml']\n",
    )
    .unwrap();
    assert!(load_error(d.path()).contains("absent.toml"));

    fs::write(d.path().join("absent.toml"), "[kind-names]\nx = 'y'\n").unwrap();
    assert!(load_error(d.path()).contains("absent.toml"));
}

#[test]
fn native_chatgpt_keys_reject_malformed_project_ids() {
    for id in [
        "g-p-short",
        "g-custom",
        "g-p-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "g-p-0123456789abcdef0123456789abcdef/extra",
    ] {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("config.toml"),
            format!("[views.project-names]\n\"chatgpt:{id}\" = \"Name\"\n"),
        )
        .unwrap();
        assert!(load_error(d.path()).contains("invalid ChatGPT Project identity"));
    }
}
