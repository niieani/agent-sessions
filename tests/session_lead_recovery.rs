use agent_sessions::{archive::Archive, leads};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::Command,
    thread,
};

fn archive() -> (tempfile::TempDir, Archive) {
    let staging = std::path::PathBuf::from("temp.local")
        .join(time::OffsetDateTime::now_utc().date().to_string());
    fs::create_dir_all(&staging).unwrap();
    let dir = tempfile::tempdir_in(staging).unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    (dir, archive)
}
fn command(archive: &Archive, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(args)
        .arg("--archive")
        .arg(archive.root())
        .output()
        .unwrap()
}
fn lead(cursor: &str) -> Value {
    json!({"cursor":cursor,"source":"codex","sessionId":if cursor == "before" { "previous" } else { "index-repair" },"ledBy":{"kind":"agent","name":cursor},"recordedAt":"2026-10-08T12:00:00Z"})
}
#[test]
fn failed_index_upsert_replays_durable_page_before_advancing_bookmark() {
    let (_dir, archive) = archive();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/leads", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for cursor in ["before", "after", "after"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            requests.push(request);
            let body = json!({"version":1,"leads":[lead(cursor)],"nextCursor":cursor,"more":false})
                .to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    fs::write(
        archive.root().join("config.toml"),
        format!("[[lead-feeds]]\nname='repair'\nurl='{url}'\n"),
    )
    .unwrap();
    assert!(command(&archive, &["leads", "sync"]).status.success());
    let bookmark = fs::read_dir(archive.root().join("lead-feeds"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = fs::read(&bookmark).unwrap();
    let db = rusqlite::Connection::open(archive.db_path()).unwrap();
    db.execute_batch("CREATE TRIGGER fail_lead_upsert BEFORE INSERT ON session_leads BEGIN SELECT RAISE(ABORT,'injected index failure'); END;").unwrap();
    let output = command(&archive, &["leads", "sync"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index refresh failed"));
    assert_eq!(fs::read(&bookmark).unwrap(), before);
    assert_eq!(
        leads::load(archive.root())
            .unwrap()
            .values()
            .next()
            .unwrap()
            .cursor,
        "after"
    );
    let indexed = archive.db().unwrap().session_leads().unwrap();
    assert_eq!(indexed.values().next().unwrap().cursor, "before");
    assert!(!indexed.contains_key(&("codex".into(), "index-repair".into())));
    db.execute_batch("DROP TRIGGER fail_lead_upsert").unwrap();
    let output = command(&archive, &["leads", "sync"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let indexed = archive.db().unwrap().session_leads().unwrap();
    assert_eq!(
        indexed
            .get(&("codex".into(), "index-repair".into()))
            .unwrap()
            .cursor,
        "after"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(bookmark).unwrap()).unwrap()["cursor"],
        "after"
    );
    let requests = server.join().unwrap();
    assert!(requests[1].contains("after=before"));
    assert!(requests[2].contains("after=before"));
}
#[test]
fn inspection_of_pre_leads_schema_gives_actionable_recovery_error_without_writes() {
    let (_dir, archive) = archive();
    let db = rusqlite::Connection::open(archive.db_path()).unwrap();
    db.execute_batch("CREATE TABLE sessions(id INTEGER PRIMARY KEY)")
        .unwrap();
    let before = fs::read(archive.db_path()).unwrap();
    for args in [
        vec!["leads", "list"],
        vec!["leads", "show", "codex:index-repair"],
    ] {
        let output = command(&archive, &args);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("pull") && error.contains("rebuild") && error.contains("first"),
            "{error}"
        );
        assert!(!error.contains("no such table"), "{error}");
    }
    assert_eq!(fs::read(archive.db_path()).unwrap(), before);
}
