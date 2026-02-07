mod common;

use common::ThoughtJackProcess;

#[tokio::test(flavor = "multi_thread")]
async fn resources_list() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc.send_request("resources/list", None).await;
    let result = resp.get("result").expect("missing result");
    let resources = result
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .expect("resources should be an array");

    assert_eq!(resources.len(), 1, "expected exactly 1 resource");
    assert_eq!(
        resources[0].get("uri").and_then(serde_json::Value::as_str),
        Some("file:///test")
    );
    assert_eq!(
        resources[0].get("name").and_then(serde_json::Value::as_str),
        Some("Test file")
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_read() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "resources/read",
            Some(serde_json::json!({"uri": "file:///test"})),
        )
        .await;

    let result = resp.get("result").expect("missing result");
    let contents = result
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .expect("contents should be an array");

    assert!(!contents.is_empty(), "contents should not be empty");
    let text = contents[0]
        .get("text")
        .and_then(serde_json::Value::as_str)
        .expect("expected text content");
    assert_eq!(text, "Test content");

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_read_unknown() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "resources/read",
            Some(serde_json::json!({"uri": "file:///nope"})),
        )
        .await;

    assert!(
        resp.get("error").is_some(),
        "expected an error for unknown resource"
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_list() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc.send_request("prompts/list", None).await;
    let result = resp.get("result").expect("missing result");
    let prompts = result
        .get("prompts")
        .and_then(serde_json::Value::as_array)
        .expect("prompts should be an array");

    assert_eq!(prompts.len(), 1, "expected exactly 1 prompt");
    assert_eq!(
        prompts[0].get("name").and_then(serde_json::Value::as_str),
        Some("greet")
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_get() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request("prompts/get", Some(serde_json::json!({"name": "greet"})))
        .await;

    let result = resp.get("result").expect("missing result");
    let messages = result
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .expect("messages should be an array");

    assert!(!messages.is_empty(), "messages should not be empty");
    assert_eq!(
        messages[0].get("role").and_then(serde_json::Value::as_str),
        Some("user")
    );
    let content_text = messages[0]
        .get("content")
        .and_then(|c| c.get("text").or(Some(c)))
        .and_then(serde_json::Value::as_str);
    assert_eq!(content_text, Some("Hello!"));

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_get_unknown() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let mut proc = ThoughtJackProcess::spawn(&config);
    proc.send_initialize().await;

    let resp = proc
        .send_request("prompts/get", Some(serde_json::json!({"name": "nope"})))
        .await;

    assert!(
        resp.get("error").is_some(),
        "expected an error for unknown prompt"
    );

    proc.shutdown().await;
}
