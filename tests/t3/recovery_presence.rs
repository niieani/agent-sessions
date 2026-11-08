use super::*;

#[test]
fn captured_provider_blocks_recovery_even_when_rebuild_cannot_extract_it() {
    let stores = tempfile::tempdir().unwrap();
    let writer = fixture(stores.path());
    let _writer = writer;
    let claude = stores.path().join("claude/-work");
    std::fs::create_dir_all(&claude).unwrap();
    let provider_id = "33333333-3333-4333-8333-333333333333";
    std::fs::write(claude.join(format!("{provider_id}.jsonl")),concat!(
        r#"{"type":"user","cwd":"/work","timestamp":"2026-08-28T10:00:00Z","message":{"role":"user","content":"actual provider Dialog"}}"#,"\n"
    )).unwrap();
    let mut roots = empty_roots(stores.path());
    roots.claude = stores.path().join("claude");
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    pull_localhost(&archive, &roots, "host").unwrap();
    let provider = archive
        .lookup_capture(provider_id, Some("claude"), None)
        .unwrap();
    std::fs::write(&provider.capture.body_path, b"invalid zstd capture body").unwrap();
    let stats = rebuild(&archive).unwrap();
    assert_eq!(stats.captures_failed, 1);
    let db = archive.db().unwrap();
    assert!(db.orchestrator_thread(SOURCE, THREAD).unwrap().is_some());
    assert!(
        db.sessions_with_dialog().unwrap().is_empty(),
        "an existing broken Capture must not turn into a recovered Session"
    );
    assert!(
        db.lookup_session_by_orchestrator_id(SOURCE, THREAD)
            .unwrap()
            .is_none()
    );
}

#[test]
fn captured_provider_presence_survives_empty_local_and_broken_bundle_ingest() {
    let stores = tempfile::tempdir().unwrap();
    let writer = fixture(stores.path());
    let _writer = writer;
    let claude = stores.path().join("claude/-work");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join("33333333-3333-4333-8333-333333333333.jsonl"),
        [0xff, 0xfe, b'\n'],
    )
    .unwrap();
    let mut roots = empty_roots(stores.path());
    roots.claude = stores.path().join("claude");
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;
    let direct_dir = tempfile::tempdir().unwrap();
    let direct = Archive::open(direct_dir.path()).unwrap();
    let pulled = pull_localhost(&direct, &roots, "host").unwrap();
    assert_eq!(
        pulled
            .sources
            .iter()
            .find(|s| s.source == "claude")
            .unwrap()
            .sessions_failed,
        0
    );
    assert!(
        direct
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .is_empty()
    );
    let mut t3_bundle = Vec::new();
    export(&mut t3_bundle, &roots, "host", Some("t3"), None).unwrap();
    let mut claude_bundle = Vec::new();
    export(&mut claude_bundle, &roots, "host", Some("claude"), None).unwrap();
    // Corrupt only the copied Bundle body; its valid sidecar still proves presence.
    let mut rewritten = tar::Builder::new(Vec::new());
    for entry in tar::Archive::new(claude_bundle.as_slice())
        .entries()
        .unwrap()
    {
        use std::io::Read as _;
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if path.extension().is_some_and(|extension| extension == "zst") {
            bytes = b"invalid zstd capture body".to_vec();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        rewritten
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    let claude_bundle = rewritten.into_inner().unwrap();
    for ordered in [
        [t3_bundle.as_slice(), claude_bundle.as_slice()],
        [claude_bundle.as_slice(), t3_bundle.as_slice()],
    ] {
        let bundled_dir = tempfile::tempdir().unwrap();
        let bundled = Archive::open(bundled_dir.path()).unwrap();
        let mut db = bundled.db().unwrap();
        for bytes in ordered {
            ingest_bundle(bytes, &bundled, &mut db).unwrap();
        }
        assert!(db.sessions_with_dialog().unwrap().is_empty());
        assert!(
            db.lookup_session_by_orchestrator_id(SOURCE, THREAD)
                .unwrap()
                .is_none()
        );
        drop(db);
        assert_eq!(rebuild(&bundled).unwrap().captures_failed, 1);
        assert!(
            bundled
                .db()
                .unwrap()
                .sessions_with_dialog()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn failed_local_extract_retracts_an_already_recovered_session() {
    let stores = tempfile::tempdir().unwrap();
    let writer = fixture(stores.path());
    let _writer = writer;
    let mut roots = empty_roots(stores.path());
    roots.claude = stores.path().join("claude");
    roots.t3 = stores.path().to_path_buf();
    roots.t3_explicit = true;
    let archive_dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(archive_dir.path()).unwrap();
    pull_localhost(&archive, &roots, "host").unwrap();
    assert!(
        archive.db().unwrap().sessions_with_dialog().unwrap()[0]
            .transcript_facts
            .recovered_via
            .is_some()
    );
    let claude = roots.claude.join("-work");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("33333333-3333-4333-8333-333333333333.jsonl"),concat!(r#"{"type":"user","cwd":"/work","timestamp":"2026-08-28T10:00:00Z","message":{"role":"user","content":"provider Dialog"}}"#,"\n")).unwrap();
    let adapter = agent_sessions::adapters::claude::ClaudeAdapter;
    let refs = adapter
        .enumerate(&roots.claude, &EnumerateGate::default())
        .unwrap();
    let mut capture = adapter
        .capture(&refs[0], &archive.captures_dir(), "host")
        .unwrap();
    std::fs::write(&capture.body_path, b"invalid zstd body").unwrap();
    capture.set_needs_retry(true).unwrap();
    let pulled = pull_localhost(&archive, &roots, "host").unwrap();
    assert_eq!(
        pulled
            .sources
            .iter()
            .find(|s| s.source == "claude")
            .unwrap()
            .sessions_failed,
        1
    );
    assert!(
        archive
            .db()
            .unwrap()
            .sessions_with_dialog()
            .unwrap()
            .is_empty()
    );
}
