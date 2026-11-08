use std::{fs, path::Path};

use agent_sessions::{archive::Archive, model::*, render::render_all};

const LAYOUT: &str = "[views]\ntimezone = 'UTC'\nunit = 'session-day'\n[views.layout]\nsession = 'out/{{ workday }} {{ title }}.md'\nloose = 'out/{{ workday }} {{ title }}.md'\nsubagent = '{{ parent_dir }}/{{ title }}.md'\njson = 'json/{{ workday }} {{ title }}.json'\n";

/// Sessions `(native id, Locked Title)`, created in the given order.
fn fixture(root: &Path, sessions: &[(&str, &str)]) -> Archive {
    let sessions = sessions
        .iter()
        .map(|(id, title)| (*id, *title, None, None))
        .collect::<Vec<_>>();
    fixture_in_projects(root, &sessions)
}

type Placed<'a> = (&'a str, &'a str, Option<&'a str>, Option<NativeProject>);

/// Sessions `(native id, Locked Title, project path, Native Project)`.
fn fixture_in_projects(root: &Path, sessions: &[Placed<'_>]) -> Archive {
    let archive = Archive::open(root).unwrap();
    for (index, (id, title, project_path, native_project)) in sessions.iter().enumerate() {
        let at = 1_788_170_400_000 + index as i64;
        archive
            .db()
            .unwrap()
            .upsert_conversation(&Conversation {
                provenance: Provenance {
                    source: "chatgpt".into(),
                    host: "test".into(),
                    native_id: (*id).into(),
                    native_path: format!("/store/{id}.json"),
                },
                created_at_ms: Some(at),
                messages: vec![Message {
                    role: Role::User,
                    kind: MessageKind::Dialog,
                    text: "hello".into(),
                    timestamp_ms: Some(at),
                    actions_before: 0,
                }],
                title: None,
                project_path: project_path.map(str::to_owned),
                native_project: native_project.clone(),
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
        let dir = agent_sessions::digest::digest_dir(root, "chatgpt", id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-08-31.json"),
            serde_json::json!({"lockedTitle": title}).to_string(),
        )
        .unwrap();
    }
    fs::write(root.join("config.toml"), LAYOUT).unwrap();
    archive
}

/// The documented `short_id` layout variable: FNV-1a 64 of `source:native-id`
/// folded to 32 bits.
fn short_id(native_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("chatgpt:{native_id}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (hash >> 32) as u32 ^ hash as u32)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn render(sessions: &[(&str, &str)]) -> (Vec<String>, Vec<String>) {
    let root = tempfile::tempdir().unwrap();
    let archive = fixture(root.path(), sessions);
    render_all(&archive).unwrap();
    let views = archive.views_dir();
    (names(&views.join("out")), names(&views.join("json")))
}

#[test]
fn titles_differing_only_by_case_get_identity_suffixes_and_others_keep_their_names() {
    let (markdown, json) = render(&[
        ("upper", "ParseMessage explanation"),
        ("lower", "parseMessage explanation"),
        ("other", "Unrelated title"),
        ("same-1", "Same"),
        ("same-2", "Same"),
    ]);
    let (upper, lower) = (short_id("upper"), short_id("lower"));
    assert_eq!(
        markdown,
        [
            format!("2026-08-31 ParseMessage explanation@{upper}.md"),
            "2026-08-31 Same (2).md".into(),
            "2026-08-31 Same.md".into(),
            "2026-08-31 Unrelated title.md".into(),
            format!("2026-08-31 parseMessage explanation@{lower}.md"),
        ]
    );
    assert_eq!(
        json,
        [
            format!("2026-08-31 ParseMessage explanation@{upper}.json"),
            "2026-08-31 Same (2).json".into(),
            "2026-08-31 Same.json".into(),
            "2026-08-31 Unrelated title.json".into(),
            format!("2026-08-31 parseMessage explanation@{lower}.json"),
        ]
    );
}

#[test]
fn case_variant_names_do_not_depend_on_session_order() {
    let forward = render(&[("upper", "Topic"), ("lower", "topic")]);
    let reverse = render(&[("lower", "topic"), ("upper", "Topic")]);
    assert_eq!(forward, reverse);
}

#[test]
fn exact_duplicates_of_a_case_variant_each_get_their_own_identity_suffix() {
    let (markdown, _) = render(&[("a", "Topic"), ("b", "Topic"), ("c", "topic")]);
    assert_eq!(
        markdown,
        [
            format!("2026-08-31 Topic@{}.md", short_id("a")),
            format!("2026-08-31 Topic@{}.md", short_id("b")),
            format!("2026-08-31 topic@{}.md", short_id("c")),
        ]
    );
}

#[test]
fn layout_project_uses_the_collision_safe_project_name_for_native_projects() {
    let root = tempfile::tempdir().unwrap();
    let native = NativeProject {
        source: "chatgpt".into(),
        native_id: "g-p-679e6c81bfa4819190d87491edb3eb13".into(),
        name: Some("axon notes".into()),
    };
    let archive = fixture_in_projects(
        root.path(),
        &[
            ("named", "Named", Some("/work/axon-notes"), None),
            ("native", "Native", None, Some(native)),
        ],
    );
    fs::write(
        root.path().join("config.toml"),
        "[views]\ntimezone = 'UTC'\nunit = 'session-day'\n[views.project-names]\n'/work/axon-notes' = 'Axon Notes'\n[views.layout]\nsession = 'projects/{{ project }}/{{ workday }} {{ project }} - {{ title }}.md'\nloose = 'loose/{{ title }}.md'\nsubagent = '{{ parent_dir }}/{{ title }}.md'\n",
    )
    .unwrap();
    render_all(&archive).unwrap();
    let projects = archive.views_dir().join("projects");
    let native_dir = names(&projects)
        .into_iter()
        .find(|name| name != "Axon Notes")
        .unwrap();
    assert!(native_dir.starts_with("axon notes@"), "{native_dir}");
    assert_eq!(
        names(&projects.join(&native_dir)),
        [format!("2026-08-31 {native_dir} - Native.md")]
    );
    assert_eq!(
        names(&projects.join("Axon Notes")),
        ["2026-08-31 Axon Notes - Named.md"]
    );
    let transcript = fs::read_to_string(
        projects
            .join(&native_dir)
            .join(format!("2026-08-31 {native_dir} - Native.md")),
    )
    .unwrap();
    assert!(transcript.contains("project: axon notes\n"), "{transcript}");
}
