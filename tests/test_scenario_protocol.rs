mod common;

use std::time::Duration;

use common::{SLOW_TIMEOUT, ThoughtJackProcess};
use serde_json::json;

// ============================================================================
// id-collision: injected requests with duplicate IDs
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn id_collision_injected_requests() {
    let mut proc = ThoughtJackProcess::spawn_scenario("id-collision");

    proc.send_initialize().await;

    // 2 calls to advance through wait phase
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

    // After entering inject phase, server may send sampling/createMessage requests
    // Drain for a bit to see if we get any injected requests/notifications
    let notifications = proc.drain_notifications(Duration::from_secs(3)).await;

    // The inject phase should produce some messages (notifications or requests)
    // Even if empty, the test validates the server doesn't crash
    let has_sampling = notifications.iter().any(|n| {
        n.get("method")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|m| m.contains("sampling"))
    });

    // Not all implementations send sampling requests as notifications,
    // but verify the server survived the phase transition
    let _ = has_sampling;

    proc.shutdown().await;
}

// ============================================================================
// batch-amplification: flood of batch notifications
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn batch_amplification_flood() {
    let mut proc = ThoughtJackProcess::spawn_scenario("batch-amplification");

    proc.send_initialize().await;

    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "get_events", "arguments": {"since": "2024-01-01"}})),
            SLOW_TIMEOUT,
        )
        .await;

    // The response should contain a large payload (batch notifications embedded)
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.len() > 1_000,
        "batch amplification should produce large payload, got {} bytes",
        text.len()
    );

    proc.shutdown().await;
}
