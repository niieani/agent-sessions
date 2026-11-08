mod common;
use agent_sessions::{archive::Archive, leads};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::Command,
    thread,
};

static CLI_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn temp_dir() -> tempfile::TempDir {
    let staging = std::path::PathBuf::from("temp.local")
        .join(time::OffsetDateTime::now_utc().date().to_string());
    fs::create_dir_all(&staging).unwrap();
    tempfile::tempdir_in(staging).unwrap()
}
fn render(archive: &Archive) {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["render", "--archive"])
        .arg(archive.root())
        .env("HOME", archive.root().join("isolated-home"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn lead(cursor: &str, name: Option<&str>) -> Value {
    let mut by = json!({"kind":"agent"});
    if let Some(name) = name {
        by["name"] = json!(name);
    }
    json!({"cursor":cursor,"source":"claude","sessionId":"11111111-1111-4111-8111-111111111111","ledBy":by,"origin":{"kind":"service","job":"42"},"recordedAt":"2026-10-08T10:00:00Z"})
}
fn server(pages: Vec<(u16, Value)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/leads", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = vec![];
        for (status, page) in pages {
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
            let body = page.to_string();
            write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    (url, handle)
}
fn sync(archive: &Archive) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["leads", "sync", "--archive"])
        .arg(archive.root())
        .output()
        .unwrap()
}
#[test]
fn pages_pending_session_rebuild_render_and_idempotency() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let (url, handle) = server(vec![
        (
            200,
            json!({"version":1,"leads":[lead("opaque /1",Some("Coordinator"))],"nextCursor":"opaque /1","more":true}),
        ),
        (
            200,
            json!({"version":1,"leads":[lead("opaque /2",None)],"nextCursor":"opaque /2","more":false}),
        ),
        (
            200,
            json!({"version":1,"leads":[],"nextCursor":"opaque /2","more":false}),
        ),
    ]);
    fs::write(root.path().join("token"), "secret\n").unwrap();
    let template = root.path().join("template.j2");
    fs::write(&template, "{{ session.led_by | tojson }}\n{{ body }}").unwrap();
    let config = format!(
        "[[lead-feeds]]\nname='coordinator'\nurl='{url}'\ntoken-file='token'\n[views]\ntimezone='UTC'\nunit='session-day'\ntemplate='{}'\n",
        template.display()
    );
    fs::write(root.path().join("config.toml"), &config).unwrap();
    let output = sync(&archive);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(archive.db().unwrap().session_leads().unwrap().len(), 1);
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().session_leads().unwrap().len(), 1);
    let store = temp_dir();
    common::copy_tree(&common::fixtures_dir().join("projects"), store.path());
    // Extend the fixture into a second Workday, proving every Session Day receives the lead.
    let rollout = store
        .path()
        .join("-workspace-Projects-example/11111111-1111-4111-8111-111111111111.jsonl");
    let mut file = fs::OpenOptions::new().append(true).open(rollout).unwrap();
    writeln!(file, "{}", json!({"type":"user","message":{"role":"user","content":"Continue tomorrow"},"timestamp":"2026-03-06T10:00:00Z","sessionId":"11111111-1111-4111-8111-111111111111"})).unwrap();
    drop(file);
    common::pull(store.path(), root.path());
    fs::write(root.path().join("config.toml"), &config).unwrap();
    render(&archive);
    let notes = glob::glob(&format!("{}/views/**/*.md", root.path().display()))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| {
            fs::read_to_string(p)
                .unwrap()
                .lines()
                .next()
                .and_then(|line| serde_json::from_str::<Value>(line).ok())
                == Some(json!({"kind":"agent", "origin":{"kind":"service","job":"42"}}))
        })
        .collect::<Vec<_>>();
    assert_eq!(notes.len(), 2);
    assert!(sync(&archive).status.success());
    let requests = handle.join().unwrap();
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer secret")
    );
    assert!(!requests[0].contains("after="));
    assert!(requests[1].contains("after=opaque%20%2F1"));
    assert!(requests[2].contains("after=opaque%20%2F2"));
    let mut records = leads::load(root.path()).unwrap();
    let record = records.values_mut().next().unwrap();
    record.led_by.name = Some("New lead".into());
    leads::store(root.path(), record).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_session_leads(std::slice::from_ref(record))
        .unwrap();
    render(&archive);
    for path in notes {
        assert!(fs::read_to_string(path).unwrap().contains("New lead"));
    }
}
#[test]
fn rejects_bad_pages_without_advancing() {
    let _guard = CLI_TESTS.lock().unwrap();
    for (status, page) in [
        (200, json!({"version":1,"leads":[],"more":false})),
        (
            400,
            json!({"error":{"code":"bad_cursor","message":"unknown cursor"}}),
        ),
        (
            200,
            json!({"version":2,"leads":[],"nextCursor":null,"more":false}),
        ),
        (
            200,
            json!({"version":1,"leads":[lead("one",None)],"nextCursor":"wrong","more":false}),
        ),
        (
            200,
            json!({"version":1,"leads":[],"nextCursor":null,"more":true}),
        ),
        (
            200,
            json!({"version":1,"leads":[lead("one",None),lead("one",None)],"nextCursor":"one","more":false}),
        ),
    ] {
        let root = temp_dir();
        let archive = Archive::open(root.path()).unwrap();
        let (url, handle) = server(vec![(status, page)]);
        fs::write(
            root.path().join("config.toml"),
            format!("[[lead-feeds]]\nname='test'\nurl='{url}'\n"),
        )
        .unwrap();
        assert!(!sync(&archive).status.success());
        handle.join().unwrap();
        assert!(leads::load(root.path()).unwrap().is_empty());
        assert!(!root.path().join("lead-feeds").exists());
    }
}

#[test]
fn partial_failure_keeps_committed_cursor_and_retries_remaining_page() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let (url, handle) = server(vec![
        (
            200,
            json!({"version":1,"leads":[lead("a",Some("First"))],"nextCursor":"a","more":true}),
        ),
        (
            503,
            json!({"error":{"code":"unavailable","message":"retry later"}}),
        ),
        (
            200,
            json!({"version":1,"leads":[lead("b",Some("Second"))],"nextCursor":"b","more":false}),
        ),
    ]);
    fs::write(
        root.path().join("config.toml"),
        format!("[[lead-feeds]]\nname='test'\nurl='{url}'\n"),
    )
    .unwrap();
    assert!(!sync(&archive).status.success());
    assert_eq!(
        leads::load(root.path())
            .unwrap()
            .values()
            .next()
            .unwrap()
            .led_by
            .name
            .as_deref(),
        Some("First")
    );
    assert!(sync(&archive).status.success());
    assert_eq!(
        leads::load(root.path())
            .unwrap()
            .values()
            .next()
            .unwrap()
            .led_by
            .name
            .as_deref(),
        Some("Second")
    );
    let requests = handle.join().unwrap();
    assert!(requests[1].contains("after=a"));
    assert!(requests[2].contains("after=a"));
}

#[test]
fn rejects_explicit_null_optional_fields() {
    for field in ["host", "origin"] {
        let mut record = lead("one", None);
        record[field] = Value::Null;
        assert!(serde_json::from_value::<leads::SessionLead>(record).is_err());
    }
    let mut record = lead("one", None);
    record["ledBy"]["name"] = Value::Null;
    assert!(serde_json::from_value::<leads::SessionLead>(record).is_err());
}

#[test]
fn inspection_and_explicit_storage_config_failures() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let record: leads::SessionLead =
        serde_json::from_value(lead("one", Some("Coordinator"))).unwrap();
    leads::store(root.path(), &record).unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    for args in [
        vec!["leads", "list"],
        vec![
            "leads",
            "show",
            "claude:11111111-1111-4111-8111-111111111111",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
            .args(args)
            .arg("--archive")
            .arg(root.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("Coordinator")
        );
    }
    // Missing optional credentials are an error when a token-file was configured.
    fs::write(
        root.path().join("config.toml"),
        "[[lead-feeds]]\nname='test'\nurl='http://127.0.0.1:1/leads'\ntoken-file='missing'\n",
    )
    .unwrap();
    let output = sync(&archive);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("token-file"));
    let lock = archive.acquire_writer_lock().unwrap();
    let output = sync(&archive);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another writer active"));
    drop(lock);
    // No archive-owned input may redirect publication or reads through a symlink.
    let target = temp_dir();
    std::os::unix::fs::symlink(target.path(), root.path().join("lead-feeds")).unwrap();
    assert!(!sync(&archive).status.success());
    assert!(fs::read_dir(target.path()).unwrap().next().is_none());
}

#[test]
fn filesystem_clutter_does_not_break_rebuild_render_or_indexed_inspection() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let record: leads::SessionLead = serde_json::from_value(lead("one", None)).unwrap();
    leads::store(root.path(), &record).unwrap();
    for relative in [
        "leads/.DS_Store",
        "leads/claude/.DS_Store",
        "leads/stray",
        "leads/claude/stray",
        "leads/claude/stray.json",
    ] {
        fs::write(root.path().join(relative), "junk").unwrap();
    }
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(archive.db().unwrap().session_leads().unwrap().len(), 1);
    render(&archive);
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["leads", "list", "--archive"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<Value>>(&output.stdout)
            .unwrap()
            .len(),
        1
    );
    let warnings = String::from_utf8_lossy(&output.stderr);
    assert!(warnings.contains("stray"));
    assert!(!warnings.contains(".DS_Store"));
    // Rendering/inspection use the committed index; no JSON file decoding on the hot path.
    fs::write(
        leads::lead_path(root.path(), &record).unwrap(),
        "invalid JSON",
    )
    .unwrap();
    render(&archive);
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["leads", "list", "--archive"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    // Canonical durable inputs remain strict when rebuilding.
    assert!(agent_sessions::archive::rebuild(&archive).is_err());
}

#[test]
fn sync_updates_only_page_leads_without_scanning_or_rewriting_existing_index() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let mut existing: leads::SessionLead = serde_json::from_value(lead("old", None)).unwrap();
    existing.session_id = "unrelated".into();
    leads::store(root.path(), &existing).unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    // Any full-index rewrite would fire this trigger; any full file load would decode junk.
    rusqlite::Connection::open(archive.db_path()).unwrap().execute_batch(
        "CREATE TRIGGER preserve_unrelated BEFORE DELETE ON session_leads WHEN OLD.native_id='unrelated' BEGIN SELECT RAISE(ABORT,'unrelated index row deleted'); END;"
    ).unwrap();
    fs::write(leads::lead_path(root.path(), &existing).unwrap(), "junk").unwrap();
    let (url, handle) = server(vec![(
        200,
        json!({"version":1,"leads":[lead("new",None)],"nextCursor":"new","more":false}),
    )]);
    fs::write(
        root.path().join("config.toml"),
        format!("[[lead-feeds]]\nname='test'\nurl='{url}'\n"),
    )
    .unwrap();
    let output = sync(&archive);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    handle.join().unwrap();
    assert_eq!(archive.db().unwrap().session_leads().unwrap().len(), 2);
}

#[test]
fn wire_validation_errors_identify_cursor_and_field() {
    let _guard = CLI_TESTS.lock().unwrap();
    for (field, bad) in [
        ("ledBy.name", json!("")),
        ("host", json!(" ")),
        ("origin.kind", json!("")),
        ("origin.job", json!("")),
        ("origin.requestedBy", json!("missing-colon")),
        ("origin.requestedBy", json!("claude:")),
        ("sessionId", json!("bad\nidentity")),
        ("host", json!(42)),
        ("ledBy.name", Value::Null),
    ] {
        let root = temp_dir();
        let archive = Archive::open(root.path()).unwrap();
        let mut record = lead("offending-cursor", None);
        if let Some((parent, key)) = field.split_once('.') {
            record[parent][key] = bad;
        } else {
            record[field] = bad;
        }
        let (url, handle) = server(vec![(
            200,
            json!({"version":1,"leads":[record],"nextCursor":"offending-cursor","more":false}),
        )]);
        fs::write(
            root.path().join("config.toml"),
            format!("[[lead-feeds]]\nname='test'\nurl='{url}'\n"),
        )
        .unwrap();
        let output = sync(&archive);
        handle.join().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("offending-cursor") && error.contains(field),
            "{error}"
        );
        assert!(leads::load(root.path()).unwrap().is_empty());
    }
}

#[test]
fn consumers_ignore_unknown_response_fields() {
    let _guard = CLI_TESTS.lock().unwrap();
    let root = temp_dir();
    let archive = Archive::open(root.path()).unwrap();
    let mut record = lead("future", None);
    record["futureLeadField"] = json!({"anything":[1,2]});
    record["ledBy"]["futureAgentField"] = json!(false);
    let (url, handle) = server(vec![(
        200,
        json!({"version":1,"leads":[record],"nextCursor":"future","more":false,"futureResponseField":true}),
    )]);
    fs::write(
        root.path().join("config.toml"),
        format!("[[lead-feeds]]\nname='test'\nurl='{url}'\n"),
    )
    .unwrap();
    let output = sync(&archive);
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(archive.db().unwrap().session_leads().unwrap().len(), 1);
}
