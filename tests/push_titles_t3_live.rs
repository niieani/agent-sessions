#[allow(dead_code)]
#[path = "push_titles/support.rs"]
mod support;
#[path = "push_titles/t3_support.rs"]
mod t3_support;

use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};
use support::*;

fn run(f: &Fixture) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sessions"))
        .args(["push-titles", "--archive"])
        .arg(f.archive_dir.path())
        .args(["--source", "t3"])
        .env("T3_BEARER_TOKEN", "fixture-token")
        .output()
        .unwrap()
}

fn digest(f: &Fixture) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(
            agent_sessions::digest::digest_dir(f.archive_dir.path(), "claude", CLAUDE_FRESH)
                .unwrap()
                .join("session.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn serve(root: &Path, reject: bool) -> thread::JoinHandle<Vec<String>> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(root.join("userdata/server-runtime.json"), serde_json::json!({"version":1,"origin":format!("http://{}",listener.local_addr().unwrap())}).to_string()).unwrap();
    let store = root.join("userdata/statev2.sqlite");
    thread::spawn(move || {
        let mut ids = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while ids.len() < if reject { 1 } else { 2 } && Instant::now() < deadline {
            let (socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // Tungstenite requires this concrete, unboxed HTTP error response.
            #[allow(clippy::result_large_err)]
            let mut socket = tungstenite::accept_hdr(
                socket,
                |request: &tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri(), "/ws?orchestrationProtocol=2");
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-token");
                    Ok(response)
                },
            )
            .unwrap();
            let request: serde_json::Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            let command = &request["payload"];
            assert_eq!(request["tag"], "orchestration.dispatchCommand");
            assert_eq!(command["type"], "thread.metadata.update");
            let id = command["threadId"].as_str().unwrap();
            if !reject {
                // This fixture server owns its projection; the CLI only dispatches commands.
                rusqlite::Connection::open(&store)
                    .unwrap()
                    .execute(
                        "UPDATE orchestration_v2_projection_threads SET title=?1 WHERE thread_id=?2",
                        (command["title"].as_str().unwrap(), id),
                    )
                    .unwrap();
            }
            ids.push(id.to_owned());
            let exit = if reject {
                serde_json::json!({"_tag":"Failure","cause":[{"_tag":"Fail","error":{"message":"rejected"}}]})
            } else {
                serde_json::json!({"_tag":"Success","value":{"sequence":1}})
            };
            socket
                .send(tungstenite::Message::Text(
                    serde_json::json!({"_tag":"Exit","requestId":request["id"],"exit":exit})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        }
        ids
    })
}

#[test]
fn live_dispatch_updates_multiple_threads_then_is_noop_preserving_ownership() {
    let (f, root) = t3_support::fixture();
    let server = serve(&root, false);
    let output = run(&f);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.join().unwrap(), ["empty", "own"]);
    let d = digest(&f);
    for id in ["empty", "own", "equal"] {
        assert_eq!(d["pushed"][format!("t3:{id}")], "Claude Fresh");
    }
    assert!(d["pushed"].get("t3:external").is_none());
    let c = rusqlite::Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT title FROM orchestration_v2_projection_threads WHERE thread_id='external'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Person"
    );
    let before = digest(&f);
    // The fixture server is stopped: success proves the second run dispatched no commands.
    let second = run(&f);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(digest(&f), before);
}

#[test]
fn rejected_commands_fail_per_thread_and_preserve_names_and_ownership() {
    let (f, root) = t3_support::fixture();
    let before = digest(&f);
    let server = serve(&root, true);
    let output = run(&f);
    assert!(!output.status.success());
    assert_eq!(server.join().unwrap(), ["empty"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for needle in ["empty", "own", "T3 v2 title command rejected"] {
        assert!(text.contains(needle), "{text}");
    }
    assert!(!text.contains("fixture-token"));
    assert!(text.contains("another interrupted"), "{text}");
    let pending = agent_sessions::digest::digest_dir(f.archive_dir.path(), "claude", CLAUDE_FRESH)
        .unwrap()
        .join("push-title.pending");
    let journal: serde_json::Value = serde_json::from_slice(&fs::read(pending).unwrap()).unwrap();
    assert_eq!(journal["nativeId"], "empty");
    assert_eq!(digest(&f), before);
    let c = rusqlite::Connection::open(root.join("userdata/statev2.sqlite")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT title FROM orchestration_v2_projection_threads WHERE thread_id='own'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Old"
    );
}
