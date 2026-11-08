mod common;
use agent_sessions::{
    archive::{Archive, rebuild},
    pull::{StoreRoots, pull_localhost},
    render::render_all,
};
use serde_json::json;
use std::fs;

fn roots(store: &std::path::Path) -> StoreRoots {
    let mut roots = common::empty_roots(store);
    roots.claude = store.to_path_buf();
    roots
}

#[test]
fn origin_survives_richer_copy_and_rebuild_without_duplicate_dialog_or_views() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    fs::write(archive.root().join("config.toml"), "[views]\ntimezone='UTC'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='http://127.0.0.1:1/v1'\nmodel='fixture'\napi-key-env='UNUSED'\n").unwrap();
    let store = dir.path().join("store");
    common::copy_tree(&common::fixtures_dir().join("projects"), &store);
    pull_localhost(&archive, &roots(&store), "studio").unwrap();
    let digest_options = agent_sessions::digest::DigestOptions {
        day: None,
        from: None,
        through: None,
        session: None,
        force: false,
        dry_run: true,
        now_ms: agent_sessions::timefmt::parse_iso_ms("2027-01-01T00:00:00Z").unwrap(),
    };
    let digest_before = agent_sessions::digest::run(&archive, &digest_options).unwrap();
    let activity_before =
        agent_sessions::activity::sessions_active_on(&archive, "2026-03-05").unwrap();
    let before = archive.db().unwrap().sessions_with_dialog().unwrap();
    let selected = before.iter().find(|s| !s.native_id.contains('/')).unwrap();
    let lead: agent_sessions::leads::SessionLead = serde_json::from_value(json!({"cursor":"lead-1","source":"claude","sessionId":selected.native_id,"host":"laptop","ledBy":{"kind":"agent","name":"Coordinator"},"recordedAt":"2026-10-09T00:00:00Z"})).unwrap();
    agent_sessions::leads::store(archive.root(), &lead).unwrap();
    archive
        .db()
        .unwrap()
        .replace_session_leads(&agent_sessions::leads::load(archive.root()).unwrap())
        .unwrap();
    let baseline_render = render_all(&archive).unwrap();
    stamp_births(&archive, "studio", Some(0));
    pull_localhost(&archive, &roots(&store), "laptop").unwrap();
    stamp_births(&archive, "laptop", Some(10000));
    // Make the copied revision win, as happens when sync changes its mtime.
    let metas = common::tree_bytes(&archive.captures_dir());
    for (path, bytes) in metas {
        if path.starts_with("laptop/") && path.ends_with(".meta.json") {
            let mut meta: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            meta["sourceMtimeMs"] = json!(meta["sourceMtimeMs"].as_i64().unwrap() + 10000);
            fs::write(archive.captures_dir().join(path), meta.to_string()).unwrap();
        }
    }
    rebuild(&archive).unwrap();
    let after = archive.db().unwrap().sessions_with_dialog().unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(
        before.iter().map(|s| s.message_count).sum::<i64>(),
        after.iter().map(|s| s.message_count).sum::<i64>()
    );
    let digest_after = agent_sessions::digest::run(&archive, &digest_options).unwrap();
    assert!(digest_before.candidates > 0);
    assert_eq!(digest_before.candidates, digest_after.candidates);
    assert_eq!(digest_before.previews, digest_after.previews);
    assert_eq!(
        activity_before.len(),
        agent_sessions::activity::sessions_active_on(&archive, "2026-03-05")
            .unwrap()
            .len()
    );
    let rendered = render_all(&archive).unwrap();
    assert_eq!(baseline_render.transcripts, rendered.transcripts);
    let leads = archive.db().unwrap().session_leads().unwrap();
    assert_eq!(leads.len(), 1);
    assert_eq!(
        leads
            .get(&("claude".into(), selected.native_id.clone()))
            .unwrap(),
        &lead
    );
    let info =
        agent_sessions::info::inspect_session(&archive, &selected.native_id, Some("claude"), None)
            .unwrap();
    assert_eq!(info.capture_host, "laptop");
    let origin = info.host_attribution.unwrap();
    assert_eq!(origin.host, "studio");
    assert_eq!(origin.copy_hosts(&info.available_hosts), vec!["laptop"]);
    let view = common::tree_bytes(&archive.views_dir())
        .into_iter()
        .find(|(_, b)| String::from_utf8_lossy(b).contains(&format!("id: {}", selected.native_id)))
        .unwrap();
    assert!(String::from_utf8_lossy(&view.1).contains("host: studio"));
    let first = common::tree_bytes(&archive.views_dir());
    rebuild(&archive).unwrap();
    render_all(&archive).unwrap();
    assert_eq!(first, common::tree_bytes(&archive.views_dir()));
}

#[test]
fn durable_origin_records_are_protected_from_render_output() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let store = dir.path().join("store");
    common::copy_tree(&common::fixtures_dir().join("projects"), &store);
    pull_localhost(&archive, &roots(&store), "studio").unwrap();
    let before = common::tree_bytes(&archive.root().join("host-attributions"));
    assert!(!before.is_empty());
    fs::write(
        archive.root().join("config.toml"),
        "[views]\ntimezone='UTC'\noutput='host-attributions'\n",
    )
    .unwrap();
    assert!(render_all(&archive).is_err());
    assert_eq!(
        before,
        common::tree_bytes(&archive.root().join("host-attributions"))
    );
}

#[test]
fn upgrading_archive_preserves_existing_host_before_local_or_bundle_copy_arrives() {
    for bundled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let store = dir.path().join("store");
        common::copy_tree(&common::fixtures_dir().join("projects"), &store);
        pull_localhost(&archive, &roots(&store), "studio").unwrap();
        let selected = archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .into_iter()
            .find(|s| !s.native_id.contains('/'))
            .unwrap();
        // Simulate the previous release: no durable origin records or index.
        fs::rename(
            archive.root().join("host-attributions"),
            dir.path().join("old-records"),
        )
        .unwrap();
        archive
            .db()
            .unwrap()
            .replace_host_attributions(&agent_sessions::origins::Attributions::new())
            .unwrap();
        let meta: serde_json::Value = serde_json::from_slice(
            &fs::read(
                archive
                    .captures_dir()
                    .join("studio/claude")
                    .join(format!("{}.meta.json", selected.native_id)),
            )
            .unwrap(),
        )
        .unwrap();
        let native = std::path::Path::new(meta["nativePath"].as_str().unwrap());
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(native)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        if bundled {
            let mut bytes = Vec::new();
            agent_sessions::bundle::export(
                &mut bytes,
                &roots(&store),
                "laptop",
                Some("claude"),
                None,
            )
            .unwrap();
            agent_sessions::bundle::ingest_bundle(&bytes[..], &archive, &mut archive.db().unwrap())
                .unwrap();
        } else {
            pull_localhost(&archive, &roots(&store), "laptop").unwrap();
        }
        let current = archive
            .db()
            .unwrap()
            .session_row(selected.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.winner_host.as_deref(), Some("laptop"));
        let origin = current.host_attribution.unwrap();
        assert_eq!(origin.host, "studio");
        assert_eq!(
            origin.status,
            agent_sessions::origins::AttributionStatus::Ambiguous
        );
    }
}

fn stamp_births(archive: &Archive, host: &str, offset: Option<i64>) {
    let sessions = archive.db().unwrap().sessions_with_dialog().unwrap();
    for (relative, bytes) in common::tree_bytes(&archive.captures_dir().join(host).join("claude")) {
        if !relative.ends_with(".meta.json") {
            continue;
        }
        let mut meta: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.native_id == meta["nativeId"].as_str().unwrap())
            .unwrap();
        meta["nativeBirthTimeMs"] = json!(offset.zip(session.created_at_ms).map(|(o, t)| t + o));
        fs::write(
            archive
                .captures_dir()
                .join(host)
                .join("claude")
                .join(relative),
            meta.to_string(),
        )
        .unwrap();
    }
    agent_sessions::origins::reconcile(archive, &mut archive.db().unwrap()).unwrap();
}

#[test]
fn new_macbook_cli_session_is_attributed_by_birth_in_both_pull_orders_without_t3() {
    for hosts in [["studio", "laptop"], ["laptop", "studio"]] {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let store = dir.path().join("store");
        common::copy_tree(&common::fixtures_dir().join("projects"), &store);
        for host in hosts {
            pull_localhost(&archive, &roots(&store), host).unwrap();
            stamp_births(
                &archive,
                host,
                Some(if host == "laptop" { 0 } else { 10000 }),
            );
        }
        assert!(archive.db().unwrap().origin_claims().unwrap().is_empty());
        for s in archive.db().unwrap().sessions_with_dialog().unwrap() {
            let origin = s.host_attribution.unwrap();
            assert_eq!(origin.host, "laptop");
            assert_eq!(
                origin.status,
                agent_sessions::origins::AttributionStatus::Confirmed
            );
        }
        // Complete birth evidence reproduces origin with no old database or preference records.
        fs::rename(
            archive.root().join("host-attributions"),
            dir.path().join("saved-preferences"),
        )
        .unwrap();
        fs::write(archive.db_path(), b"corrupt database").unwrap();
        rebuild(&archive).unwrap();
        for s in archive.db().unwrap().sessions_with_dialog().unwrap() {
            assert_eq!(s.host_attribution.unwrap().host, "laptop");
        }
    }
}

#[test]
fn tied_or_missing_births_retain_current_host_across_rebuild_and_gap_requires_one_second() {
    for other_birth in [None, Some(0), Some(999), Some(1000)] {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(&dir.path().join("archive")).unwrap();
        let store = dir.path().join("store");
        common::copy_tree(&common::fixtures_dir().join("projects"), &store);
        pull_localhost(&archive, &roots(&store), "studio").unwrap();
        stamp_births(&archive, "studio", Some(0));
        pull_localhost(&archive, &roots(&store), "laptop").unwrap();
        stamp_births(&archive, "laptop", other_birth);
        let before = archive.db().unwrap().sessions_with_dialog().unwrap();
        for session in &before {
            let origin = session.host_attribution.as_ref().unwrap();
            assert_eq!(origin.host, "studio");
            assert_eq!(
                origin.status,
                if other_birth == Some(1000) {
                    agent_sessions::origins::AttributionStatus::Confirmed
                } else {
                    agent_sessions::origins::AttributionStatus::Ambiguous
                }
            );
        }
        fs::write(archive.db_path(), b"corrupt").unwrap();
        rebuild(&archive).unwrap();
        let after = archive.db().unwrap().sessions_with_dialog().unwrap();
        for session in after {
            assert_eq!(
                session.host_attribution,
                before
                    .iter()
                    .find(|s| s.native_id == session.native_id)
                    .unwrap()
                    .host_attribution
            );
        }
    }
}
