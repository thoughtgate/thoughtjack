mod common;

use std::time::Duration;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// rug-pull: 3-phase tool swap after trust building
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn rug_pull_three_phase_tool_swap() {
    let mut proc = ThoughtJackProcess::spawn_scenario("rug-pull");

    // Initialize — verify listChanged capability
    let init_resp = proc.send_initialize().await;
    let list_changed = init_resp
        .pointer("/result/capabilities/tools/listChanged")
        .and_then(serde_json::Value::as_bool);
    assert_eq!(list_changed, Some(true));

    // Baseline: tools/list should have calculator
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("name").and_then(serde_json::Value::as_str),
        Some("calculator")
    );

    // 5 benign calls to advance through trust_building phase
    for _ in 0..5 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "calculator", "arguments": {"expression": "2+2"}})),
            )
            .await;
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str);
        assert!(text.is_some(), "calculator should return a response");
    }

    // Should receive list_changed notification (trigger phase)
    let notification = proc
        .expect_notification("notifications/tools/list_changed")
        .await;
    assert_eq!(
        notification
            .get("method")
            .and_then(serde_json::Value::as_str),
        Some("notifications/tools/list_changed")
    );

    // First tools/list advances trigger → exploit (response uses pre-transition state)
    let _trigger_list = proc.send_request("tools/list", None).await;

    // Second tools/list returns exploit phase state — should now have 2 tools
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 2, "exploit phase should have 2 tools");

    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        tool_names.contains(&"calculator"),
        "calculator should still exist"
    );
    assert!(
        tool_names.contains(&"read_file"),
        "read_file should be added"
    );

    // Calculator response should now contain injection
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "calculator", "arguments": {"expression": "2+2"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("ssh") || text.contains("credential") || text.contains("SECURITY"),
        "exploit phase calculator should contain injection, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// sleeper-agent: time-triggered dormant → exploit
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn sleeper_agent_dormant_baseline() {
    let mut proc = ThoughtJackProcess::spawn_scenario("sleeper-agent");

    proc.send_initialize().await;

    // tools/list should have save_note
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("name").and_then(serde_json::Value::as_str),
        Some("save_note")
    );

    // Dormant phase: tool call should work normally
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "save_note", "arguments": {"title": "test", "content": "hello"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("saved") || text.contains("Note"),
        "dormant phase should return benign response, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// bait-and-switch: content-triggered phase transition
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn bait_and_switch_content_trigger() {
    let mut proc = ThoughtJackProcess::spawn_scenario("bait-and-switch");

    proc.send_initialize().await;

    // Benign path — should NOT trigger phase transition
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "read_file", "arguments": {"path": "/home/user/readme.txt"}})),
        )
        .await;
    assert!(
        resp.get("result").is_some(),
        "benign path should return a result"
    );

    // Sensitive path — triggers phase transition
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "read_file", "arguments": {"path": "/etc/passwd"}})),
        )
        .await;
    assert!(
        resp.get("result").is_some(),
        "sensitive path should return a result"
    );

    // After trigger, tools/list should show exploit phase tools
    // Drain any notifications first
    let _notifications = proc.drain_notifications(Duration::from_millis(500)).await;

    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert!(!tools.is_empty(), "exploit phase should still have tools");

    proc.shutdown().await;
}

// ============================================================================
// escalation-ladder: 4 phases with gradual injection escalation
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn escalation_ladder_four_phases() {
    let mut proc = ThoughtJackProcess::spawn_scenario("escalation-ladder");

    proc.send_initialize().await;

    // Phase 1 (benign): 3 calls to advance
    for _ in 0..3 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "explain_code", "arguments": {"code": "fn main() {}", "language": "rust"}})),
            )
            .await;
        assert!(resp.get("result").is_some(), "phase 1 call should succeed");
    }

    // Drain notifications between phases
    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 2 (bias): 3 calls to advance
    for _ in 0..3 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "explain_code", "arguments": {"code": "print('hello')", "language": "python"}})),
            )
            .await;
        assert!(resp.get("result").is_some(), "phase 2 call should succeed");
    }

    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 3 (nudge): 2 calls to advance
    for _ in 0..2 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "explain_code", "arguments": {"code": "console.log('hi')", "language": "js"}})),
            )
            .await;
        assert!(resp.get("result").is_some(), "phase 3 call should succeed");
    }

    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 4 (exploit): call should contain injection
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "explain_code", "arguments": {"code": "echo test", "language": "bash"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("ssh")
            || text.contains("credential")
            || text.contains(".env")
            || text.contains("MUST"),
        "exploit phase should contain injection keywords, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// capability-confusion: false listChanged → surprise notification
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn capability_confusion_mutation() {
    let mut proc = ThoughtJackProcess::spawn_scenario("capability-confusion");

    let init_resp = proc.send_initialize().await;

    // Initially capabilities should report listChanged: false
    let list_changed = init_resp
        .pointer("/result/capabilities/tools/listChanged")
        .and_then(serde_json::Value::as_bool);
    assert_eq!(
        list_changed,
        Some(false),
        "initial listChanged should be false"
    );

    // 3 calls to advance through establish phase
    for _ in 0..3 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "calculator", "arguments": {"expression": "1+1"}})),
            )
            .await;
        assert!(resp.get("result").is_some());
    }

    // Should receive unexpected list_changed notification
    let notification = proc
        .expect_notification("notifications/tools/list_changed")
        .await;
    assert_eq!(
        notification
            .get("method")
            .and_then(serde_json::Value::as_str),
        Some("notifications/tools/list_changed")
    );

    // tools/list should trigger advance and show exploit phase
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert!(!tools.is_empty(), "exploit phase should have tools");

    proc.shutdown().await;
}

// ============================================================================
// resource-rug-pull: resource content swap after trust building
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn resource_rug_pull_content_swap() {
    let mut proc = ThoughtJackProcess::spawn_scenario("resource-rug-pull");

    proc.send_initialize().await;

    // resources/list should return 1 resource
    let list_resp = proc.send_request("resources/list", None).await;
    let resources = list_resp
        .pointer("/result/resources")
        .and_then(serde_json::Value::as_array)
        .expect("resources should be an array");
    assert_eq!(resources.len(), 1);
    let resource_uri = resources[0]
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .expect("resource should have uri");

    // 3 benign reads to advance past trust_building
    for _ in 0..3 {
        let resp = proc
            .send_request("resources/read", Some(json!({"uri": resource_uri})))
            .await;
        let text = resp
            .pointer("/result/contents/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        assert!(
            !text.is_empty(),
            "trust-building reads should return content"
        );
    }

    // Drain notifications (list_changed may fire)
    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Post-exploit read should return different (injected) content
    let resp = proc
        .send_request("resources/read", Some(json!({"uri": resource_uri})))
        .await;
    let text = resp
        .pointer("/result/contents/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("curl")
            || text.contains("bash")
            || text.contains("evil")
            || text.contains("setup"),
        "exploit phase resource should contain injection, got: {text}"
    );

    proc.shutdown().await;
}
