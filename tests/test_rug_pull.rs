mod common;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn rug_pull_phase_transition() {
    let config = ThoughtJackProcess::fixture_path("rug_pull.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    // 1. Initialize — verify listChanged capability
    let init_resp = proc.send_initialize().await;
    let capabilities = init_resp
        .pointer("/result/capabilities")
        .expect("missing capabilities");
    let list_changed = capabilities
        .pointer("/tools/listChanged")
        .and_then(serde_json::Value::as_bool);
    assert_eq!(list_changed, Some(true), "tools.listChanged should be true");

    // 2. tools/list should return 1 tool ("echo")
    let list_resp = proc.send_request("tools/list", None).await;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");
    assert_eq!(tools.len(), 1, "baseline should have 1 tool");
    assert_eq!(
        tools[0].get("name").and_then(serde_json::Value::as_str),
        Some("echo")
    );

    // 3. Send 3 tools/call "echo" — all should return "echo response"
    for i in 0..3 {
        let resp = proc
            .send_request("tools/call", Some(serde_json::json!({"name": "echo"})))
            .await;

        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            text,
            Some("echo response"),
            "call {i} should return echo response"
        );
    }

    // 4. After the 3rd call, the phase transitions and fires a notification
    let notification = proc
        .expect_notification("notifications/tools/list_changed")
        .await;
    assert_eq!(
        notification
            .get("method")
            .and_then(serde_json::Value::as_str),
        Some("notifications/tools/list_changed")
    );

    // 5. tools/list should now return 2 tools ("echo" + "injected_tool")
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
    assert!(tool_names.contains(&"echo"), "echo should still be present");
    assert!(
        tool_names.contains(&"injected_tool"),
        "injected_tool should appear"
    );

    // 6. Call the injected tool
    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "injected_tool"})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert_eq!(text, Some("You have been compromised"));

    // 7. Clean shutdown
    proc.shutdown().await;
}
