use std::{fs, path::Path};

use agent_sessions::{archive::Archive, model::*, render::render_all};

fn fixture(root: &Path, titles: &[&str]) -> Archive {
    let archive = Archive::open(root).unwrap();
    for (index, title) in titles.iter().enumerate() {
        let id = format!("unicode-{index}");
        archive
            .db()
            .unwrap()
            .upsert_conversation(&Conversation {
                provenance: Provenance {
                    source: "codex".into(),
                    host: "test".into(),
                    native_id: id.clone(),
                    native_path: format!("/store/{id}.jsonl"),
                },
                created_at_ms: Some(1_788_170_400_000 + index as i64),
                messages: vec![Message {
                    role: Role::User,
                    kind: MessageKind::Dialog,
                    text: "hello".into(),
                    timestamp_ms: Some(1_788_170_400_000 + index as i64),
                    actions_before: 0,
                }],
                title: None,
                project_path: None,
                native_project: None,
                updated_at_ms: None,
                transcript_facts: Default::default(),
                subagent_parent: None,
                inherited_context: None,
                subagent_references: vec![],
                branches: Vec::new(),
                subagent_links: vec![],
                trailing_actions: 0,
            })
            .unwrap();
        let dir = agent_sessions::digest::digest_dir(root, "codex", &id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-08-31.json"),
            serde_json::json!({"lockedTitle": title}).to_string(),
        )
        .unwrap();
    }
    fs::write(root.join("config.toml"), "[views]\ntimezone = 'UTC'\nunit = 'session-day'\n[views.layout]\nsession = 'Café/{{ title }}.md'\nloose = 'Café/{{ title }}.md'\nsubagent = '{{ parent_dir }}/{{ title }}.md'\nproject-index = 'Café/INDEX.md'\nloose-index = 'Café/INDEX.md'\njson = 'Café/{{ title }}.json'\n").unwrap();
    archive
}

#[test]
fn all_layouts_and_collision_assignment_use_nfc() {
    let root = tempfile::tempdir().unwrap();
    let archive = fixture(root.path(), &["Cafe\u{301}", "Café"]);
    render_all(&archive).unwrap();
    let views = archive.views_dir();
    let dir = fs::read_dir(&views).unwrap().next().unwrap().unwrap();
    assert_eq!(dir.file_name(), "Café");
    for name in [
        "Café.md",
        "Café (2).md",
        "Café.json",
        "Café (2).json",
        "INDEX.md",
    ] {
        assert!(views.join("Café").join(name).is_file(), "{name}");
    }
    let index = fs::read_to_string(views.join("Café/INDEX.md")).unwrap();
    assert!(index.contains("Café.md"));
    assert!(!index.contains("Cafe\u{301}.md"));
}

#[test]
fn existing_nfd_directories_and_files_are_renamed_without_rewriting() {
    let root = tempfile::tempdir().unwrap();
    let archive = fixture(root.path(), &["Café"]);
    let config_path = root.path().join("config.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("Cafe\u{301}", "Café");
    fs::write(config_path, config).unwrap();
    render_all(&archive).unwrap();
    let views = archive.views_dir();
    let canonical = views.join("Café/Café.md");
    let metadata = fs::metadata(&canonical).unwrap();
    let content = fs::read(&canonical).unwrap();
    fs::rename(&canonical, views.join("Café/Cafe\u{301}.md")).unwrap();
    fs::rename(views.join("Café"), views.join("Cafe\u{301}")).unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "core.precomposeunicode", "false"]);
    git(root.path(), &["config", "core.quotepath", "false"]);
    git(root.path(), &["add", "views"]);
    let before = git(root.path(), &["write-tree"]);
    let stats = render_all(&archive).unwrap();
    // Snapshot directory entries afresh: APFS can resolve old index spellings
    // after a rename, so `add -A` alone can retain those old index entries.
    git(root.path(), &["read-tree", "--empty"]);
    git(root.path(), &["add", "-A", "views"]);
    let changes = git(
        root.path(),
        &["diff", "--cached", "--name-status", "-M", before.trim()],
    );
    assert_eq!(changes.lines().count(), 3, "{changes}");
    assert!(
        changes.lines().all(|line| line.starts_with("R100\t")),
        "{changes}"
    );
    assert_eq!(stats.removed, 0);
    assert_eq!(stats.changed, 0);
    assert_eq!(fs::read(&canonical).unwrap(), content);
    assert_eq!(
        fs::metadata(&canonical).unwrap().modified().unwrap(),
        metadata.modified().unwrap()
    );
    assert_eq!(
        fs::read_dir(&views)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name(),
        "Café"
    );
    let names = fs::read_dir(views.join("Café"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"Café.md".into()));
    assert_eq!(render_all(&archive).unwrap().changed, 0);
}

#[test]
fn ambiguous_normalized_entries_fail_before_any_view_mutation() {
    let root = tempfile::tempdir().unwrap();
    let archive = fixture(root.path(), &["Café"]);
    let config_path = root.path().join("config.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("Cafe\u{301}", "Café");
    fs::write(config_path, config).unwrap();
    render_all(&archive).unwrap();
    let views = archive.views_dir();
    let canonical = views.join("Café/Café.md");
    let nfd = views.join("Café/Cafe\u{301}.md");
    fs::write(&nfd, "conflicting note").unwrap();
    // Exercise ambiguity only where the filesystem can represent both entries.
    if fs::read_dir(views.join("Café")).unwrap().count() == 3 {
        return;
    }
    let original = fs::read(&canonical).unwrap();
    let sentinel = views.join("orphan.md");
    fs::write(&sentinel, "keep until validated").unwrap();
    let error = render_all(&archive).unwrap_err().to_string();
    assert!(error.contains("Unicode normalization"), "{error}");
    assert_eq!(fs::read(&canonical).unwrap(), original);
    assert_eq!(fs::read_to_string(&nfd).unwrap(), "conflicting note");
    assert!(sentinel.is_file());
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ambiguous_normalized_directories_fail_without_merging_notes() {
    let root = tempfile::tempdir().unwrap();
    let archive = fixture(root.path(), &["Café"]);
    let config_path = root.path().join("config.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("Cafe\u{301}", "Café");
    fs::write(config_path, config).unwrap();
    render_all(&archive).unwrap();
    let views = archive.views_dir();
    let nfd = views.join("Cafe\u{301}");
    match fs::create_dir(&nfd) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        result => result.unwrap(),
    }
    let conflicting = nfd.join("keep.md");
    fs::write(&conflicting, "distinct directory").unwrap();
    let canonical = views.join("Café/Café.md");
    let original = fs::read(&canonical).unwrap();
    let error = render_all(&archive).unwrap_err().to_string();
    assert!(error.contains("Unicode normalization"), "{error}");
    assert_eq!(fs::read(&canonical).unwrap(), original);
    assert_eq!(
        fs::read_to_string(&conflicting).unwrap(),
        "distinct directory"
    );
}
