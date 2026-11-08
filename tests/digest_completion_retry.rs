use agent_sessions::{
    archive::Archive,
    digest::{self, DigestOptions, HttpModelClient},
    fleet::FleetConfig,
    model::Conversation,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct Server {
    address: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, done) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut responses = responses.into_iter();
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let read = stream.read(&mut chunk).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + len {
                            seen.lock().unwrap().push(
                                serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap(),
                            );
                            break;
                        }
                    }
                }
                let body = responses
                    .next()
                    .expect("unexpected model request")
                    .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
fn response(content: &str) -> Value {
    json!({"choices":[{"message":{"content":content}}]})
}
fn setup(server: &Server) -> (tempfile::TempDir, Archive, HttpModelClient) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let dialogs: Vec<Conversation> =
        serde_json::from_str(include_str!("fixtures/digest-upgrade/dialog.json")).unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&dialogs[0])
        .unwrap();
    fs::write(root.path().join("config.toml"), format!("[views]\ntimezone='UTC'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='{}'\nmodel='fixture'\napi-key-env='UNUSED'\nconcurrency=1\nrequest-timeout='2s'\nretries=2\n", server.address)).unwrap();
    let config = FleetConfig::load(root.path())
        .unwrap()
        .unwrap()
        .digest
        .unwrap();
    let client = HttpModelClient::new(&config, "unused".into()).unwrap();
    (root, archive, client)
}
fn options() -> DigestOptions {
    DigestOptions {
        day: Some("2026-08-28".into()),
        from: None,
        through: None,
        session: None,
        force: false,
        dry_run: false,
        now_ms: 1_900_000_000_000,
    }
}
const DAY: &str = r#"{"topic":"Archive","title":"Restored work","intent":"Archive sessions","tldr":null,"summary":"Done.","classification":["fix"]}"#;

#[test]
fn missing_or_null_completion_content_is_rejected_and_retried() {
    for bad in [
        json!({"choices":[]}),
        json!({"choices":[{"message":{"content":null}}]}),
        json!({"choices":[{"message":{}}]}),
    ] {
        let server = Server::new(vec![
            bad.clone(),
            response(DAY),
            response(r#"{"title":"Recovered"}"#),
        ]);
        let (root, archive, client) = setup(&server);
        let result = digest::run_with_client(&archive, &client, &options()).unwrap();
        assert_eq!(
            (
                result.requests,
                result.day_written,
                result.session_written,
                result.failed
            ),
            (3, 1, 1, 0)
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let rejected: Value =
            serde_json::from_str(requests[1]["messages"][2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(rejected, bad);
        assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 4);
        let rejects = root.path().join("digests");
        assert!(rejects.is_dir());
    }
}

#[test]
fn invalid_completion_retries_are_bounded_and_missing_days_retry_next_run() {
    let bad = json!({"choices":[{"message":{"content":null}}]});
    let server = Server::new(vec![
        bad.clone(),
        bad.clone(),
        bad,
        response(DAY),
        response(r#"{"title":"Recovered"}"#),
    ]);
    let (_root, archive, client) = setup(&server);
    let first = digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!((first.requests, first.day_written, first.failed), (3, 0, 1));
    let next = digest::run_with_client(&archive, &client, &options()).unwrap();
    assert_eq!(
        (
            next.requests,
            next.day_written,
            next.session_written,
            next.failed
        ),
        (2, 1, 1, 0)
    );
}

#[test]
fn invalid_intent_classification_and_truncated_json_require_corrected_output() {
    for bad in [
        DAY.replace("\"Archive sessions\"", "null"),
        DAY.replace("\"fix\"", "\"design\""),
        "{\"topic\":\"Unfinished".into(),
        "   ".into(),
    ] {
        let server = Server::new(vec![
            response(&bad),
            response(DAY),
            response(r#"{"title":"Recovered"}"#),
        ]);
        let (_root, archive, client) = setup(&server);
        let result = digest::run_with_client(&archive, &client, &options()).unwrap();
        assert_eq!(
            (result.requests, result.day_written, result.failed),
            (3, 1, 0)
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests[1]["messages"][2]["content"], bad);
        assert!(
            requests[1]["messages"][3]["content"]
                .as_str()
                .unwrap()
                .contains("rejected")
        );
        assert_eq!(
            requests[0]["response_format"]["json_schema"]["strict"],
            true
        );
        let allowed = &requests[0]["response_format"]["json_schema"]["schema"]["properties"]["classification"]
            ["items"]["enum"];
        assert!(!allowed.as_array().unwrap().contains(&json!("design")));
    }
}
