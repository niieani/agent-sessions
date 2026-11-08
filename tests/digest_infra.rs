mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, HttpModelClient, ModelClient, ModelRequest};
use agent_sessions::fleet::{DigestConfig, FleetConfig};
use common::{pull, temp_store, tree_bytes};

fn config() -> &'static str {
    r#"[views]
timezone = "UTC"
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
concurrency = 2
max-input-tokens = 60000
request-timeout = "1s"
"#
}

#[test]
fn http_client_sends_strict_schema_and_extra_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server_capture = Arc::clone(&captured);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            bytes.extend_from_slice(&chunk[..read]);
            let header_end = bytes.windows(4).position(|window| window == b"\r\n\r\n");
            if let Some(header_end) = header_end {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if bytes.len() >= header_end + 4 + content_length {
                    *server_capture.lock().unwrap() =
                        bytes[header_end + 4..header_end + 4 + content_length].to_vec();
                    break;
                }
            }
        }
        let content = r#"{"title":"Work Sessions"}"#;
        let body = serde_json::json!({"choices":[{"message":{"content":content,"reasoning_content":"ignored"}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let mut extra = toml::Table::new();
    extra.insert(
        "chat_template_kwargs".into(),
        toml::Value::Table(toml::Table::from_iter([(
            "enable_thinking".into(),
            toml::Value::Boolean(false),
        )])),
    );
    let client = HttpModelClient::new(
        &DigestConfig {
            base_url: format!("http://{address}/v1"),
            model: "fixture".into(),
            tokenizer: None,
            tokenizer_prompt_format: Default::default(),
            api_key_env: "IGNORED".into(),
            concurrency: 1,
            max_input_tokens: 1000,
            request_timeout: "1s".into(),
            retries: 2,
            progress_interval: "5m".into(),
            prompts: Default::default(),
            extra_body: extra,
        },
        "secret".into(),
    )
    .unwrap();
    let output = client
        .complete(&ModelRequest {
            kind: CompletionKind::SessionTitle,
            system: "system".into(),
            user: "user".into(),
            corrections: Vec::new(),
        })
        .unwrap();
    server.join().unwrap();
    assert_eq!(output, r#"{"title":"Work Sessions"}"#);
    let request: serde_json::Value = serde_json::from_slice(&captured.lock().unwrap()).unwrap();
    assert_eq!(request["response_format"]["type"], "json_schema");
    assert_eq!(request["response_format"]["json_schema"]["strict"], true);
    assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
}

#[test]
fn config_rejects_omit_treatment_conflicts_and_reserved_extra_body() {
    FleetConfig::parse(&format!(
        "{}\n[views.kind.default-digest]\nmatch.path.is = '/x'\ndigest = false\n",
        config()
    ))
    .unwrap();
    assert!(
        FleetConfig::parse(&format!(
            "{}\n[views.kind.bad]\nmatch.path.is = '/x'\nomit = true\ndigest = false\n",
            config()
        ))
        .is_err()
    );
    assert!(
        FleetConfig::parse(
            &config().replace("request-timeout = \"1s\"", "request-timeout = 'soon'")
        )
        .is_err()
    );
    assert!(
        FleetConfig::parse(&format!(
            "{}\n[digest.extra-body]\nmodel = 'override'\n",
            config()
        ))
        .is_err()
    );
}

#[test]
fn rebuild_preserves_digest_files_and_rebuilds_cache() {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "preserved").unwrap();
    fs::create_dir_all(&dir).unwrap();
    let day = serde_json::json!({
        "schemaVersion": 1,
        "source": "codex",
        "nativeId": "preserved",
        "workday": "2026-08-30",
        "covered": {"firstIdx": 0, "lastIdx": 2},
        "topic": "Archive",
        "title": "Archive - Preserved",
        "lockedTitle": "Archive - Preserved",
        "tldr": null,
        "summary": "Preserved.",
        "classification": ["build"],
        "truncated": false,
        "model": "fixture",
        "promptVersion": 1,
        "digestedAtMs": 1
    });
    let session = serde_json::json!({
        "schemaVersion": 1,
        "topic": "Archive",
        "title": "Archive Sessions",
        "updatedAtMs": 1,
        "model": "fixture",
        "promptVersion": 1,
        "pushed": {}
    });
    fs::write(
        dir.join("2026-08-30.json"),
        serde_json::to_vec_pretty(&day).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec_pretty(&session).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("2026-08-30.rejected.jsonl"),
        "{\"attemptedAtMs\":1,\"kind\":\"day\",\"attempt\":1,\"error\":\"bad\",\"raw\":\"{}\"}\n",
    )
    .unwrap();
    let rejected_before = fs::read(dir.join("2026-08-30.rejected.jsonl")).unwrap();
    let before = fs::read(dir.join("2026-08-30.json")).unwrap();
    agent_sessions::archive::rebuild(&archive).unwrap();
    assert_eq!(fs::read(dir.join("2026-08-30.json")).unwrap(), before);
    assert_eq!(
        fs::read(dir.join("2026-08-30.rejected.jsonl")).unwrap(),
        rejected_before
    );
    let connection = rusqlite::Connection::open(archive.db_path()).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM digest_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

struct StaticClient;

impl ModelClient for StaticClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        Ok(match request.kind {
            CompletionKind::FirstSessionDay => {
                r#"{"topic":"Store","title":"Read only check","intent":"Do the work","tldr":null,"summary":"Checked the Store boundary.","classification":["test"]}"#.into()
            }
            CompletionKind::ContinuationSessionDay => {
                r#"{"title":"Continued read only check","intent":null,"tldr":null,"summary":"Continued checking.","classification":["test"]}"#.into()
            }
            CompletionKind::ContinuationSessionDayWithoutIntent => {
                r#"{"title":"Continued read only check","intent":"Do the work","tldr":null,"summary":"Continued checking.","classification":["test"]}"#.into()
            }
            CompletionKind::SessionTitle => r#"{"title":"Store Read Only Sessions"}"#.into(),
        })
    }
}

#[test]
fn pull_digest_and_render_leave_native_store_byte_identical() {
    let store = temp_store();
    let before = tree_bytes(store.path());
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("config.toml"), config()).unwrap();
    let pulled = pull(store.path(), root.path());
    assert!(pulled.conversations_extracted > 0);
    let archive = Archive::open(root.path()).unwrap();
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        &StaticClient,
        &agent_sessions::digest::DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: agent_sessions::timefmt::parse_iso_ms("2030-01-01T00:00:00Z").unwrap(),
        },
    )
    .unwrap();
    assert_eq!(stats.failed, 0);
    agent_sessions::render::render_all(&archive).unwrap();

    assert_eq!(tree_bytes(store.path()), before, "native Store changed");
}
