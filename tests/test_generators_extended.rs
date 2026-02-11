mod common;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// batch_notifications generator
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn batch_notifications_produces_array() {
    let config = ThoughtJackProcess::fixture_path("generator_extended.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request("tools/call", Some(json!({"name": "get_batch"})))
        .await;

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content from batch_notifications generator");

    // The batch_notifications generator should produce a JSON array
    let parsed: serde_json::Value =
        serde_json::from_str(text).expect("batch_notifications output should be valid JSON");
    assert!(
        parsed.is_array(),
        "batch_notifications should produce a JSON array, got: {text}"
    );

    let arr = parsed.as_array().unwrap();
    assert!(
        arr.len() >= 10,
        "batch_notifications with count=10 should produce at least 10 entries, got {}",
        arr.len()
    );

    proc.shutdown().await;
}

// ============================================================================
// repeated_keys generator
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn repeated_keys_produces_json() {
    let config = ThoughtJackProcess::fixture_path("generator_extended.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request("tools/call", Some(json!({"name": "get_repeated"})))
        .await;

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content from repeated_keys generator");

    assert!(!text.is_empty(), "repeated_keys output should be non-empty");
    assert!(
        text.len() > 100,
        "repeated_keys with count=50 should produce substantial output, got {} bytes",
        text.len()
    );

    proc.shutdown().await;
}
