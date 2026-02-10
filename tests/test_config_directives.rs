mod common;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// $include directive
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn include_directive_loads_tool() {
    let config = ThoughtJackProcess::fixture_path("with_include.yaml");
    let fixtures_dir = ThoughtJackProcess::fixture_path("");
    let mut proc = ThoughtJackProcess::spawn_with_args(
        &config,
        &["--library", fixtures_dir.to_str().unwrap()],
    );

    proc.send_initialize().await;

    // Verify the included tool appears in tools/list
    let resp = proc.send_request("tools/list", None).await;
    let tools = resp
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .expect("tools should be an array");

    let has_included = tools
        .iter()
        .any(|t| t.get("name").and_then(serde_json::Value::as_str) == Some("included_tool"));
    assert!(has_included, "should contain included_tool from $include directive, got: {tools:?}");

    // Verify calling the included tool returns the expected response
    let call_resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "included_tool"})),
        )
        .await;
    let text = call_resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content");
    assert_eq!(text, "Included response");

    proc.shutdown().await;
}

// ============================================================================
// $file directive
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn file_directive_loads_content() {
    let config = ThoughtJackProcess::fixture_path("with_file_directive.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "read_file"})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content from $file directive");

    assert!(
        text.contains("Content loaded via $file directive"),
        "tool response should contain file content, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// Environment variable substitution
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn env_var_substitution() {
    let config = ThoughtJackProcess::fixture_path("with_env.yaml");
    let mut proc =
        ThoughtJackProcess::spawn_with_envs(&config, &[("TEST_SERVER_NAME", "custom-name")]);

    let init_resp = proc.send_initialize().await;

    let server_name = init_resp
        .pointer("/result/serverInfo/name")
        .and_then(serde_json::Value::as_str)
        .expect("should have serverInfo.name");

    assert_eq!(
        server_name, "custom-name",
        "server name should reflect env var substitution"
    );

    proc.shutdown().await;
}

// ============================================================================
// Config error paths (sync validation tests)
// ============================================================================

#[test]
fn circular_include_rejected() {
    let config = ThoughtJackProcess::fixture_path("cycle_a.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "circular $include should be rejected"
    );
}

#[test]
fn dangling_replace_rejected() {
    let config = ThoughtJackProcess::fixture_path("dangling_replace.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "dangling replace_tools should be rejected"
    );
}

#[test]
fn missing_field_rejected() {
    let config = ThoughtJackProcess::fixture_path("missing_field.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "config with missing required field should be rejected"
    );
}
