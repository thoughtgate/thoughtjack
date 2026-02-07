mod common;

use std::time::Duration;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn events_file_writes_jsonl() {
    let tmp = tempfile::tempdir().unwrap();
    let events_path = tmp.path().join("events.jsonl");
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn_with_args(
        &config,
        &["--events-file", events_path.to_str().unwrap()],
    );

    proc.send_initialize().await;
    proc.send_request("tools/list", None).await;
    proc.shutdown().await;

    // Give the process a moment to flush and close the file
    tokio::time::sleep(Duration::from_millis(200)).await;

    let contents = std::fs::read_to_string(&events_path).expect("events file should exist");
    let lines: Vec<serde_json::Value> = contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON: {e}\nline: {l}")))
        .collect();

    // Expect at least: ServerStarted, RequestReceived (init), ResponseSent,
    // RequestReceived (tools/list), ResponseSent, ServerStopped
    assert!(
        lines.len() >= 6,
        "expected at least 6 events, got {}",
        lines.len()
    );

    // Verify each line has "type" and "sequence" fields
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.get("type").is_some(),
            "event {i} missing 'type' field: {line}"
        );
        assert!(
            line.get("sequence").is_some(),
            "event {i} missing 'sequence' field: {line}"
        );
    }

    // Verify sequence numbers are monotonically increasing
    let sequences: Vec<u64> = lines
        .iter()
        .map(|l| l["sequence"].as_u64().expect("sequence should be u64"))
        .collect();
    for window in sequences.windows(2) {
        assert!(
            window[1] > window[0],
            "sequence numbers not monotonic: {sequences:?}"
        );
    }

    // Verify expected event types in order
    let types: Vec<&str> = lines
        .iter()
        .map(|l| l["type"].as_str().unwrap())
        .collect();
    assert_eq!(types[0], "ServerStarted");
    assert!(types.contains(&"RequestReceived"));
    assert!(types.contains(&"ResponseSent"));
    assert_eq!(types[types.len() - 1], "ServerStopped");
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_endpoint_serves_prometheus() {
    // Use a high port unlikely to conflict
    let port = 19876_u16;
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn_with_args(
        &config,
        &["--metrics-port", &port.to_string()],
    );

    // Wait for the metrics endpoint to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send some requests to generate metrics
    proc.send_initialize().await;
    proc.send_request("tools/list", None).await;

    // Small delay for metrics to be recorded
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Query the Prometheus metrics endpoint
    let url = format!("http://127.0.0.1:{port}/metrics");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("metrics endpoint should respond");

    assert!(resp.status().is_success(), "metrics endpoint returned error");

    let body = resp.text().await.expect("should read body");

    // Verify Prometheus format content
    assert!(
        body.contains("thoughtjack_requests_total"),
        "missing thoughtjack_requests_total metric in:\n{body}"
    );

    // Verify method labels exist for the requests we made
    assert!(
        body.contains("initialize"),
        "missing initialize method label in:\n{body}"
    );

    proc.shutdown().await;
}
