mod common;
use agent_sessions::{
    archive::{Archive, rebuild},
    assignments,
};
use common::{pull, temp_store, tree_bytes};
use std::fs;
const ID: &str = "11111111-1111-4111-8111-111111111111";
fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Archive) {
    let store = temp_store();
    let dir = tempfile::tempdir().unwrap();
    pull(store.path(), dir.path());
    let archive = Archive::open_existing(dir.path()).unwrap();
    (store, dir, archive)
}
#[test]
fn assignments_survive_rebuild_and_capture_updates_and_unassign_cleanly() {
    let (store, _dir, archive) = fixture();
    let before = tree_bytes(&archive.captures_dir());
    assignments::assign(
        &archive,
        "claude",
        ID,
        "Unit 802 Floor Plans",
        Some("confirmed from Dialog"),
    )
    .unwrap();
    let first = assignments::load(archive.root()).unwrap();
    assert_eq!(
        first[&("claude".into(), ID.into())].project,
        "Unit 802 Floor Plans"
    );
    assert_eq!(archive.db().unwrap().project_assignments().unwrap(), first);
    rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().project_assignments().unwrap(), first);
    pull(store.path(), archive.root());
    assert_eq!(assignments::load(archive.root()).unwrap(), first);
    assert_eq!(tree_bytes(&archive.captures_dir()), before);
    assignments::assign(&archive, "claude", ID, "Updated Project", None).unwrap();
    assert_eq!(
        assignments::load(archive.root()).unwrap()[&("claude".into(), ID.into())].project,
        "Updated Project"
    );
    assignments::unassign(&archive, "claude", ID).unwrap();
    rebuild(&archive).unwrap();
    assert!(assignments::load(archive.root()).unwrap().is_empty());
    assert!(
        archive
            .db()
            .unwrap()
            .project_assignments()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn rejects_unknown_sessions_invalid_projects_and_malformed_durable_records() {
    let (_store, _dir, archive) = fixture();
    for project in ["", "  ", "[[Project]]", "Project\nOther"] {
        assert!(assignments::assign(&archive, "claude", ID, project, None).is_err());
    }
    assert!(assignments::assign(&archive, "claude", "missing", "Project", None).is_err());
    assert!(assignments::unassign(&archive, "claude", "missing").is_err());
    assignments::assign(&archive, "claude", ID, "Project", None).unwrap();
    fs::write(
        assignments::assignment_path(archive.root(), "claude", ID).unwrap(),
        "{}",
    )
    .unwrap();
    assert!(assignments::load(archive.root()).is_err());
    let before_db = fs::read(archive.db_path()).unwrap();
    assert!(rebuild(&archive).is_err());
    assert_eq!(fs::read(archive.db_path()).unwrap(), before_db);
}
#[test]
fn file_publication_survives_index_failure_and_rebuild_recovers() {
    let (_store, _dir, archive) = fixture();
    let connection = rusqlite::Connection::open(archive.db_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_assignment BEFORE INSERT ON project_assignments BEGIN SELECT RAISE(FAIL, 'injected index failure'); END;").unwrap();
    let error = assignments::assign(&archive, "claude", ID, "Durable Project", None).unwrap_err();
    assert!(format!("{error:#}").contains("injected index failure"));
    assert_eq!(
        assignments::load(archive.root()).unwrap()[&("claude".into(), ID.into())].project,
        "Durable Project"
    );
    drop(connection);
    rebuild(&archive).unwrap();
    assert_eq!(
        archive.db().unwrap().project_assignments().unwrap(),
        assignments::load(archive.root()).unwrap()
    );
}
#[test]
fn opaque_ids_are_bounded_and_records_must_match_their_paths() {
    let (_store, _dir, archive) = fixture();
    let child = "33333333-3333-4333-8333-333333333333/subagents/agent-a0123456789abcdef";
    assignments::assign(&archive, "claude", child, "Child Project", None).unwrap();
    let path = assignments::assignment_path(archive.root(), "claude", child).unwrap();
    assert_eq!(
        path.parent().unwrap(),
        archive.root().join("assignments/claude")
    );
    assert!(assignments::assignment_path(archive.root(), "../x", ID).is_err());
    assert!(
        assignments::assignment_path(archive.root(), "claude", &"/".repeat(1000))
            .unwrap()
            .file_name()
            .unwrap()
            .len()
            < 255
    );
    let record = fs::read(&path).unwrap();
    fs::write(archive.root().join("assignments/claude/wrong.json"), record).unwrap();
    assert!(assignments::load(archive.root()).is_err());
}
#[cfg(unix)]
#[test]
fn refuses_symlinked_assignment_directories_and_records() {
    use std::os::unix::fs::symlink;
    let (_store, _dir, archive) = fixture();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), archive.root().join("assignments")).unwrap();
    assert!(assignments::assign(&archive, "claude", ID, "Project", None).is_err());
    assert!(assignments::load(archive.root()).is_err());
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn durable_unassignment_wins_over_stale_index_after_interruption() {
    let (_store, _dir, archive) = fixture();
    assignments::assign(&archive, "claude", ID, "Project", None).unwrap();
    let connection = rusqlite::Connection::open(archive.db_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_assignment_delete BEFORE DELETE ON project_assignments BEGIN SELECT RAISE(FAIL, 'injected delete failure'); END;").unwrap();
    assert!(assignments::unassign(&archive, "claude", ID).is_err());
    assert!(assignments::load(archive.root()).unwrap().is_empty());
    assert_eq!(
        archive.db().unwrap().project_assignments().unwrap().len(),
        1
    );
    drop(connection);
    rebuild(&archive).unwrap();
    assert!(
        archive
            .db()
            .unwrap()
            .project_assignments()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_source_and_record_and_ignores_unpublished_temps() {
    use std::os::unix::fs::symlink;
    let (_store, _dir, archive) = fixture();
    fs::create_dir(archive.root().join("assignments")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), archive.root().join("assignments/codex")).unwrap();
    assert!(assignments::load(archive.root()).is_err());
    // Separate fixture for file links; neither read nor mutation may follow it.
    let (_store2, _dir2, archive2) = fixture();
    assignments::assign(&archive2, "claude", ID, "Project", None).unwrap();
    let source_dir = archive2.root().join("assignments/claude");
    fs::write(source_dir.join(".assignment-interrupted"), "{partial").unwrap();
    assert_eq!(assignments::load(archive2.root()).unwrap().len(), 1);
    let target = outside.path().join("record.json");
    fs::write(&target, "outside").unwrap();
    symlink(&target, source_dir.join("alias.json")).unwrap();
    assert!(assignments::load(archive2.root()).is_err());
    assert!(assignments::assign(&archive2, "claude", ID, "New Project", None).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "outside");
}
