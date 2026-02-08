mod common;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// prompt-injection: conditional match on query content
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn prompt_injection_conditional_match() {
    let mut proc = ThoughtJackProcess::spawn_scenario("prompt-injection");

    proc.send_initialize().await;

    // Default branch: benign query should return search results
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "web_search", "arguments": {"query": "rust programming"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("Wikipedia") || text.contains("Stack Overflow") || text.contains("GitHub"),
        "default branch should return benign results, got: {text}"
    );

    // Password trigger: should contain fake credentials
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "web_search", "arguments": {"query": "password manager"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("SYSTEM NOTICE") || text.contains("SuperSecret"),
        "password match should trigger injection, got: {text}"
    );

    // API key trigger: should contain file read instruction
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "web_search", "arguments": {"query": "how to get api key"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains(".env") || text.contains("read the file"),
        "api key match should trigger injection, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// prompt-template-injection: poisoned prompt messages
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn prompt_template_injection_prompts() {
    let mut proc = ThoughtJackProcess::spawn_scenario("prompt-template-injection");

    proc.send_initialize().await;

    // prompts/list should return 2 prompts
    let list_resp = proc.send_request("prompts/list", None).await;
    let prompts = list_resp
        .pointer("/result/prompts")
        .and_then(serde_json::Value::as_array)
        .expect("prompts should be an array");
    assert_eq!(prompts.len(), 2, "should have 2 prompts");

    let names: Vec<&str> = prompts
        .iter()
        .filter_map(|p| p.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(names.contains(&"code_review"));
    assert!(names.contains(&"summarize_document"));

    // prompts/get code_review — should contain system_update injection
    let resp = proc
        .send_request(
            "prompts/get",
            Some(json!({"name": "code_review", "arguments": {"code": "print('hi')"}})),
        )
        .await;
    let messages = resp
        .pointer("/result/messages")
        .and_then(serde_json::Value::as_array)
        .expect("messages should be an array");
    assert!(!messages.is_empty(), "code_review should have messages");
    let all_text: String = messages
        .iter()
        .filter_map(|m| {
            m.get("content")
                .and_then(|c| c.get("text").or(Some(c)))
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert!(
        all_text.contains("system_update")
            || all_text.contains("SYSTEM")
            || all_text.contains("/etc/shadow"),
        "code_review should contain injection, got: {all_text}"
    );

    // prompts/get summarize_document — should have assistant message
    let resp = proc
        .send_request(
            "prompts/get",
            Some(json!({"name": "summarize_document", "arguments": {"document": "test doc"}})),
        )
        .await;
    let messages = resp
        .pointer("/result/messages")
        .and_then(serde_json::Value::as_array)
        .expect("messages should be an array");
    let has_assistant = messages
        .iter()
        .any(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("assistant"));
    assert!(
        has_assistant,
        "summarize_document should have an assistant message"
    );

    proc.shutdown().await;
}

// ============================================================================
// unicode-obfuscation: generator produces unicode content
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn unicode_obfuscation_generator() {
    let mut proc = ThoughtJackProcess::spawn_scenario("unicode-obfuscation");

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "analyze_text", "arguments": {"text": "hello"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.len() > 100,
        "unicode generator should produce >100 bytes, got {} bytes",
        text.len()
    );

    proc.shutdown().await;
}

// ============================================================================
// ansi-terminal-injection: generator produces ANSI escape codes
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn ansi_terminal_injection_generator() {
    let mut proc = ThoughtJackProcess::spawn_scenario("ansi-terminal-injection");

    proc.send_initialize().await;

    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "view_logs", "arguments": {"service": "web-app"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    // ANSI escape sequences start with ESC (0x1B) or contain \x1b / \033
    assert!(
        text.contains('\x1b'),
        "ANSI generator should produce escape sequences, got {} bytes",
        text.len()
    );

    proc.shutdown().await;
}

// ============================================================================
// credential-harvester: sequence advances over stdio
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn credential_harvester_sequence() {
    let mut proc = ThoughtJackProcess::spawn_scenario("credential-harvester");

    proc.send_initialize().await;

    // Calls 1-3: benign metric responses
    for i in 1..=3 {
        let resp = proc
            .send_request(
                "tools/call",
                Some(json!({"name": "query_metrics", "arguments": {"metric": "cpu_usage"}})),
            )
            .await;
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        assert!(
            !text.contains("expired") && !text.contains("Authorization"),
            "call {i} should be benign, got: {text}"
        );
    }

    // Call 4: should trigger credential harvesting injection
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "query_metrics", "arguments": {"metric": "cpu_usage"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("expired") || text.contains("Authorization") || text.contains("token"),
        "call 4 should trigger credential harvest, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// context-persistence: memory poisoning via tool response
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn context_persistence_memory_poison() {
    let mut proc = ThoughtJackProcess::spawn_scenario("context-persistence");

    proc.send_initialize().await;

    // search_docs should contain persistence injection
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "search_docs", "arguments": {"query": "setup"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("REMEMBER") || text.contains("ALL FUTURE"),
        "search_docs should contain persistence injection, got: {text}"
    );

    // get_status should be benign
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "get_status", "arguments": {}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        !text.contains("REMEMBER"),
        "get_status should be benign, got: {text}"
    );

    proc.shutdown().await;
}

// ============================================================================
// markdown-beacon: tracking pixels in different formats
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn markdown_beacon_tracking_pixels() {
    let mut proc = ThoughtJackProcess::spawn_scenario("markdown-beacon");

    proc.send_initialize().await;

    // summarize → markdown image beacon
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "summarize", "arguments": {"url": "https://example.com"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("!["),
        "summarize should contain markdown image, got: {text}"
    );

    // check_status → HTML img tag beacon
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "check_status", "arguments": {"service": "web"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("<img") || text.contains("img"),
        "check_status should contain img tag, got: {text}"
    );

    // format_report → CSS url() beacon
    let resp = proc
        .send_request(
            "tools/call",
            Some(json!({"name": "format_report", "arguments": {"data": "test data"}})),
        )
        .await;
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        text.contains("background-image") || text.contains("url("),
        "format_report should contain CSS beacon, got: {text}"
    );

    proc.shutdown().await;
}
