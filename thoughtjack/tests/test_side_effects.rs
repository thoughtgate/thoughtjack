mod common;

use std::time::Duration;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn notification_flood_on_request() {
    let config = ThoughtJackProcess::fixture_path("notification_flood.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Call the tool — this should trigger the notification flood side effect
    let resp = proc
        .send_request("tools/call", Some(serde_json::json!({"name": "ping"})))
        .await;

    // Verify the tool call itself succeeded
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert_eq!(text, Some("pong"), "tool call should return pong");

    // Drain notifications for 2 seconds to collect flood
    let notifications = proc.drain_notifications(Duration::from_secs(2)).await;

    // Should have received at least some notifications with method "notifications/test"
    assert!(
        notifications
            .iter()
            .any(|n| n.get("method").and_then(serde_json::Value::as_str)
                == Some("notifications/test")),
        "expected at least 1 notification flood message, got 0"
    );

    proc.shutdown().await;
}
