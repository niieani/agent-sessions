#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{archive::Archive, render::render_all};
use serde_json::json;
use sha2::{Digest, Sha256};
#[test]
fn media_is_archived_once_and_materialized_separately_without_rewriting_views() {
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir_all(store.join("assets")).unwrap();
    let bytes = b"\x89PNG\r\n\x1a\nfixture";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = format!("assets/{hash}.png");
    std::fs::write(store.join(&path), bytes).unwrap();
    std::fs::write(store.join("assets/image.meta.json"),json!({"mime":"image/png","conversationId":"conv","capturedAt":"2026-10-19T00:00:00Z","format":"chatgpt-generated-image-v1","assetId":"image","conversationId":"c","mime":"image/png","sha256":hash,"bytes":bytes.len(),"path":path,"quality":"original","url":"https://chatgpt.com/backend-api/estuary/content?id=image&sig=PRIVATE&se=PRIVATE&foo=keep"}).to_string()).unwrap();
    let payload=json!({"update_time":1,"messages":[{"id":"u","author":{"role":"user"},"create_time":1770000000,"content":{"parts":["draw"]}},{"id":"i","author":{"role":"tool"},"content":{"parts":[{"asset_pointer":"sediment://image","metadata":{"generation":{}}}]}}]}).to_string();
    std::fs::write(store.join("c.json"), &payload).unwrap();
    std::fs::write(store.join("c.meta.json"),json!({"accountId":"one","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
    assert_eq!(
        shared::collect(&archive, &store).unwrap().sessions_failed,
        0
    );
    render_all(&archive).unwrap();
    let before = shared::text(&archive);
    assert!(before.contains(&format!("media/sha256/{hash}.png")));
    assert!(!archive.views_dir().join("media").exists());
    assert!(
        archive
            .root()
            .join(format!("media/objects/{hash}"))
            .is_file()
    );
    assert!(!archive.root().join("chatgpt-media").exists());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["materialize", "--archive"])
        .arg(archive.root())
        .args(["--method", "copy"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(archive.views_dir().join(format!("media/sha256/{hash}.png"))).unwrap(),
        bytes
    );
    render_all(&archive).unwrap();
    assert_eq!(before, shared::text(&archive));
    assert!(
        archive
            .views_dir()
            .join(format!("media/sha256/{hash}.png"))
            .is_file()
    );
    fn metadata(root: &std::path::Path) -> String {
        std::fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    metadata(&p)
                } else if p.extension().is_some_and(|e| e == "json") {
                    std::fs::read_to_string(p).unwrap()
                } else {
                    String::new()
                }
            })
            .collect()
    }
    let meta = metadata(&archive.root().join("media"));
    assert!(!meta.contains("PRIVATE"));
    assert!(meta.contains("foo=keep"));
}

#[test]
fn embedded_claude_and_codex_images_share_the_same_archive_object() {
    use agent_sessions::pull::{StoreRoots, pull_localhost};
    use base64::Engine;
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let bytes = b"\x89PNG\r\n\x1a\nshared";
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let hash = format!("{:x}", Sha256::digest(bytes));
    let claude = temp.path().join("claude/project");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("11111111-1111-4111-8111-111111111111.jsonl"),json!({"type":"user","sessionId":"11111111-1111-4111-8111-111111111111","timestamp":"2026-02-02T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Claude image"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":encoded}}]}}).to_string()+"\n").unwrap();
    let codex = temp.path().join("codex/sessions/2026/02/02");
    std::fs::create_dir_all(&codex).unwrap();
    let meta = json!({"type":"session_meta","timestamp":"2026-02-02T00:00:00Z","payload":{"id":"22222222-2222-4222-8222-222222222222","cwd":"/work/project","source":"cli"}});
    let message = json!({"type":"response_item","timestamp":"2026-02-02T00:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Codex image"},{"type":"input_image","image_url":format!("data:image/png;base64,{encoded}")}]}});
    std::fs::write(
        codex.join("rollout-2026-02-02T00-00-00-22222222-2222-4222-8222-222222222222.jsonl"),
        format!("{meta}\n{message}\n"),
    )
    .unwrap();
    let empty = temp.path().join("empty");
    let roots = StoreRoots {
        claude: temp.path().join("claude"),
        codex: temp.path().join("codex"),
        pi: empty.clone(),
        cursor: empty.clone(),
        t3: empty.clone(),
        chatgpt: empty,
        t3_explicit: false,
    };
    let result = pull_localhost(&archive, &roots, "machine").unwrap();
    assert_eq!(result.sessions_failed, 0);
    render_all(&archive).unwrap();
    let text = shared::text(&archive);
    assert_eq!(text.matches(&format!("media/sha256/{hash}.png")).count(), 2);
    assert!(
        archive
            .root()
            .join(format!("media/objects/{hash}"))
            .is_file()
    );
}

/// Claude stores skill expansions as `isMeta` user records, which are not
/// Dialog; an image inside one must not break presentation of the Session's
/// Dialog images.
#[test]
fn image_in_non_dialog_claude_record_does_not_fail_render_preparation() {
    use agent_sessions::pull::{StoreRoots, pull_localhost};
    use base64::Engine;
    let temp = tempfile::tempdir().unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let dialog = b"\x89PNG\r\n\x1a\ndialog";
    let meta = b"\x89PNG\r\n\x1a\nmeta";
    let image = |bytes: &[u8]| json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}});
    let id = "33333333-3333-4333-8333-333333333333";
    let records = [
        json!({"type":"user","sessionId":id,"timestamp":"2026-02-02T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Dialog image"},image(dialog)]}}),
        json!({"type":"assistant","sessionId":id,"timestamp":"2026-02-02T00:00:01Z","message":{"role":"assistant","model":"claude","content":[{"type":"text","text":"Seen."}]}}),
        json!({"type":"user","sessionId":id,"timestamp":"2026-02-02T00:00:02Z","message":{"role":"user","content":"<command-message>skill</command-message>\n<command-name>/skill</command-name>"}}),
        json!({"type":"user","isMeta":true,"sessionId":id,"timestamp":"2026-02-02T00:00:02Z","message":{"role":"user","content":[{"type":"text","text":"Skill expansion"},image(meta),{"type":"text","text":"Base directory for this skill"}]}}),
    ];
    let claude = temp.path().join("claude/project");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join(format!("{id}.jsonl")),
        records.map(|record| format!("{record}\n")).concat(),
    )
    .unwrap();
    let empty = temp.path().join("empty");
    let roots = StoreRoots {
        claude: temp.path().join("claude"),
        codex: empty.clone(),
        pi: empty.clone(),
        cursor: empty.clone(),
        t3: empty.clone(),
        chatgpt: empty,
        t3_explicit: false,
    };
    assert_eq!(
        pull_localhost(&archive, &roots, "machine")
            .unwrap()
            .sessions_failed,
        0
    );
    let stats = render_all(&archive).unwrap();
    assert_eq!(stats.preparation_errors, Vec::<String>::new());
    let text = shared::text(&archive);
    let hash = format!("{:x}", Sha256::digest(dialog));
    assert!(text.contains(&format!("media/sha256/{hash}.png")), "{text}");
    assert!(!text.contains("Skill expansion"), "{text}");
}

#[test]
fn bundle_carries_session_media_with_its_capture() {
    use agent_sessions::bundle::{
        BUNDLE_SCHEMA_VERSION, BundleManifest, ingest_bundle, write_bundle,
    };
    let temp = tempfile::tempdir().unwrap();
    let source = Archive::open(&temp.path().join("source")).unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let payload=json!({"update_time":1,"messages":[{"id":"u","author":{"role":"user"},"create_time":1770000000,"content":{"parts":["image"]}}]}).to_string();
    std::fs::write(store.join("c.json"), &payload).unwrap();
    std::fs::write(store.join("c.meta.json"),json!({"accountId":"one","url":"https://chatgpt.com/backend-api/conversations/c","capturedAt":"2026-10-19T00:00:00Z","sha256":format!("{:x}",Sha256::digest(payload.as_bytes()))}).to_string()).unwrap();
    shared::collect(&source, &store).unwrap();
    let media = agent_sessions::media::MediaStore::new(source.root())
        .put(b"\x89PNG\r\n\x1a\nmedia".as_slice(), "image/png")
        .unwrap();
    let capture = source
        .lookup_capture("c", Some("chatgpt"), None)
        .unwrap()
        .capture;
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        host: capture.meta.host.clone(),
        sources: vec!["chatgpt".into()],
        since_ms: None,
        unit_count: 1,
        units_failed: 0,
        units_retry_later: 0,
        exported_at_ms: 1,
        retention: Default::default(),
    };
    let mut bytes = Vec::new();
    write_bundle(&mut bytes, &manifest, &[capture], &source.captures_dir()).unwrap();
    let target = Archive::open(&temp.path().join("target")).unwrap();
    ingest_bundle(bytes.as_slice(), &target, &mut target.db().unwrap()).unwrap();
    assert!(
        agent_sessions::media::MediaStore::new(target.root())
            .object(&media)
            .is_file()
    );
}

#[test]
fn materialize_rejects_nested_directory_symlinks() {
    use agent_sessions::media::{MaterializationMethod, MediaStore, materialize};
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("archive");
    let output = temp.path().join("views");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::create_dir(&outside).unwrap();
    MediaStore::new(&archive)
        .put(b"image".as_slice(), "image/png")
        .unwrap();
    std::os::unix::fs::symlink(&outside, output.join("media")).unwrap();
    assert!(materialize(&archive, &output, MaterializationMethod::Copy).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}
