use agent_sessions::{
    archive::Archive,
    digest::{DigestOptions, ModelClient, ModelRequest},
};
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn correction_history_cannot_exceed_the_real_input_limit() {
    struct InvalidClient(AtomicUsize);
    impl ModelClient for InvalidClient {
        fn complete(&self, _: &ModelRequest) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("invalid output ".repeat(40_000))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(root.path().join("config.toml"), "[views]\ntimezone='UTC'\n[digest]\nbase-url='http://localhost/v1'\nmodel='fixture'\napi-key-env='UNUSED'\ntokenizer='tests/fixtures/tokenizer'\n").unwrap();
    let dialogs: Vec<agent_sessions::model::Conversation> =
        serde_json::from_slice(&fs::read("tests/fixtures/digest-upgrade/dialog.json").unwrap())
            .unwrap();
    for dialog in dialogs.into_iter().take(1) {
        archive.db().unwrap().upsert_conversation(&dialog).unwrap();
    }
    let client = InvalidClient(AtomicUsize::new(0));
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: 1_900_000_000_000,
        },
    )
    .unwrap();
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.requests, 1);
    assert_eq!(client.0.load(Ordering::SeqCst), 1);
    assert_eq!(stats.day_written, 0);
}
