#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    archive::{Archive, rebuild},
    assignments,
    render::render_all,
};
use serde_json::{Value, json};
use std::fs;

#[path = "support/chatgpt_projects.rs"]
mod projects;
use projects::*;

#[test]
fn verified_native_project_survives_rebuild_and_uses_standard_project_grouping() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    directory(
        &input,
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Example Person B",
        15,
    );
    detail(
        &input,
        "conversation",
        json!("g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        15,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    assert!(shared::collect(&archive, &input).unwrap().sessions_failed == 0);
    let session = archive
        .db()
        .unwrap()
        .sessions_with_dialog()
        .unwrap()
        .remove(0);
    let project = session.native_project.as_ref().unwrap();
    assert_eq!(
        (
            &*project.source,
            &*project.native_id,
            project.name.as_deref()
        ),
        (
            "chatgpt",
            "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("Example Person B")
        )
    );
    assert_eq!(session.project_path, None);
    let dump = archive.db().unwrap().content_dump().unwrap();
    let rebuilt = rebuild(&archive).unwrap();
    assert_eq!(rebuilt.captures_failed, 0);
    assert_eq!(archive.db().unwrap().content_dump().unwrap(), dump);
    config(&archive, "");
    render_all(&archive).unwrap();
    let context: Value = serde_json::from_slice(
        &fs::read(
            fs::read_dir(archive.views_dir().join("json"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        context["session"]["project"]["name"]
            .as_str()
            .unwrap()
            .starts_with("Example Person B")
    );
    assert_eq!(
        context["session"]["project"]["native"]["native_id"],
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(context["session"]["project"]["path"], "");
}

#[test]
fn latest_observed_membership_and_name_win_and_explicit_null_clears_a_move() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    directory(
        &input,
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Old name",
        14,
    );
    detail(
        &input,
        "conversation",
        json!("g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        14,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    directory(
        &input,
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "New name",
        15,
    );
    let stats = shared::collect(&archive, &input).unwrap();
    assert!(stats.sessions_failed == 0, "{:?}", stats);
    assert_eq!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .native_project
            .as_ref()
            .unwrap()
            .name
            .as_deref(),
        Some("New name")
    );
    detail(&input, "conversation", Value::Null, 16);
    shared::collect(&archive, &input).unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .native_project
            .is_none()
    );
    rebuild(&archive).unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .native_project
            .is_none()
    );
}

#[test]
fn project_listing_proves_membership_but_unknown_custom_gizmo_is_not_a_project() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    detail(&input, "member", Value::Null, 14);
    detail(&input, "custom", json!("g-p-unverified"), 14);
    capture(
        &input,
        "project-list",
        "gizmos/g-p-cccccccccccccccccccccccccccccccc/conversations?limit=5",
        json!({"items":[{"id":"member"}],"cursor":null}),
        15,
    );
    directory(
        &input,
        "g-p-cccccccccccccccccccccccccccccccc",
        "Native Project",
        13,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    let sessions = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert_eq!(
        sessions
            .iter()
            .find(|s| s.native_id == "member")
            .unwrap()
            .native_project
            .as_ref()
            .unwrap()
            .native_id,
        "g-p-cccccccccccccccccccccccccccccccc"
    );
    assert!(
        sessions
            .iter()
            .find(|s| s.native_id == "custom")
            .unwrap()
            .native_project
            .is_none()
    );
}

#[test]
fn explicit_assignment_and_native_name_table_override_native_grouping() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    directory(
        &input,
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Example Person B",
        15,
    );
    detail(
        &input,
        "conversation",
        json!("g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        15,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    config(
        &archive,
        "[views.project-names]\n\"chatgpt:g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"=\"Parenting\"\n",
    );
    render_all(&archive).unwrap();
    assert!(archive.views_dir().join("projects/Parenting").is_dir());
    assignments::assign(&archive, "chatgpt", "conversation", "Explicit", None).unwrap();
    render_all(&archive).unwrap();
    assert!(archive.views_dir().join("projects/Explicit").is_dir());
    assert!(!archive.views_dir().join("projects/Parenting").exists());
}

#[test]
fn identical_listing_bytes_observed_after_a_move_restore_membership_and_preserve_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let body = json!({"items":[{"id":"member"}],"cursor":null});
    capture(
        &input,
        "first",
        "gizmos/g-p-cccccccccccccccccccccccccccccccc/conversations?limit=5",
        body.clone(),
        14,
    );
    detail(&input, "member", Value::Null, 15);
    directory(
        &input,
        "g-p-cccccccccccccccccccccccccccccccc",
        "Native Project",
        13,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .native_project
            .is_none()
    );
    capture(
        &input,
        "last",
        "gizmos/g-p-cccccccccccccccccccccccccccccccc/conversations?limit=5",
        body,
        16,
    );
    shared::collect(&archive, &input).unwrap();
    assert!(!archive.root().join("chatgpt-evidence").exists());
    let mut preserved = Vec::new();
    for entry in fs::read_dir(
        archive
            .captures_dir()
            .join(agent_sessions::pull::local_hostname())
            .join("chatgpt"),
    )
    .unwrap()
    {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "zst") {
            let bytes = zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap();
            for line in String::from_utf8(bytes).unwrap().lines() {
                preserved
                    .push(serde_json::from_str::<agent_sessions::chatgpt::Evidence>(line).unwrap());
            }
        }
    }
    let listing = fs::read_to_string(input.join("first.json")).unwrap();
    assert_eq!(preserved.iter().filter(|r| r.payload == listing).count(), 2);
    fs::rename(&input, temp.path().join("offline-inputs")).unwrap();
    rebuild(&archive).unwrap();
    assert_eq!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .native_project
            .as_ref()
            .unwrap()
            .native_id,
        "g-p-cccccccccccccccccccccccccccccccc"
    );
}

#[test]
fn equal_display_names_have_distinct_safe_directories_and_renames_sweep_old_views() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    for (id, conv) in [
        ("g-p-11111111111111111111111111111111", "one"),
        ("g-p-22222222222222222222222222222222", "two"),
    ] {
        directory(&input, id, "A/B\\C", 14);
        detail(&input, conv, json!(id), 14);
    }
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    config(&archive, "");
    render_all(&archive).unwrap();
    let folders = fs::read_dir(archive.views_dir().join("projects"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(folders.len(), 2);
    assert_ne!(folders[0], folders[1]);
    assert!(folders.iter().all(|name| name.starts_with("A／B＼C@")));
    directory(
        &input,
        "g-p-11111111111111111111111111111111",
        "Renamed",
        15,
    );
    shared::collect(&archive, &input).unwrap();
    render_all(&archive).unwrap();
    let folders = fs::read_dir(archive.views_dir().join("projects"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(folders.len(), 2);
    assert_eq!(folders.iter().filter(|name| *name == "A／B＼C").count(), 1);
    assert_eq!(folders.iter().filter(|name| *name == "Renamed").count(), 1);
}

#[test]
fn native_project_namespace_retains_unknown_names_and_absent_fields_do_not_clear_membership() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let project = "g-p-0123456789abcdef0123456789abcdef";
    detail(&input, "conversation", json!(project), 14);
    capture(
        &input,
        "without-project-field",
        "conversations/conversation?num_turns=10",
        json!({"update_time":1,"messages":[{"id":"u-conversation","author":{"role":"user"},"create_time":1780000000,"content":{"content_type":"text","parts":["Original words"]}}],"page_info":{"has_previous_page":false}}),
        15,
    );
    detail(
        &input,
        "custom",
        json!("g-0123456789abcdef0123456789abcdef"),
        14,
    );
    directory(
        &input,
        "g-p-0123456789abcdef0123456789abcdef",
        "Native Project",
        13,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    let sessions = archive.db().unwrap().sessions_with_dialog().unwrap();
    let known = sessions
        .iter()
        .find(|s| s.native_id == "conversation")
        .unwrap()
        .native_project
        .as_ref()
        .unwrap();
    assert_eq!(known.native_id, project);
    assert_eq!(known.name.as_deref(), Some("Native Project"));
    assert!(
        sessions
            .iter()
            .find(|s| s.native_id == "custom")
            .unwrap()
            .native_project
            .is_none()
    );
}

#[test]
fn bundle_prepares_native_project_context_after_every_capture_has_settled() {
    use agent_sessions::bundle::{
        BUNDLE_SCHEMA_VERSION, BundleManifest, ingest_bundle, write_bundle,
    };
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let native = "g-p-0123456789abcdef0123456789abcdef";
    detail(&input, "first", json!(native), 14);
    detail(&input, "second", json!(native), 14);
    directory(
        &input,
        "g-p-0123456789abcdef0123456789abcdef",
        "Native Project",
        13,
    );
    let source = Archive::open(&temp.path().join("source")).unwrap();
    shared::collect(&source, &input).unwrap();
    let mut captures = ["first", "second"]
        .map(|id| {
            source
                .lookup_capture(id, Some("chatgpt"), None)
                .unwrap()
                .capture
        })
        .into_iter()
        .collect::<Vec<_>>();
    let directory_id = fs::read_dir(
        source
            .captures_dir()
            .join(agent_sessions::pull::local_hostname())
            .join("chatgpt"),
    )
    .unwrap()
    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
    .find(|name| name.starts_with("catalog-") && name.ends_with(".meta.json"))
    .unwrap();
    captures.push(
        source
            .lookup_capture(
                directory_id.trim_end_matches(".meta.json"),
                Some("chatgpt"),
                None,
            )
            .unwrap()
            .capture,
    );
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        host: agent_sessions::pull::local_hostname(),
        sources: vec!["chatgpt".into()],
        since_ms: None,
        unit_count: 3,
        units_failed: 0,
        units_retry_later: 0,
        exported_at_ms: 1,
        retention: Default::default(),
    };
    let mut bytes = Vec::new();
    write_bundle(&mut bytes, &manifest, &captures, &source.captures_dir()).unwrap();
    let target = Archive::open(&temp.path().join("target")).unwrap();
    let result = ingest_bundle(bytes.as_slice(), &target, &mut target.db().unwrap()).unwrap();
    assert_eq!(result.stats.sessions_failed, 0);
    let sessions = target.db().unwrap().sessions_with_dialog().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|s| {
        s.native_project
            .as_ref()
            .is_some_and(|p| p.native_id == native)
    }));
}

#[test]
fn catalog_capture_hash_detects_payload_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    directory(
        &input,
        "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Example Person B",
        15,
    );
    detail(
        &input,
        "conversation",
        json!("g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        15,
    );
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    shared::collect(&archive, &input).unwrap();
    let path = fs::read_dir(
        archive
            .captures_dir()
            .join(agent_sessions::pull::local_hostname())
            .join("chatgpt"),
    )
    .unwrap()
    .map(|e| e.unwrap().path())
    .find(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("catalog-")
            && p.extension().is_some_and(|ext| ext == "zst")
    })
    .unwrap();
    let bytes = zstd::stream::decode_all(fs::File::open(&path).unwrap()).unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["payload"] = json!("{}");
    fs::write(
        path,
        zstd::stream::encode_all(serde_json::to_string(&value).unwrap().as_bytes(), 3).unwrap(),
    )
    .unwrap();
    let error = agent_sessions::chatgpt::status(&archive).unwrap_err();
    assert!(format!("{error:#}").contains("evidence hash mismatch"));
}

#[test]
fn sole_native_name_is_unsuffixed_and_case_or_nfc_collisions_are_hashed() {
    for names in [
        ["condu", "other"],
        ["axon notes", "Axon Notes"],
        ["Café", "Cafe\u{301}"],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input");
        for (id, conv, name) in [
            ("g-p-11111111111111111111111111111111", "one", names[0]),
            ("g-p-22222222222222222222222222222222", "two", names[1]),
        ] {
            directory(&input, id, name, 14);
            detail(&input, conv, json!(id), 14);
        }
        let archive = Archive::open(&temp.path().join("archive")).unwrap();
        shared::collect(&archive, &input).unwrap();
        config(&archive, "");
        render_all(&archive).unwrap();
        let folders = fs::read_dir(archive.views_dir().join("projects"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders.iter().any(|n| n.contains('@')), names[0] != "condu");
        if names[0] == "condu" {
            assert!(archive.views_dir().join("projects/condu").is_dir());
        }
    }
}
