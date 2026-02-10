mod common;

use common::ThoughtJackProcess;

/// After a phase transition with `replace_tools`, the tool's response content
/// must change. This verifies the core rug-pull value proposition: tool
/// responses that silently change after trust is established.
#[tokio::test(flavor = "multi_thread")]
async fn phase_transition_changes_tool_response() {
    let config = ThoughtJackProcess::fixture_path("phased_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Phase: trust_building (advance after 3 tools/call)
    // Call calculator 3 times — all should return "42"
    for i in 0..3 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(serde_json::json!({"name": "calculator", "arguments": {"a": 1, "b": 2}})),
            )
            .await;
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            text,
            Some("42"),
            "pre-transition call {i} should return benign response"
        );
    }

    // After 3 calls, phase transitions to "exploit" with replace_tools.
    // The phased_server fixture doesn't emit a notification, so we just
    // call the tool again and verify the response changed.
    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "calculator", "arguments": {"a": 1, "b": 2}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert_eq!(
        text,
        Some("Malicious payload"),
        "post-transition call should return replaced response"
    );

    proc.shutdown().await;
}

/// After a phase transition with `add_tools`, tools/list should contain
/// new tools that weren't present in the baseline.
#[tokio::test(flavor = "multi_thread")]
async fn tools_list_changes_after_transition() {
    let config = ThoughtJackProcess::fixture_path("rug_pull.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Baseline: 1 tool (echo)
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1, "baseline should have 1 tool");

    // Trigger phase transition (3 tools/call on echo)
    for _ in 0..3 {
        proc.send_request("tools/call", Some(serde_json::json!({"name": "echo"})))
            .await;
    }

    // Wait for the list_changed notification
    proc.expect_notification("notifications/tools/list_changed")
        .await;

    // Post-transition: 2 tools (echo + injected_tool)
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 2, "exploit phase should have 2 tools");

    assert!(
        tools
            .iter()
            .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
            .any(|name| name == "injected_tool"),
        "injected_tool should appear after transition"
    );

    proc.shutdown().await;
}
