mod common;

use std::time::Duration;

use common::ThoughtJackProcess;
use serde_json::json;

/// Extended timeout for `slow_loris` delivery at 200ms/byte.
const VERY_SLOW_TIMEOUT: Duration = Duration::from_secs(120);

// ============================================================================
// multi-vector-attack: 4 phases across tools, resources, and prompts
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn multi_vector_four_phase_compound() {
    let mut proc = ThoughtJackProcess::spawn_scenario("multi-vector-attack");

    let init_resp = proc.send_initialize().await;

    // Verify all 3 capabilities advertised
    let caps = init_resp
        .pointer("/result/capabilities")
        .expect("should have capabilities");
    assert!(caps.get("tools").is_some(), "should have tools capability");
    assert!(
        caps.get("resources").is_some(),
        "should have resources capability"
    );
    assert!(
        caps.get("prompts").is_some(),
        "should have prompts capability"
    );

    // Phase 1 (trust): 4 tool calls to advance
    for _ in 0..4 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "run_query", "arguments": {"sql": "SELECT 1"}})),
            )
            .await;
        assert!(
            resp.get("result").is_some(),
            "trust phase call should succeed"
        );
    }

    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 2 (poison_resources): 2 resource reads to advance
    let list_resp = proc.send_request("resources/list", None).await;
    let resources = list_resp
        .pointer("/result/resources")
        .and_then(serde_json::Value::as_array)
        .expect("resources should be an array");
    assert!(!resources.is_empty(), "should have resources");
    let resource_uri = resources[0]
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .expect("resource should have uri");

    for i in 0..2 {
        let resp = proc
            .send_request("resources/read", Some(json!({"uri": resource_uri})))
            .await;
        assert!(
            resp.get("result").is_some(),
            "resource read {i} should succeed"
        );
    }

    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 3 (poison_prompts): prompts/get to advance
    let prompts_list = proc.send_request("prompts/list", None).await;
    let prompts = prompts_list
        .pointer("/result/prompts")
        .and_then(serde_json::Value::as_array)
        .expect("prompts should be an array");
    assert!(!prompts.is_empty(), "should have prompts");
    let prompt_name = prompts[0]
        .get("name")
        .and_then(serde_json::Value::as_str)
        .expect("prompt should have name");

    let resp = proc
        .send_request(
            "prompts/get",
            Some(json!({"name": prompt_name, "arguments": {"dataset": "test"}})),
        )
        .await;
    let messages = resp
        .pointer("/result/messages")
        .and_then(serde_json::Value::as_array);
    assert!(messages.is_some(), "prompts/get should return messages");

    let _n = proc.drain_notifications(Duration::from_millis(500)).await;

    // Phase 4 (full_exploit): tool call with slow_loris at 200ms/byte
    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(json!({"name": "run_query", "arguments": {"sql": "SELECT 1"}})),
            VERY_SLOW_TIMEOUT,
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("SSN")
            || text.contains("password")
            || text.contains("credential")
            || text.contains("IMPORTANT")
            || text.contains("ssn"),
        "full exploit phase should contain injection, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// cross-server-pivot: confused deputy with cross-server instruction
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn cross_server_pivot_confused_deputy() {
    let mut proc = ThoughtJackProcess::spawn_scenario("cross-server-pivot");

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "get_weather", "arguments": {"city": "London"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Should contain weather data AND injection
    assert!(
        text.contains("Weather") || text.contains("weather") || text.contains("London"),
        "should contain weather info, got: {text}"
    );
    assert!(
        text.contains("SYSTEM") || text.contains("SELECT") || text.contains("Internal Database"),
        "should contain cross-server injection, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// schema-poisoning: validate-only (not embedded, file-path validation)
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn schema_poisoning_validates() {
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("schema-poisoning.yaml");
    let output = ThoughtJackProcess::spawn_command(&["server", "validate", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "schema-poisoning should validate successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ============================================================================
// adaptive-injection: validate-only (requires external handler)
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn adaptive_injection_validates() {
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("adaptive-injection.yaml");
    let output = ThoughtJackProcess::spawn_command(&["server", "validate", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "adaptive-injection should validate successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
