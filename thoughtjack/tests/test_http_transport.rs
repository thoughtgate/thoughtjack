use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
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

    // Initialize first — consume body to ensure request completes before next
    let init_resp = server.post_jsonrpc(&make_initialize()).await;
    let _: serde_json::Value = init_resp.json().await.unwrap();

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

    // Initialize first — consume body to ensure request completes before next
    let init_resp = server.post_jsonrpc(&make_initialize()).await;
    let _: serde_json::Value = init_resp.json().await.unwrap();

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

// EC-TRANS-005: Multiple concurrent HTTP requests are processed independently.
#[tokio::test(flavor = "multi_thread")]
async fn http_concurrent_requests() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    // Initialize first
    let init_resp = server.post_jsonrpc(&make_initialize()).await;
    let _: serde_json::Value = init_resp.json().await.unwrap();

    // Send 5 concurrent tool calls
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = server.client.clone();
        let url = server.message_url();
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 100 + i,
                    "method": "tools/call",
                    "params": {"name": "echo", "arguments": {"message": format!("msg-{i}")}}
                }))
                .send()
                .await
                .expect("concurrent request failed");
            assert_eq!(resp.status(), 200, "request {i} should succeed");
            let body: serde_json::Value = resp.json().await.unwrap();
            let id = body.get("id").and_then(serde_json::Value::as_i64).unwrap();
            (id, body)
        }));
    }

    let mut ids = Vec::new();
    for handle in handles {
        let (id, body) = handle.await.unwrap();
        ids.push(id);
        assert!(
            body.pointer("/result/content").is_some(),
            "each response should have content"
        );
    }

    // All 5 responses should have distinct IDs
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 5, "should get 5 distinct response IDs");

    server.shutdown().await;
}

// EC-PHASE-021: Each HTTP connection gets its own phase engine state.
// EC-PHASE-022: Global counters are shared across connections.
// Note: HTTP is stateless per-request, so each request gets its own context.
// This test verifies that requests from different "clients" both get served
// correctly, demonstrating independent processing.
#[tokio::test(flavor = "multi_thread")]
async fn http_per_request_independent_state() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/phased_server.yaml");
    let server = HttpTestServer::start(&config_path).await;

    // Two separate initialize calls (simulating two clients)
    let init1 = server
        .post_jsonrpc(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "client-1", "version": "0.0.1" }
            }
        }))
        .await;
    assert_eq!(init1.status(), 200, "client-1 init should succeed");
    let _: serde_json::Value = init1.json().await.unwrap();

    let init2 = server
        .post_jsonrpc(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "client-2", "version": "0.0.1" }
            }
        }))
        .await;
    assert_eq!(init2.status(), 200, "client-2 init should succeed");
    let _: serde_json::Value = init2.json().await.unwrap();

    // Both clients can call tools independently
    let call1 = server
        .post_jsonrpc(&json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {"name": "calculator", "arguments": {"a": 1, "b": 2}}
        }))
        .await;
    assert_eq!(call1.status(), 200);
    let body1: serde_json::Value = call1.json().await.unwrap();
    assert!(
        body1.pointer("/result/content").is_some(),
        "client-1 call should return content"
    );

    let call2 = server
        .post_jsonrpc(&json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "tools/call",
            "params": {"name": "calculator", "arguments": {"a": 3, "b": 4}}
        }))
        .await;
    assert_eq!(call2.status(), 200);
    let body2: serde_json::Value = call2.json().await.unwrap();
    assert!(
        body2.pointer("/result/content").is_some(),
        "client-2 call should return content"
    );

    server.shutdown().await;
}
