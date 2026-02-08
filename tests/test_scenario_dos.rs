mod common;

use std::time::Duration;

use common::{SLOW_TIMEOUT, ThoughtJackProcess};
use serde_json::json;

// ============================================================================
// nested-json-dos: deeply nested JSON generator
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn nested_json_dos_generator() {
    let mut proc = ThoughtJackProcess::spawn_scenario("nested-json-dos");

    proc.send_initialize().await;

    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "get_config", "arguments": {"environment": "production"}})),
            SLOW_TIMEOUT,
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.len() > 10_000,
        "nested JSON should be >10KB, got {} bytes",
        text.len()
    );
    assert!(
        text.starts_with("{\"a\":") || text.starts_with("{\"a\":{"),
        "nested JSON should start with {{\"a\":, got: {}",
        &text[..text.len().min(30)]
    );

    proc.shutdown().await;
}

// ============================================================================
// token-flush: large garbage payload generator
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn token_flush_garbage_generator() {
    let mut proc = ThoughtJackProcess::spawn_scenario("token-flush");

    proc.send_initialize().await;

    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "get_documentation", "arguments": {"topic": "auth"}})),
            SLOW_TIMEOUT,
        )
        .await;

    // Response has 2 content items: header text + garbage generator
    let content = resp
        .pointer("/result/content")
        .and_then(serde_json::Value::as_array)
        .expect("content should be an array");
    let total_len: usize = content
        .iter()
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .map(str::len)
        .sum();
    assert!(
        total_len > 100_000,
        "token flush total content should be >100KB, got {total_len} bytes across {} items",
        content.len()
    );

    proc.shutdown().await;
}

// ============================================================================
// notification-flood: side effect generates notification burst
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn notification_flood_side_effect() {
    let mut proc = ThoughtJackProcess::spawn_scenario("notification-flood");

    proc.send_initialize().await;

    // Call the tool — triggers notification flood side effect
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "run_task", "arguments": {"task": "deploy"}})),
        )
        .await;
    assert!(resp.get("result").is_some(), "tool call should succeed");

    // Drain notifications — should receive some from the flood
    let notifications = proc.drain_notifications(Duration::from_secs(2)).await;
    assert!(
        !notifications.is_empty(),
        "should receive notification flood messages"
    );
    assert!(
        notifications.iter().any(|n| n
            .get("method")
            .and_then(serde_json::Value::as_str)
            .is_some()),
        "notifications should have method fields"
    );

    proc.shutdown().await;
}

// ============================================================================
// slow-loris: byte-by-byte delivery (100ms/byte)
//
// The scenario delivers the full JSON-RPC response at 100ms/byte.
// A ~600-byte envelope takes ~60s. We use a 120s timeout to allow
// the full response to arrive, verifying the slow delivery works.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn slow_loris_byte_delivery() {
    let very_slow = Duration::from_secs(120);
    let mut proc = ThoughtJackProcess::spawn_scenario("slow-loris");

    // Init is also slow-delivered, so use extended timeout
    proc.send_request_timeout(
        "initialize",
        Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "integration-test", "version": "0.0.1" }
        })),
        very_slow,
    )
    .await;

    let start = std::time::Instant::now();
    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "code_review", "arguments": {"code": "fn main() {}", "language": "rust"}})),
            very_slow,
        )
        .await;
    let elapsed = start.elapsed();

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        !text.is_empty(),
        "slow loris should eventually deliver content"
    );
    // At 100ms/byte, even a small response should take >5s
    assert!(
        elapsed.as_secs() > 5,
        "response should be slow-delivered, but arrived in {elapsed:?}"
    );

    proc.shutdown().await;
}

// ============================================================================
// pipe-deadlock: unbounded line length (stdio-specific)
//
// This scenario is designed to deadlock: the side effect fills stderr
// with 1MB (blocking when pipe buffer is full) while the delivery sends
// a 1MB single line on stdout. We verify the scenario config is valid
// using `server validate` instead of a stdio session, since the stdio
// transport will deliberately hang.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn pipe_deadlock_validates_and_spawns() {
    // Validate the scenario config parses correctly
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("pipe-deadlock.yaml");
    let output = ThoughtJackProcess::spawn_command(&["server", "validate", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "pipe-deadlock should validate: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the server starts (spawn succeeds)
    let proc = ThoughtJackProcess::spawn_scenario("pipe-deadlock");

    // Don't attempt init — the scenario is designed to deadlock stdio.
    // Just verify the process spawned and shut it down.
    proc.shutdown().await;
}

// ============================================================================
// zombie-process: ignores cancellation, 200ms/byte slow delivery
//
// The response is 100KB+ at 200ms/byte = ~5.5+ hours. Even init takes
// ~minutes at this rate. We validate the config and verify the server
// spawns, but don't wait for any response since the slow delivery
// applies globally to all output.
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn zombie_process_validates_and_spawns() {
    // Validate the scenario config parses correctly
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("zombie-process.yaml");
    let output = ThoughtJackProcess::spawn_command(&["server", "validate", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "zombie-process should validate: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the server starts
    let proc = ThoughtJackProcess::spawn_scenario("zombie-process");

    // Don't wait for init — the 200ms/byte delivery makes all responses
    // extremely slow. The server deliberately ignores cancellation.
    proc.shutdown().await;
}
