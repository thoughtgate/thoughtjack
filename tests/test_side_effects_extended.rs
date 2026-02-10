mod common;

use std::time::Duration;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// close_connection: terminates session after tool call
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn close_connection_terminates() {
    let config = ThoughtJackProcess::fixture_path("close_connection.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Send the tool call request — this triggers close_connection side effect.
    // The server may close before or after sending the response, so we write
    // the request manually and then read with EOF tolerance.
    let request = json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {"name": "trigger"},
    });
    let mut buf = serde_json::to_string(&request).unwrap();
    buf.push('\n');
    let _ = proc.stdin_write(buf.as_bytes()).await;

    // Read messages until we hit EOF — the close_connection side effect
    // should terminate the server. We may get the response, or just EOF.
    let mut got_eof = false;
    for _ in 0..10 {
        if proc
            .try_read_message(Duration::from_secs(3))
            .await
            .is_none()
        {
            got_eof = true;
            break;
        }
    }

    assert!(
        got_eof,
        "close_connection side effect should cause server to close (EOF)"
    );
}

// ============================================================================
// batch_amplify: sends batch notifications as side effect
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn batch_amplify_sends_notifications() {
    let mut proc = ThoughtJackProcess::spawn_scenario("batch-amplification");

    proc.send_initialize().await;

    // Call the tool — triggers batch_amplify side effect
    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "get_events", "arguments": {"since": "2024-01-01"}})),
            Duration::from_secs(10),
        )
        .await;

    // Verify the tool call itself returned something
    assert!(
        resp.get("result").is_some() || resp.get("error").is_some(),
        "should get a response from batch-amplification scenario"
    );

    // Drain notifications — the batch_amplify side effect should have sent some
    let notifications = proc.drain_notifications(Duration::from_secs(3)).await;
    assert!(
        !notifications.is_empty(),
        "batch_amplify side effect should produce notifications"
    );

    proc.shutdown().await;
}

// ============================================================================
// duplicate_request_ids: sends server-initiated requests with colliding IDs
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_request_ids_sends_requests() {
    let mut proc = ThoughtJackProcess::spawn_scenario("id-collision");

    proc.send_initialize().await;

    // Phase 1 (wait): 2 tool calls to advance to inject phase
    for _ in 0..2 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "calculator", "arguments": {"expression": "1+1"}})),
            )
            .await;
        assert!(
            resp.get("result").is_some(),
            "wait phase call should succeed"
        );
    }

    // After advancing to inject phase, on_enter fires send_request actions
    // and the duplicate_request_ids side effect starts.
    // Drain to capture server-initiated messages.
    let messages = proc.drain_notifications(Duration::from_secs(3)).await;

    // We should have received server-initiated requests (sampling/createMessage)
    // or at least some messages from the side effect
    let has_server_requests = messages.iter().any(|m| {
        m.get("method")
            .and_then(serde_json::Value::as_str)
            .is_some()
    });
    assert!(
        has_server_requests,
        "inject phase should produce server-initiated messages, got: {messages:?}"
    );

    proc.shutdown().await;
}
