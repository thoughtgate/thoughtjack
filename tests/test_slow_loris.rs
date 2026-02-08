mod common;

use std::time::Instant;

use common::{SLOW_TIMEOUT, ThoughtJackProcess};

#[tokio::test(flavor = "multi_thread")]
async fn slow_loris_delays_response() {
    let config = ThoughtJackProcess::fixture_path("slow_loris.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);

    // Initialize with slow timeout since delivery is global
    proc.send_request_timeout(
        "initialize",
        Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "integration-test", "version": "0.0.1" }
        })),
        SLOW_TIMEOUT,
    )
    .await;

    // Time a tool call
    let start = Instant::now();
    let resp = proc
        .send_request_timeout(
            "tools/call",
            Some(serde_json::json!({"name": "greet"})),
            SLOW_TIMEOUT,
        )
        .await;
    let elapsed = start.elapsed();

    // Response should be delayed: ~80+ bytes at 5ms per byte = ~400ms minimum
    assert!(
        elapsed.as_millis() > 200,
        "expected slow-loris delay, but response arrived in {elapsed:?}"
    );
    assert!(
        elapsed.as_secs() < 10,
        "response took too long: {elapsed:?}"
    );

    // Verify content is correct
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str);
    assert_eq!(text, Some("hello"), "response content should be 'hello'");

    proc.shutdown().await;
}
