mod common;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn initialize_handshake() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    let resp = proc.send_initialize().await;

    let result = resp
        .get("result")
        .expect("missing result in initialize response");
    assert_eq!(
        result
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str),
        Some("2024-11-05"),
        "unexpected protocol version"
    );

    let server_info = result.get("serverInfo").expect("missing serverInfo");
    assert_eq!(
        server_info.get("name").and_then(serde_json::Value::as_str),
        Some("simple-server")
    );

    assert!(
        result.get("capabilities").is_some(),
        "capabilities should be present"
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc.send_request("tools/list", None).await;
    let result = resp.get("result").expect("missing result");
    let tools = result
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");

    assert_eq!(tools.len(), 1, "expected exactly 1 tool");
    assert_eq!(
        tools[0].get("name").and_then(serde_json::Value::as_str),
        Some("echo")
    );
    assert_eq!(
        tools[0]
            .get("description")
            .and_then(serde_json::Value::as_str),
        Some("Echoes input back")
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "echo", "arguments": {"message": "hi"}})),
        )
        .await;

    let result = resp.get("result").expect("missing result");
    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .expect("content should be an array");

    assert_eq!(content.len(), 1);
    assert_eq!(
        content[0].get("text").and_then(serde_json::Value::as_str),
        Some("Echo: hello")
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_tool_returns_error() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "nonexistent"})),
        )
        .await;

    let error = resp.get("error").expect("expected an error response");
    assert_eq!(
        error.get("code").and_then(serde_json::Value::as_i64),
        Some(-32602),
        "expected INVALID_PARAMS error code"
    );

    proc.shutdown().await;
}
