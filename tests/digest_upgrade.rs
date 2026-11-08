//! Upgrading never regenerates Digests whose Dialog is unchanged.
//!
//! `tests/fixtures/digest-upgrade/<release>/digests` holds the Digest files
//! exactly as that release wrote them for `dialog.json`. Each release that
//! changes the stored Digest format adds its own directory; every directory
//! must keep planning zero model work against the current build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{DigestOptions, DigestStats, ModelClient, ModelRequest};
use agent_sessions::model::Conversation;
use serde_json::Value;

/// Fails the test on any model call: an upgrade must not need one.
struct ForbiddenClient;

impl ModelClient for ForbiddenClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        panic!("upgrade made a {:?} model request", request.kind)
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/digest-upgrade")
}

fn releases() -> Vec<PathBuf> {
    let mut releases = fs::read_dir(fixture_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("digests").is_dir())
        .collect::<Vec<_>>();
    releases.sort();
    assert!(
        releases.iter().any(|path| path.ends_with("v0.2.3")),
        "the fingerprint-less release fixture is present"
    );
    releases
}

fn dialog() -> Vec<Conversation> {
    serde_json::from_slice(&fs::read(fixture_root().join("dialog.json")).unwrap()).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// An Archive holding the fixture Dialog and one release's Digest files.
fn upgraded_archive(release: &Path, dialog: &[Conversation]) -> (tempfile::TempDir, Archive) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::copy(
        fixture_root().join("config.toml"),
        root.path().join("config.toml"),
    )
    .unwrap();
    let mut db = archive.db().unwrap();
    for conversation in dialog {
        db.upsert_conversation(conversation).unwrap();
    }
    drop(db);
    copy_tree(&release.join("digests"), &root.path().join("digests"));
    (root, archive)
}

fn options(dry_run: bool) -> DigestOptions {
    DigestOptions {
        day: None,
        from: None,
        through: None,
        session: None,
        force: false,
        dry_run,
        now_ms: agent_sessions::timefmt::parse_iso_ms("2026-10-01T00:00:00Z").unwrap(),
    }
}

fn run(archive: &Archive, dry_run: bool) -> DigestStats {
    agent_sessions::digest::run_with_client(archive, &ForbiddenClient, &options(dry_run)).unwrap()
}

#[test]
fn every_release_fixture_plans_no_model_work_for_unchanged_dialog() {
    let dialog = dialog();
    for release in releases() {
        let (root, archive) = upgraded_archive(&release, &dialog);
        let name = release.file_name().unwrap().to_string_lossy();
        let planned = run(&archive, true);
        assert_eq!(
            (planned.candidates, planned.requests),
            (0, 0),
            "{name}: dry-run plans Digest work for unchanged Dialog: {:?}",
            planned.previews
        );
        let ran = run(&archive, false);
        assert_eq!(
            (
                ran.requests,
                ran.day_written,
                ran.session_written,
                ran.failed
            ),
            (0, 0, 0, 0),
            "{name}: upgrade run regenerated Digests"
        );
        for path in files(&root.path().join("digests")) {
            let mut after = read_json(&path);
            if path.file_name().unwrap() != "session.json" {
                assert!(
                    after["dialogFingerprint"].is_string(),
                    "{name}: {} keeps no Dialog fingerprint",
                    path.display()
                );
            }
            let before = read_json(&release.join(path.strip_prefix(root.path()).unwrap()));
            if before.get("dialogFingerprint").is_none() {
                after.as_object_mut().unwrap().remove("dialogFingerprint");
            }
            assert_eq!(before, after, "{name}: {} changed", path.display());
        }
        let again = run(&archive, true);
        assert_eq!(
            (again.candidates, again.fingerprints_backfilled),
            (0, 0),
            "{name}"
        );
    }
}

#[test]
fn release_fixtures_report_no_new_or_redo_work_through_the_cli_dry_run() {
    let dialog = dialog();
    for release in releases() {
        let (root, _archive) = upgraded_archive(&release, &dialog);
        let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(["digest", "--dry-run", "--archive"])
            .arg(root.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout
                .lines()
                .last()
                .is_some_and(|line| line.starts_with("digest dry-run: 0 new · 0 redo")),
            "{}: {stdout}",
            release.display()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(["digest", "--dry-run", "--json", "--archive"])
            .arg(root.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            (&summary["new"], &summary["redo"], &summary["warning"]),
            (&0.into(), &0.into(), &Value::Null),
            "{}",
            release.display()
        );
    }
}

#[test]
fn backfill_never_adopts_a_digest_whose_covered_range_moved() {
    let mut dialog = dialog();
    let mail = dialog
        .iter_mut()
        .find(|conversation| conversation.provenance.native_id == "019a2f3c-mail-optout")
        .unwrap();
    let mut extra = mail.messages[2].clone();
    extra.text = "also unsubscribe the catalogs".into();
    mail.messages.insert(3, extra);
    let release = fixture_root().join("v0.2.3");
    let (_root, archive) = upgraded_archive(&release, &dialog);
    let planned = run(&archive, true);
    let days = planned
        .previews
        .iter()
        .map(|preview| (preview.native_id.as_str(), preview.workday.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        days,
        [
            ("019a2f3c-mail-optout", "2026-08-28"),
            ("019a2f3c-mail-optout", "2026-08-29")
        ],
        "both Session Days of the changed Session have moved ranges"
    );
    assert_eq!(planned.fingerprints_backfilled, 3);
}

#[test]
fn changing_repository_project_preserves_every_release_digest() {
    for release in releases() {
        let mut conversations = dialog();
        let (_root, archive) = upgraded_archive(&release, &conversations);
        assert_eq!(run(&archive, true).candidates, 0);
        for conversation in &mut conversations {
            conversation.project_path = Some("/different/worktree".into());
            conversation
                .transcript_facts
                .git
                .get_or_insert_with(Default::default)
                .repository_url = Some("https://github.com/example-org/infrastructure.git".into());
            archive
                .db()
                .unwrap()
                .upsert_conversation(conversation)
                .unwrap();
        }
        assert_eq!(run(&archive, true).candidates, 0);
        assert_eq!(run(&archive, false).requests, 0);
    }
}

/// Re-extract real-shaped Codex Captures against released Digest fixtures:
/// machine tags and mirrored histories must not invalidate unrelated Dialog.
#[test]
fn codex_reply_recovery_only_redoes_changed_dialog() {
    use agent_sessions::adapters::{Adapter, Capture, CaptureOutcome, codex::CodexAdapter};
    let baseline = dialog();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/question-replies");
    for release in releases() {
        let (root, archive) = upgraded_archive(&release, &baseline);
        let extract = |name: &str, index: usize| {
            let original = &baseline[index];
            let body_path = root.path().join("input.jsonl.zst");
            let input = fs::File::open(fixture.join(name)).unwrap();
            let output = fs::File::create(&body_path).unwrap();
            zstd::stream::copy_encode(input, output, 0).unwrap();
            let meta = serde_json::from_value(serde_json::json!({
                "schemaVersion":1, "source":"codex", "nativeId":original.provenance.native_id,
                "host":"studio", "nativePath":"/redacted/rollout.jsonl", "kind":"main",
                "sourceMtimeMs":1, "sourceSize":1, "capturedAtMs":1, "adapterVersion":5
            }))
            .unwrap();
            CodexAdapter
                .extract(&Capture {
                    body_path,
                    meta_path: root.path().join("unused.meta.json"),
                    meta,
                    written: false,
                    outcome: CaptureOutcome::Unchanged,
                })
                .unwrap()
                .unwrap()
        };
        for (index, original) in baseline
            .iter()
            .enumerate()
            .filter(|(_, conversation)| conversation.provenance.source == "codex")
        {
            let extracted = extract(&format!("unchanged-{index}.jsonl"), index);
            assert_eq!(extracted.messages.len(), original.messages.len());
            for (after, before) in extracted.messages.iter().zip(&original.messages) {
                assert_eq!(after.timestamp_ms, before.timestamp_ms);
                assert_eq!(
                    (&after.role, &after.kind, &after.text),
                    (&before.role, &before.kind, &before.text)
                );
            }
            archive
                .db()
                .unwrap()
                .upsert_conversation(&extracted)
                .unwrap();
        }
        assert_eq!(
            run(&archive, true).candidates,
            0,
            "unchanged Dialog must stay fresh"
        );
        let restored = extract("restored.jsonl", 0);
        assert_eq!(restored.messages.len(), baseline[0].messages.len() + 1);
        assert_eq!(
            restored.messages.last().unwrap().text,
            "Question: Install?\n\nAnswer: Install and verify\n\nQuestion: Where?\n\nAnswer: Local only"
        );
        archive
            .db()
            .unwrap()
            .upsert_conversation(&restored)
            .unwrap();
        let planned = run(&archive, true);
        assert!(planned.candidates > 0);
        assert!(planned.plan.total().redo_days() > 0);
        assert!(
            planned
                .previews
                .iter()
                .all(|preview| preview.native_id == baseline[0].provenance.native_id)
        );
    }
}
