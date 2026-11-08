use agent_sessions::{archive::Archive, model::*, render::render_all};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn conversation(id: &str, project_path: Option<&str>, timestamp_ms: i64) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "testhost".into(),
            native_id: id.into(),
            native_path: format!("/store/{id}.jsonl"),
        },
        title: Some(format!("Session {id}")),
        project_path: project_path.map(str::to_owned),
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
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

fn add(archive: &Archive, id: &str, path: Option<&str>, url: Option<&str>) {
    let mut c = conversation(id, path, 1_786_080_000_000);
    c.transcript_facts.git = url.map(|url| GitSnapshot {
        repository_url: Some(url.into()),
        ..Default::default()
    });
    archive.db().unwrap().upsert_conversation(&c).unwrap();
}
fn notes(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "md") && p.file_name().unwrap() != "INDEX.md"
        })
        .collect()
}
#[test]
fn worktrees_share_repository_project_path_names_win_and_unaffected_bytes_stay_identical() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "stable", Some("/work/stable"), None);
    add(&a, "one", Some("/worktrees/one/example-assistant"), None);
    add(&a, "two", Some("/worktrees/two/example-assistant"), None);
    render_all(&a).unwrap();
    let stable = notes(&a.views_dir().join("projects/stable")).remove(0);
    let before = fs::read(&stable).unwrap();
    add(
        &a,
        "one",
        Some("/worktrees/one/example-assistant"),
        Some("git@example.github.com:example-org/assistant.git"),
    );
    add(
        &a,
        "two",
        Some("/worktrees/two/example-assistant"),
        Some("https://github.com/example-org/assistant.git/"),
    );
    add(
        &a,
        "three",
        Some("/work/checkout"),
        Some("ssh://git@github.com/example-org/assistant"),
    );
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'example-org/assistant'='Example Assistant Service'\n'/work/checkout'='Exact Path'\n",
    )
    .unwrap();
    let r = render_all(&a).unwrap();
    assert!(r.removed > 0);
    assert_eq!(
        notes(&a.views_dir().join("projects/Example Assistant Service")).len(),
        2
    );
    assert_eq!(notes(&a.views_dir().join("projects/Exact Path")).len(), 1);
    assert_eq!(fs::read(stable).unwrap(), before);
    assert!(
        fs::read_dir(a.views_dir().join("projects"))
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().contains('@'))
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["render", "--archive"])
        .arg(d.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("0 changed"), "{stdout}");
}
#[test]
fn distinct_repositories_with_same_repo_segment_use_key_hash() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(
        &a,
        "one",
        Some("/w/one"),
        Some("git@github.com:first/shared.git"),
    );
    add(
        &a,
        "two",
        Some("/w/two"),
        Some("https://gitlab.com/group/second/shared.git"),
    );
    render_all(&a).unwrap();
    let mut names = fs::read_dir(a.views_dir().join("projects"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names.len(), 2);
    assert!(
        names
            .iter()
            .all(|n| n.starts_with("shared@") && n.len() == 15)
    );
    assert_ne!(names[0], names[1]);
    add(
        &a,
        "one",
        Some("/different/worktree"),
        Some("ssh://git@github.com/first/shared.git"),
    );
    render_all(&a).unwrap();
    let mut after = fs::read_dir(a.views_dir().join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    after.sort();
    assert_eq!(
        after, names,
        "repository collision suffix must not depend on recorded path"
    );
}
#[test]
fn repository_without_recorded_path_can_have_own_project_and_kinds_still_use_paths() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(
        &a,
        "no-path",
        None,
        Some("https://github.com/owner/repo.git"),
    );
    add(
        &a,
        "omitted",
        Some("/omit/actual"),
        Some("https://github.com/owner/repo.git"),
    );
    fs::write(d.path().join("config.toml"),"[views.kind.hidden]\nmatch.path.is='/omit/actual'\nomit=true\n[views.kind.rest]\nmatch.source='codex'\noutput='./notes'\n").unwrap();
    assert_eq!(render_all(&a).unwrap().transcripts, 1);
    assert_eq!(notes(&a.views_dir().join("projects/repo/notes")).len(), 1);
}

#[test]
fn assignment_wins_over_repository_and_path_name_tables() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(
        &a,
        "assigned",
        Some("/work/checkout"),
        Some("git@github.com:owner/repo.git"),
    );
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'owner/repo'='Repository Name'\n'/work/checkout'='Path Name'\n",
    )
    .unwrap();
    agent_sessions::assignments::assign(&a, "codex", "assigned", "Explicit", None).unwrap();
    render_all(&a).unwrap();
    assert_eq!(notes(&a.views_dir().join("projects/Explicit")).len(), 1);
    assert!(!a.views_dir().join("projects/Path Name").exists());
}

#[test]
fn partial_repository_metadata_does_not_rename_unchanged_path_projects() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "path-only", Some("/work/repo"), None);
    render_all(&a).unwrap();
    let path = notes(&a.views_dir().join("projects/repo")).remove(0);
    let before = fs::read(&path).unwrap();
    add(
        &a,
        "repository",
        Some("/work/repo"),
        Some("git@github.com:owner/repo.git"),
    );
    render_all(&a).unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(notes(&a.views_dir().join("projects/repo")).len(), 2);
}

fn project_dirs(archive: &Archive) -> Vec<String> {
    let mut names = fs::read_dir(archive.views_dir().join("projects"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn case_insensitive_fs(dir: &Path) -> bool {
    let probe = dir.join("case-probe");
    fs::write(&probe, "").unwrap();
    let insensitive = dir.join("CASE-PROBE").exists();
    fs::remove_file(probe).unwrap();
    insensitive
}

#[test]
fn path_only_project_adopts_repository_spelling_ignoring_case() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "path-only", Some("/workspace/Infrastructure"), None);
    add(
        &a,
        "repository",
        Some("/workspace/infrastructure"),
        Some("git@github.com:example-org/infrastructure.git"),
    );
    render_all(&a).unwrap();
    assert_eq!(project_dirs(&a), ["infrastructure"]);
    assert_eq!(
        notes(&a.views_dir().join("projects/infrastructure")).len(),
        2
    );
    let again = render_all(&a).unwrap();
    assert_eq!(again.removed, 0);
    assert_eq!(
        notes(&a.views_dir().join("projects/infrastructure")).len(),
        2
    );
}

#[test]
fn derived_names_differing_only_by_case_without_repository_get_hash_suffixes() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "upper", Some("/a/Foo"), None);
    add(&a, "lower", Some("/b/foo"), None);
    add(&a, "derived", Some("/c/vault"), None);
    add(&a, "explicit", Some("/d/notes"), None);
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'/d/notes'='Vault'\n",
    )
    .unwrap();
    render_all(&a).unwrap();
    let dirs = project_dirs(&a);
    assert_eq!(dirs.len(), 4, "{dirs:?}");
    assert!(dirs.contains(&"Vault".to_string()), "{dirs:?}");
    assert_eq!(
        dirs.iter().filter(|n| n.contains('@')).count(),
        3,
        "{dirs:?}"
    );
    for dir in dirs {
        assert_eq!(notes(&a.views_dir().join("projects").join(dir)).len(), 1);
    }
}

#[test]
fn explicit_project_names_differing_only_by_case_fail_before_mutation() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "one", Some("/a/x"), None);
    add(&a, "two", Some("/b/y"), None);
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'/a/x'='Alpha'\n'/b/y'='alpha'\n",
    )
    .unwrap();
    let error = render_all(&a).unwrap_err().to_string();
    assert!(error.contains("differ only by case"), "{error}");
    assert!(!a.views_dir().join("projects").exists());
}

#[test]
fn case_variant_of_a_live_view_directory_is_not_swept() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "one", Some("/work/proj"), None);
    render_all(&a).unwrap();
    let projects = a.views_dir().join("projects");
    // Simulate a directory created under an older spelling of the name.
    fs::rename(projects.join("proj"), projects.join("tmp")).unwrap();
    fs::rename(projects.join("tmp"), projects.join("Proj")).unwrap();
    let stats = render_all(&a).unwrap();
    let dirs = project_dirs(&a);
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(notes(&projects.join(&dirs[0])).len(), 1);
    if case_insensitive_fs(d.path()) {
        assert_eq!(stats.removed, 0);
    } else {
        assert_eq!(dirs, ["proj"]);
    }
}

#[test]
fn derived_name_matching_an_assignment_only_by_case_gets_hash_suffix() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "assigned", Some("/a/elsewhere"), None);
    add(&a, "derived", Some("/b/servers"), None);
    agent_sessions::assignments::assign(&a, "codex", "assigned", "Servers", None).unwrap();
    render_all(&a).unwrap();
    let dirs = project_dirs(&a);
    assert_eq!(dirs.len(), 2, "{dirs:?}");
    assert!(dirs.contains(&"Servers".to_string()), "{dirs:?}");
    assert!(dirs.iter().any(|n| n.starts_with("servers@")), "{dirs:?}");
}

#[test]
fn project_renamed_by_case_only_respells_existing_views() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "one", Some("/work/budgeting"), None);
    let layout = "[views.layout]\nsession = 'projects/{{ project }}/{{ project }} notes.md'\n";
    fs::write(d.path().join("config.toml"), layout).unwrap();
    render_all(&a).unwrap();
    fs::write(
        d.path().join("config.toml"),
        format!("{layout}[views.project-names]\n'/work/budgeting' = 'Budgeting'\n"),
    )
    .unwrap();
    let stats = render_all(&a).unwrap();
    assert_eq!(project_dirs(&a), ["Budgeting"]);
    let files = fs::read_dir(a.views_dir().join("projects/Budgeting"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(files, ["Budgeting notes.md"]);
    if case_insensitive_fs(d.path()) {
        assert_eq!(stats.removed, 0);
    }
}

#[test]
fn explicitly_named_identities_do_not_force_hash_suffixes_on_raw_names() {
    let d = tempfile::tempdir().unwrap();
    let a = Archive::open(d.path()).unwrap();
    add(&a, "named", Some("/a/nara-viz"), None);
    add(&a, "raw", Some("/b/nara-viz"), None);
    fs::write(
        d.path().join("config.toml"),
        "[views.project-names]\n'/a/nara-viz' = 'Nara Baby Charting'\n",
    )
    .unwrap();
    render_all(&a).unwrap();
    assert_eq!(project_dirs(&a), ["Nara Baby Charting", "nara-viz"]);
}
