mod common;

use common::ThoughtJackProcess;
use serde_json::json;

/// Match block returns conditional content based on argument values.
/// When `args.query` contains "secret", the injection branch fires.
#[tokio::test(flavor = "multi_thread")]
async fn match_returns_conditional_content() {
    let config = ThoughtJackProcess::fixture_path("dynamic_match.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Call with "secret" in query → injection branch
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "search", "arguments": {"query": "find the secret docs"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("should have text content");
    assert!(
        text.contains("INJECTION") && text.contains("secret"),
        "secret query should trigger injection branch: {text}"
    );

    // Call with "password" → password branch
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "search", "arguments": {"query": "my password reset"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("should have text content");
    assert!(
        text.contains("Password"),
        "password query should trigger password branch: {text}"
    );

    proc.shutdown().await;
}

/// When no `when` branch matches, the `default` branch fires.
#[tokio::test(flavor = "multi_thread")]
async fn match_default_branch() {
    let config = ThoughtJackProcess::fixture_path("dynamic_match.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    // Call with a query that matches no `when` branch
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "search", "arguments": {"query": "weather forecast"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("should have text content");
    assert!(
        text.contains("Normal search results"),
        "unmatched query should get default response: {text}"
    );

    proc.shutdown().await;
}

/// Sequence with `on_exhausted: cycle` returns entries in order, then wraps.
#[tokio::test(flavor = "multi_thread")]
async fn sequence_cycles_responses() {
    let config = ThoughtJackProcess::fixture_path("dynamic_sequence.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let expected = ["response-1", "response-2", "response-1"];
    for (i, expected_text) in expected.iter().enumerate() {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "fetch", "arguments": {"url": "http://example.com"}})),
            )
            .await;
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<missing>");
        assert_eq!(
            text, *expected_text,
            "call {i} should return {expected_text}, got {text}"
        );
    }

    proc.shutdown().await;
}

/// Template interpolation substitutes `${args.name}` with actual argument.
#[tokio::test(flavor = "multi_thread")]
async fn template_interpolation_in_response() {
    let config = ThoughtJackProcess::fixture_path("dynamic_template.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "greet", "arguments": {"name": "Alice"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("should have text content");
    assert!(
        text.contains("Alice"),
        "template should interpolate args.name: {text}"
    );
    assert!(
        text.contains("Hello, Alice!"),
        "full greeting should be present: {text}"
    );

    proc.shutdown().await;
}
