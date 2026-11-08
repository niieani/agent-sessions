mod common;
use agent_sessions::{archive::Archive, pull::pull_localhost, render};
use rusqlite::Connection;
use std::{fs, path::Path};
#[test]
fn t3_store_to_view_matches_0_2_3_golden_with_wrapper_blank_lines() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("t3");
    fs::create_dir_all(store.join("userdata")).unwrap();
    let db = Connection::open(store.join("userdata/statev2.sqlite")).unwrap();
    db.execute_batch(include_str!("fixtures/t3/v2-schema.sql"))
        .unwrap();
    db.execute_batch(include_str!("fixtures/t3/dialog-golden.sql"))
        .unwrap();
    let archive = Archive::open(&temp.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(temp.path());
    roots.t3 = store;
    roots.t3_explicit = true;
    assert_eq!(
        pull_localhost(&archive, &roots, "testhost")
            .unwrap()
            .sessions_failed,
        0
    );
    common::pin_utc_workday(archive.root());
    render::render_all(&archive).unwrap();
    let actual = common::tree_bytes(&archive.views_dir())
        .into_iter()
        .filter(|(path, _)| path.ends_with(".md"))
        .collect::<Vec<_>>();
    let expected = common::tree_bytes(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/t3/expected"),
    );
    assert_eq!(actual, expected);
}
