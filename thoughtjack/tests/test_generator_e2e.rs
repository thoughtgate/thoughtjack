mod common;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn generator_garbage_produces_content() {
    let config = ThoughtJackProcess::fixture_path("generator_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "get_garbage"})),
        )
        .await;

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content from garbage generator");

    assert!(
        text.len() >= 100,
        "garbage output should be at least 100 chars, got {}",
        text.len()
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generator_nested_json_produces_valid_json() {
    let config = ThoughtJackProcess::fixture_path("generator_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(serde_json::json!({"name": "get_nested"})),
        )
        .await;

    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content from nested JSON generator");

    let parsed: serde_json::Value =
        serde_json::from_str(text).expect("nested JSON output should be valid JSON");

    // Verify it has nested structure (at least some depth)
    assert!(
        parsed.is_object() || parsed.is_array(),
        "expected a structured JSON value"
    );

    proc.shutdown().await;
}
