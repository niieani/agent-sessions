use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{path::Path, time::Duration};

pub(super) fn dispatch(root: &Path, thread_id: &str, title: &str) -> Result<()> {
    let token = std::env::var("T3_BEARER_TOKEN")
        .context("T3_BEARER_TOKEN must contain an existing T3 bearer access token")?;
    dispatch_with_token(root, thread_id, title, &token)
}

fn dispatch_with_token(root: &Path, thread_id: &str, title: &str, token: &str) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COMMAND_COUNTER: AtomicU64 = AtomicU64::new(0);
    if token.trim().is_empty() {
        bail!("T3_BEARER_TOKEN must contain an existing T3 bearer access token");
    }
    if thread_id.trim().is_empty() || title.trim().is_empty() {
        bail!("T3 Thread id and Session Title must be non-empty");
    }
    let path = root.join("userdata/server-runtime.json");
    let runtime: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(&path).context("T3 running server runtime state is unavailable")?,
    )
    .context("T3 server runtime state is invalid JSON")?;
    if runtime["version"] != 1 {
        bail!("Unsupported T3 server runtime state version");
    }
    let origin = runtime["origin"]
        .as_str()
        .context("T3 server runtime state has no origin")?;
    let uri: ureq::http::Uri = origin.parse().context("Invalid T3 server origin")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri
            .authority()
            .is_none_or(|authority| authority.as_str().contains('@'))
        || uri
            .path_and_query()
            .is_some_and(|path| path.as_str() != "/")
    {
        bail!("T3 server origin must be an HTTP(S) origin without credentials or a path");
    }
    let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let command_id = format!(
        "agent-sessions-{}-{}-{}",
        std::process::id(),
        elapsed.as_nanos(),
        COMMAND_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    use std::net::{TcpStream, ToSocketAddrs};
    use tungstenite::{Message, client::IntoClientRequest};
    let host = uri
        .host()
        .context("Invalid T3 server origin: missing host")?;
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("https") {
            443
        } else {
            80
        });
    let timeout = Duration::from_secs(5);
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| anyhow::anyhow!("Failed to resolve the running T3 server"))?;
    let mut stream = None;
    for address in addresses {
        if let Ok(socket) = TcpStream::connect_timeout(&address, timeout) {
            stream = Some(socket);
            break;
        }
    }
    let stream = stream.context("Failed to call the running T3 server")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let scheme = if uri.scheme_str() == Some("https") {
        "wss"
    } else {
        "ws"
    };
    let mut request = format!(
        "{scheme}://{}/ws?orchestrationProtocol=2",
        uri.authority().context("Invalid T3 origin")?
    )
    .into_client_request()?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid T3 bearer token header"))?,
    );
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let (mut socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map_err(|error| match error {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                anyhow::anyhow!(
                    "T3 WebSocket upgrade rejected (HTTP {})",
                    response.status().as_u16()
                )
            }
            _ => anyhow::anyhow!("Failed to connect to the running T3 v2 WebSocket"),
        })?;
    socket.send(Message::Text(json!({"_tag":"Request","id":"1","tag":"orchestration.dispatchCommand","payload":{"type":"thread.metadata.update","commandId":command_id,"threadId":thread_id,"title":title.trim()},"headers":[]}).to_string().into()))
        .map_err(|_| anyhow::anyhow!("Failed to dispatch T3 v2 title command"))?;
    let deadline = std::time::Instant::now() + timeout;
    for _ in 0..64 {
        if std::time::Instant::now() >= deadline {
            bail!("Timed out waiting for T3 v2 dispatch response");
        }
        let message = socket
            .read()
            .map_err(|_| anyhow::anyhow!("Failed to read T3 v2 dispatch response"))?;
        let Message::Text(raw) = message else {
            continue;
        };
        let body: serde_json::Value =
            serde_json::from_str(&raw).context("Invalid T3 v2 RPC response")?;
        let responses = match body {
            serde_json::Value::Array(rows) => rows,
            row => vec![row],
        };
        for response in responses {
            if response["_tag"] == "Defect" {
                bail!("T3 v2 RPC protocol rejected the request");
            }
            if response["_tag"] != "Exit" || response["requestId"] != "1" {
                continue;
            }
            let exit = &response["exit"];
            if exit["_tag"] == "Failure" {
                bail!(
                    "T3 v2 title command rejected: {}",
                    failure_codes(&exit["cause"])
                );
            }
            if exit["_tag"] != "Success" || exit["value"]["sequence"].as_u64().is_none() {
                bail!("Invalid T3 dispatch success response: missing non-negative sequence");
            }
            let _ = socket.close(None);
            return Ok(());
        }
    }
    bail!("T3 v2 dispatch response limit exceeded");
}

fn failure_codes(value: &serde_json::Value) -> String {
    let mut codes = Vec::new();
    fn visit(value: &serde_json::Value, codes: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                for key in ["_tag", "code"] {
                    if let Some(code) = fields.get(key).and_then(|v| v.as_str()) {
                        codes.push(code.to_owned());
                    }
                }
                for (key, child) in fields {
                    if key != "traceId" {
                        visit(child, codes);
                    }
                }
            }
            serde_json::Value::Array(rows) => {
                for row in rows {
                    visit(row, codes);
                }
            }
            _ => {}
        }
    }
    visit(value, &mut codes);
    codes.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    fn runtime(root: &Path, origin: &str) {
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        std::fs::write(
            root.join("userdata/server-runtime.json"),
            json!({"version":1,"origin":origin}).to_string(),
        )
        .unwrap();
    }
    fn server(exit: serde_json::Value) -> (String, thread::JoinHandle<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // Tungstenite requires this concrete, unboxed HTTP error response.
            #[allow(clippy::result_large_err)]
            let mut ws = tungstenite::accept_hdr(
                stream,
                |request: &tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri(), "/ws?orchestrationProtocol=2");
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-secret");
                    Ok(response)
                },
            )
            .unwrap();
            let request: serde_json::Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"_tag":"Exit","requestId":request["id"],"exit":exit})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            request
        });
        (origin, handle)
    }
    #[test]
    fn dispatches_v2_rpc_with_bearer_and_unique_command_id() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let (origin, handle) = server(json!({"_tag":"Success","value":{"sequence":1}}));
            runtime(root.path(), &origin);
            dispatch_with_token(root.path(), "thread-1", " Title ", "fixture-secret").unwrap();
            let request = handle.join().unwrap();
            assert_eq!(request["_tag"], "Request");
            assert_eq!(request["tag"], "orchestration.dispatchCommand");
            assert_eq!(request["payload"]["type"], "thread.metadata.update");
            assert_eq!(request["payload"]["threadId"], "thread-1");
            assert_eq!(request["payload"]["title"], "Title");
            ids.push(request["payload"]["commandId"].clone());
        }
        assert_ne!(ids[0], ids[1]);
    }
    #[test]
    fn rejection_and_malformed_success_are_explicit_without_secrets() {
        let root = tempfile::tempdir().unwrap();
        for exit in [
            json!({"_tag":"Failure","cause":[{"_tag":"Fail","error":{"code":"forbidden","traceId":"fixture-secret"}}]}),
            json!({"_tag":"Success","value":{}}),
            json!({"_tag":"Success","value":{"sequence":-1}}),
        ] {
            let (origin, handle) = server(exit);
            runtime(root.path(), &origin);
            let error = dispatch_with_token(root.path(), "thread", "Title", "fixture-secret")
                .unwrap_err()
                .to_string();
            handle.join().unwrap();
            assert!(error.contains("T3"), "{error}");
            if error.contains("rejected") {
                assert!(error.contains("forbidden"));
            }
            assert!(!error.contains("fixture-secret"), "{error}");
        }
    }
    #[test]
    fn missing_runtime_or_empty_token_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert!(dispatch_with_token(root.path(), "thread", "Title", "token").is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(dispatch_with_token(root.path(), "thread", "Title", " ").is_err());
    }
}
