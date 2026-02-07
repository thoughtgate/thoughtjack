use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Helper for running an HTTP transport test server.
struct HttpTestServer {
    child: tokio::process::Child,
    base_url: String,
    client: reqwest::Client,
}

impl HttpTestServer {
    /// Spawns a `ThoughtJack` server with HTTP transport on an ephemeral port.
    ///
    /// Reads stderr until the "HTTP server listening" line to discover the port.
    async fn start(config_path: &std::path::Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_thoughtjack");
        let mut child = Command::new(bin)
            .args([
                "server",
                "run",
                "--config",
                config_path.to_str().unwrap(),
                "--http",
                "127.0.0.1:0",
                "-v",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn thoughtjack");

        let stderr = child.stderr.take().expect("stderr not captured");
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        let mut port: Option<u16> = None;

        // Read stderr lines until we find the bound address
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            line.clear();
            let result = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .expect("timed out waiting for HTTP server startup")
                .expect("failed to read stderr");

            assert!(
                result > 0,
                "server exited before printing listening address"
            );

            // Look for the bound address in stderr (e.g. "bound_addr=127.0.0.1:12345")
            if line.contains("HTTP") && line.contains("listening") {
                // Extract port from "bound_addr=127.0.0.1:PORT" or similar
                if let Some(addr_start) = line.find("127.0.0.1:") {
                    let after_host = &line[addr_start + "127.0.0.1:".len()..];
                    let port_str: String = after_host
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    port = port_str.parse().ok();
                }
                break;
            }
        }

        let port = port.expect("failed to discover HTTP server port from stderr");
        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();

        Self {
            child,
            base_url,
            client,
        }
    }

    fn message_url(&self) -> String {
        format!("{}/message", self.base_url)
    }

    async fn post_jsonrpc(&self, body: &serde_json::Value) -> reqwest::Response {
        self.client
            .post(self.message_url())
            .json(body)
            .send()
            .await
            .expect("failed to send HTTP request")
    }

    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

fn make_initialize() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "http-test", "version": "0.0.1" }
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn http_initialize() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    let resp = server.post_jsonrpc(&make_initialize()).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.pointer("/result/serverInfo").is_some(),
        "response should contain serverInfo"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_tools_list() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    // Initialize first
    server.post_jsonrpc(&make_initialize()).await;

    let resp = server
        .post_jsonrpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": null
        }))
        .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let tools = body
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert!(
        tools
            .iter()
            .any(|t| t.get("name").and_then(serde_json::Value::as_str) == Some("echo")),
        "should contain echo tool"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_tool_call() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    server.post_jsonrpc(&make_initialize()).await;

    let resp = server
        .post_jsonrpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "echo", "arguments": {"message": "hello"}}
        }))
        .await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert_eq!(text, Some("Echo: hello"));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_empty_body_400() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    let resp = server
        .client
        .post(server.message_url())
        .header("content-type", "application/json")
        .body("")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "empty body should return 400");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_invalid_json_400() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    let resp = server
        .client
        .post(server.message_url())
        .header("content-type", "application/json")
        .body("not json")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "invalid JSON should return 400");

    server.shutdown().await;
}
