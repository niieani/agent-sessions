//! A 507 (Insufficient Storage) from the model server is a server-wide
//! capacity state: the Digest pauses all work and retries one request per
//! interval, instead of failing every item within seconds.
use agent_sessions::{
    archive::Archive,
    digest::{self, CapacityUnavailable, DigestOptions, HttpModelClient, ModelClient},
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

const INSUFFICIENT: &str =
    "Model 'fixture' (20.00GB) does not fit under the dynamic memory ceiling (14.00GB)";

/// One scripted reply: an HTTP status and body.
type Reply = (u16, String);

struct Server {
    address: String,
    requests: Arc<Mutex<usize>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// Serves `script` in order, then `tail` for every later request.
    fn new(script: Vec<Reply>, tail: Option<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, done) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut script = script.into_iter();
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
                            break;
                        }
                    }
                }
                *seen.lock().unwrap() += 1;
                let (status, body) = script
                    .next()
                    .or_else(|| tail.clone())
                    .expect("unexpected model request");
                write!(
                    stream,
                    "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> usize {
        *self.requests.lock().unwrap()
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

fn ok(content: &str) -> Reply {
    (
        200,
        json!({"choices":[{"message":{"content":content}}]}).to_string(),
    )
}

fn insufficient() -> Reply {
    (507, json!({"error": {"message": INSUFFICIENT}}).to_string())
}

const DAY: &str = r#"{"topic":"Archive","title":"Restored work","intent":"Archive sessions","tldr":null,"summary":"Done.","classification":["fix"]}"#;
const TITLE: &str = r#"{"title":"Recovered"}"#;

/// Three synthetic Sessions, each with one closed Session Day to digest.
fn setup(server: &Server, extra: &str) -> (tempfile::TempDir, Archive, HttpModelClient) {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let dialogs: Vec<Conversation> =
        serde_json::from_str(include_str!("fixtures/digest-upgrade/dialog.json")).unwrap();
    let mut db = archive.db().unwrap();
    for index in 0..3 {
        let mut dialog = dialogs[0].clone();
        dialog.provenance.native_id = format!("capacity-fixture-{index}");
        db.upsert_conversation(&dialog).unwrap();
    }
    drop(db);
    fs::write(
        root.path().join("config.toml"),
        format!(
            "[views]\ntimezone='UTC'\n[digest]\ntokenizer = 'tests/fixtures/tokenizer'\nbase-url='{}'\nmodel='fixture'\napi-key-env='UNUSED'\nrequest-timeout='2s'\nretries=2\n{extra}",
            server.address
        ) + if extra.contains("concurrency") { "" } else { "concurrency=1\n" },
    )
    .unwrap();
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

/// Records requested sleeps instead of sleeping.
#[derive(Default)]
struct Sleeps(Mutex<Vec<Duration>>);

impl Sleeps {
    fn taken(&self) -> Vec<Duration> {
        self.0.lock().unwrap().clone()
    }
}

impl digest::CapacitySleep for Sleeps {
    fn sleep(&self, duration: Duration) {
        self.0.lock().unwrap().push(duration);
    }
}

fn day_files(root: &std::path::Path) -> usize {
    walk(&root.join("digests"))
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == "2026-08-28.json")
        })
        .count()
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn http_client_reports_507_as_capacity_unavailable_with_the_server_detail() {
    let server = Server::new(vec![insufficient()], None);
    let (_root, _archive, client) = setup(&server, "");
    let request = digest::ModelRequest {
        kind: digest::CompletionKind::SessionTitle,
        system: "s".into(),
        user: "u".into(),
        corrections: Vec::new(),
    };
    let error = client.complete(&request).unwrap_err();
    let capacity = error
        .downcast_ref::<CapacityUnavailable>()
        .expect("507 is a capacity state");
    assert!(capacity.detail.contains("dynamic memory ceiling"));
}

#[test]
fn capacity_returning_within_the_wait_continues_the_run() {
    let mut script = vec![insufficient(), insufficient(), insufficient()];
    script.extend([ok(DAY), ok(TITLE), ok(DAY), ok(TITLE), ok(DAY), ok(TITLE)]);
    let server = Server::new(script, None);
    let (root, archive, client) = setup(&server, "");
    let sleeps = Sleeps::default();
    let stats = digest::run_with_client_and_sleep(&archive, &client, &options(), &sleeps).unwrap();
    assert_eq!(
        (stats.day_written, stats.session_written, stats.failed),
        (3, 3, 0)
    );
    assert_eq!(server.requests(), 9);
    assert_eq!(stats.requests, 9);
    // Three 507s: one wait per retry at the default interval.
    assert_eq!(sleeps.taken(), vec![Duration::from_secs(300); 3]);
    assert_eq!(day_files(root.path()), 3);
}

#[test]
fn capacity_that_never_returns_ends_the_pass_once_and_leaves_items_pending() {
    let server = Server::new(Vec::new(), Some(insufficient()));
    let (root, archive, client) = setup(
        &server,
        "capacity-retry-interval='10m'\ncapacity-max-wait='30m'\n",
    );
    let sleeps = Sleeps::default();
    let error =
        digest::run_with_client_and_sleep(&archive, &client, &options(), &sleeps).unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "digest model unavailable: insufficient memory for 30 minutes; 6 items left for next run"
    );
    // One request when the 507 arrived, then one per retry interval — not one per item.
    assert_eq!(sleeps.taken(), vec![Duration::from_secs(600); 3]);
    assert_eq!(server.requests(), 4);
    assert_eq!(day_files(root.path()), 0);
    // Nothing was recorded as rejected per item.
    assert!(
        walk(&root.path().join("digests"))
            .iter()
            .all(|path| !path.to_string_lossy().ends_with(".rejected.jsonl"))
    );
    let status: Value =
        serde_json::from_slice(&fs::read(root.path().join("digest-status.json")).unwrap()).unwrap();
    assert_eq!(status["state"], "failed");

    // The next run picks every item up.
    drop(server);
    let server = Server::new(
        vec![ok(DAY), ok(TITLE), ok(DAY), ok(TITLE), ok(DAY), ok(TITLE)],
        None,
    );
    let (_, _, next_client) = {
        // Point the existing Archive at the new server.
        let config = fs::read_to_string(root.path().join("config.toml")).unwrap();
        let old = config
            .lines()
            .find(|line| line.starts_with("base-url="))
            .unwrap()
            .to_owned();
        fs::write(
            root.path().join("config.toml"),
            config.replace(&old, &format!("base-url='{}'", server.address)),
        )
        .unwrap();
        let config = FleetConfig::load(root.path())
            .unwrap()
            .unwrap()
            .digest
            .unwrap();
        (
            (),
            (),
            HttpModelClient::new(&config, "unused".into()).unwrap(),
        )
    };
    let next =
        digest::run_with_client_and_sleep(&archive, &next_client, &options(), &sleeps).unwrap();
    assert_eq!(
        (next.day_written, next.session_written, next.failed),
        (3, 3, 0)
    );
}

#[test]
fn other_http_errors_keep_failing_per_item_without_waiting() {
    let server = Server::new(Vec::new(), Some((503, "busy".into())));
    let (_root, archive, client) = setup(&server, "");
    let sleeps = Sleeps::default();
    let stats = digest::run_with_client_and_sleep(&archive, &client, &options(), &sleeps).unwrap();
    assert_eq!(
        (stats.day_written, stats.session_written, stats.failed),
        (0, 0, 3)
    );
    assert_eq!(server.requests(), 3);
    assert!(sleeps.taken().is_empty());
}

#[test]
fn capacity_config_defaults_and_validation() {
    let base = "[digest]\nbase-url='http://localhost/v1'\nmodel='m'\napi-key-env='K'\n";
    let config = FleetConfig::parse(base).unwrap().digest.unwrap();
    assert_eq!(
        config.capacity_retry_interval().unwrap(),
        Duration::from_secs(300)
    );
    assert_eq!(
        config.capacity_max_wait().unwrap(),
        Duration::from_secs(75 * 60)
    );
    for bad in [
        "capacity-retry-interval='0s'",
        "capacity-retry-interval='soon'",
        "capacity-max-wait='forever'",
    ] {
        assert!(
            FleetConfig::parse(&format!("{base}{bad}\n")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn concurrent_workers_share_one_wait_and_end_together() {
    let server = Server::new(Vec::new(), Some(insufficient()));
    let (_root, archive, client) = setup(
        &server,
        "concurrency=2\ncapacity-retry-interval='1m'\ncapacity-max-wait='2m'\n",
    );
    let sleeps = Sleeps::default();
    let error =
        digest::run_with_client_and_sleep(&archive, &client, &options(), &sleeps).unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "digest model unavailable: insufficient memory for 2 minutes; 6 items left for next run"
    );
    // Only the leader sleeps; at most one concurrent first request also hit 507.
    assert_eq!(sleeps.taken(), vec![Duration::from_secs(60); 2]);
    assert!(
        (3..=4).contains(&server.requests()),
        "{}",
        server.requests()
    );
}
