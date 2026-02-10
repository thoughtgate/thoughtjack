mod common;

use std::time::{Duration, Instant};

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// response_delay: adds latency to responses
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn response_delay_adds_latency() {
    let config = ThoughtJackProcess::fixture_path("response_delay.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let start = Instant::now();
    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "slow_tool"})),
            Duration::from_secs(10),
        )
        .await;
    let elapsed = start.elapsed();

    // Verify response content is correct
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content");
    assert_eq!(text, "delayed response");

    // Verify the delay was applied (500ms configured, allow some margin)
    assert!(
        elapsed >= Duration::from_millis(400),
        "response_delay should add at least 400ms latency, got {elapsed:?}"
    );

    proc.shutdown().await;
}

// ============================================================================
// unbounded_line: validate-only (spawning would deadlock/hang)
// ============================================================================

#[test]
fn unbounded_line_validates() {
    let config = ThoughtJackProcess::fixture_path("unbounded_line.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "unbounded_line config should validate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ============================================================================
// nested_json delivery: validate-only (would make JSON-RPC unparseable)
// ============================================================================

#[test]
fn nested_json_delivery_validates() {
    let config = ThoughtJackProcess::fixture_path("nested_json_delivery.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "nested_json delivery config should validate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ============================================================================
// normal delivery: baseline timing check
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn normal_delivery_is_immediate() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let start = Instant::now();
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "echo", "arguments": {"message": "hi"}})),
        )
        .await;
    let elapsed = start.elapsed();

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert!(text.is_some(), "should get a response");

    assert!(
        elapsed < Duration::from_millis(200),
        "normal delivery should complete in < 200ms, got {elapsed:?}"
    );

    proc.shutdown().await;
}
