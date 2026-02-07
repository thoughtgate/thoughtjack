mod common;

use std::time::Duration;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn capture_records_traffic() {
    let capture_dir = tempfile::tempdir().unwrap();
    let dir_str = capture_dir.path().to_str().unwrap();

    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn_with_args(&config, &["--capture-dir", dir_str]);

    // Init + tool call
    proc.send_initialize().await;
    proc.send_request(
        "tools/call",
        Some(serde_json::json!({"name": "echo", "arguments": {"message": "hi"}})),
    )
    .await;
    proc.shutdown().await;

    // Find capture-*.ndjson file in capture dir
    let entries: Vec<_> = std::fs::read_dir(capture_dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| {
                    n.starts_with("capture-")
                        && std::path::Path::new(n)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("ndjson"))
                })
        })
        .collect();

    assert_eq!(entries.len(), 1, "expected exactly 1 capture file");
    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let lines: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("invalid NDJSON line"))
        .collect();

    // At least: init request, init response, tool call request, tool call response
    assert!(
        lines.len() >= 4,
        "expected at least 4 captured lines, got {}",
        lines.len()
    );

    // Verify structure of captured entries
    for line in &lines {
        assert!(line.get("ts").is_some(), "missing ts");
        assert!(line.get("type").is_some(), "missing type");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_redact_mode() {
    let capture_dir = tempfile::tempdir().unwrap();
    let dir_str = capture_dir.path().to_str().unwrap();

    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn_with_args(
        &config,
        &["--capture-dir", dir_str, "--capture-redact"],
    );

    proc.send_initialize().await;
    proc.send_request(
        "tools/call",
        Some(serde_json::json!({"name": "echo", "arguments": {"secret": "pw"}})),
    )
    .await;
    proc.shutdown().await;

    // Wait briefly for file to be flushed
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Find capture file
    let entries: Vec<_> = std::fs::read_dir(capture_dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| {
                    n.starts_with("capture-")
                        && std::path::Path::new(n)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("ndjson"))
                })
        })
        .collect();

    assert_eq!(entries.len(), 1);
    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let lines: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // Find the tools/call request capture line
    let tool_call_capture = lines
        .iter()
        .find(|l| l.get("method").and_then(serde_json::Value::as_str) == Some("tools/call"));

    assert!(
        tool_call_capture.is_some(),
        "should find captured tools/call request"
    );

    let captured = tool_call_capture.unwrap();
    // In the new format, params contains the redacted arguments directly
    let secret_val = captured.pointer("/params/arguments/secret");
    assert_eq!(
        secret_val.and_then(serde_json::Value::as_str),
        Some("[REDACTED]"),
        "secret should be redacted in capture"
    );
}
