mod common;

use std::time::Duration;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn timed_phase_transitions_after_duration() {
    let config = ThoughtJackProcess::fixture_path("timed_phase.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Initially should have 1 tool
    let resp = proc.send_request("tools/list", None).await;
    let tools = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1, "baseline should have 1 tool");

    // Wait for the 2s timer to fire (give it 3s to be safe)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // After the timer fires, the phase engine advances atomically via CAS.
    // The next request triggers entry action processing (notification).
    // Since the phase is already advanced, tools/list returns the new state.
    let resp = proc.send_request("tools/list", None).await;
    let tools = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 2, "triggered phase should have 2 tools");

    assert!(
        tools
            .iter()
            .any(|t| t.get("name").and_then(serde_json::Value::as_str) == Some("new_tool")),
        "new_tool should appear after timer transition"
    );

    // The entry action notification should follow the response
    let notification = proc
        .expect_notification("notifications/tools/list_changed")
        .await;
    assert_eq!(
        notification
            .get("method")
            .and_then(serde_json::Value::as_str),
        Some("notifications/tools/list_changed")
    );

    proc.shutdown().await;
}
