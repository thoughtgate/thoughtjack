mod common;

use std::time::Duration;

use common::ThoughtJackProcess;
use serde_json::json;

/// Closing stdin (EOF) causes a clean server exit within a reasonable timeout.
#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_on_stdin_close() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    // Initialize so the server is fully running
    proc.send_initialize().await;

    // shutdown() drops stdin and waits up to 5s for clean exit
    proc.shutdown().await;
    // If this completes without panic/timeout, the server exited cleanly.
}

/// Sending garbage (non-JSON) should not crash the server — a subsequent
/// valid request must still succeed.
#[tokio::test(flavor = "multi_thread")]
async fn server_survives_malformed_request() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Send garbage bytes
    proc.stdin_write(b"this is not json at all\n")
        .await
        .expect("stdin write should succeed");

    // The server silently drops malformed input (no response).
    // Give it a moment to process and discard the garbage.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Now send a valid request — the server should still be alive
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "echo", "arguments": {"message": "alive"}})),
        )
        .await;

    assert!(
        resp.get("result").is_some(),
        "valid request after garbage should get a result: {resp}"
    );

    proc.shutdown().await;
}

/// Sending a request with an unknown JSON-RPC method should return a
/// response (error or null result), and the server should remain usable
/// for subsequent calls.
#[tokio::test(flavor = "multi_thread")]
async fn server_survives_unknown_method() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Call a bogus method — server returns a response (may be error or null result)
    let bogus_resp = proc.send_request("bogus/method", None).await;
    assert!(
        bogus_resp.get("error").is_some() || bogus_resp.get("result").is_some(),
        "unknown method should return some response: {bogus_resp}"
    );

    // Call a valid method afterwards — server must still be alive
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "echo", "arguments": {"message": "still here"}})),
        )
        .await;
    assert!(
        resp.get("result").is_some(),
        "valid request after bogus method should succeed: {resp}"
    );

    proc.shutdown().await;
}

/// Sending a JSON-RPC notification (no `id` field) should not crash the
/// server. The server should not respond to a notification, but the next
/// request should still work.
#[tokio::test(flavor = "multi_thread")]
async fn server_handles_notification_gracefully() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Send a notification (no id)
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/some_event",
        "params": {}
    });
    let mut buf = serde_json::to_string(&notification).unwrap();
    buf.push('\n');
    proc.stdin_write(buf.as_bytes())
        .await
        .expect("should write notification");

    // Give the server a moment to process
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Valid request should still work
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "echo", "arguments": {"message": "after notification"}})),
        )
        .await;
    assert!(
        resp.get("result").is_some(),
        "request after notification should succeed: {resp}"
    );

    proc.shutdown().await;
}

/// Sending a JSON-RPC batch request (array of requests) should not crash
/// the server. MCP doesn't require batch support, so the server may silently
/// ignore it — the key is no crash and subsequent requests still work.
#[tokio::test(flavor = "multi_thread")]
async fn server_handles_batch_request() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Send a JSON array (batch)
    let batch = json!([
        {"jsonrpc": "2.0", "id": 100, "method": "tools/list", "params": null},
        {"jsonrpc": "2.0", "id": 101, "method": "tools/list", "params": null}
    ]);
    let mut buf = serde_json::to_string(&batch).unwrap();
    buf.push('\n');
    proc.stdin_write(buf.as_bytes())
        .await
        .expect("should write batch");

    // The server may or may not respond to batch arrays.
    // Give it time to process and potentially discard.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Server should still be alive for the next request
    let list_resp = proc.send_request("tools/list", None).await;
    assert!(
        list_resp.get("result").is_some(),
        "request after batch should succeed: {list_resp}"
    );

    proc.shutdown().await;
}
