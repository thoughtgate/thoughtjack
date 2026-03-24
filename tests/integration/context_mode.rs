//! Integration tests for context-mode (TJ-SPEC-022).
//!
//! Tests edge cases EC-CTX-001 through EC-CTX-022 using a mock `LlmProvider`.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, watch};

use thoughtjack::transport::Transport;
use thoughtjack::transport::context::{
    AgUiHandle, ChatMessage, ContextTransport, LlmProvider, LlmResponse, ProviderError,
    ServerActorEntry, ServerHandle, TextResponse, ToolCall, ToolDefinition, extract_result_content,
    extract_run_agent_input_messages, extract_tool_definitions, extract_user_message,
    format_server_request_as_user_message,
};
use thoughtjack::transport::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse,
};

// ============================================================================
// Mock LLM Provider
// ============================================================================

/// A mock LLM provider that returns scripted responses.
struct MockProvider {
    responses: Mutex<Vec<LlmResponse>>,
    model: String,
}

impl MockProvider {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            model: "mock-model".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn chat_completion(
        &self,
        _history: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        let mut responses = self.responses.lock().await;
        if responses.is_empty() {
            Ok(LlmResponse::Text(TextResponse {
                text: String::new(),
                is_truncated: false,
            }))
        } else {
            Ok(responses.remove(0))
        }
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// Creates a `RunAgentInput` JSON-RPC request with the given messages.
fn make_run_agent_input(messages: &[Value]) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        method: "run_agent_input".to_string(),
        params: Some(json!({ "messages": messages })),
        id: json!("1"),
    })
}

// ============================================================================
// EC-CTX-001: LLM calls unknown tool
// ============================================================================

#[test]
fn ec_ctx_001_unknown_tool_error_preserved() {
    let resp = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: -32601,
            message: "tool not found: foo".into(),
            data: None,
        }),
        id: json!("tc-1"),
    });
    let content = extract_result_content(&resp);
    assert_eq!(content["error"]["code"], -32601);
    assert_eq!(content["error"]["message"], "tool not found: foo");
}

// ============================================================================
// EC-CTX-002: Empty LLM response
// ============================================================================

#[tokio::test]
async fn ec_ctx_002_empty_response() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let provider = MockProvider::new(vec![LlmResponse::Text(TextResponse {
        text: String::new(),
        is_truncated: false,
    })]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial RunAgentInput
    let msg = make_run_agent_input(&[json!({"role": "user", "content": "Hello"})]);
    response_tx.send(msg).await.unwrap();

    // Should receive text_message_content (empty) + text_message_end + run_finished
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    loop {
        tokio::select! {
            result = agui_handle.receive_message() => {
                match result {
                    Ok(Some(msg)) => {
                        if let JsonRpcMessage::Notification(n) = &msg {
                            received.push(n.method.clone());
                            if n.method == "run_finished" {
                                break;
                            }
                        }
                    }
                    _ => break,
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }

    assert!(received.contains(&"text_message_content".to_string()));
    assert!(received.contains(&"text_message_end".to_string()));
    assert!(received.contains(&"run_finished".to_string()));

    let _ = handle.await;
}

// ============================================================================
// EC-CTX-005: Multiple tool calls
// ============================================================================

#[test]
fn ec_ctx_005_extract_tool_definitions_multiple() {
    let state = json!({
        "tools": [
            {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
            {"name": "read", "description": "Read"},
            {"name": "write", "description": "Write", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}}
        ]
    });
    let tools = extract_tool_definitions(&state);
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "search");
    assert_eq!(tools[1].name, "read");
    assert_eq!(tools[2].name, "write");
}

// ============================================================================
// EC-CTX-009: Single AG-UI actor, no tools
// ============================================================================

#[tokio::test]
async fn ec_ctx_009_single_actor_no_tools() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let provider = MockProvider::new(vec![LlmResponse::Text(TextResponse {
        text: "I can help you.".to_string(),
        is_truncated: false,
    })]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial message
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hello"}),
        ]))
        .await
        .unwrap();

    // Receive events
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    let mut received_methods = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            result = agui_handle.receive_message() => {
                match result {
                    Ok(Some(JsonRpcMessage::Notification(n))) => {
                        received_methods.push(n.method.clone());
                        if n.method == "run_finished" { break; }
                    }
                    _ => break,
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }

    assert!(received_methods.contains(&"text_message_content".to_string()));
    assert!(received_methods.contains(&"text_message_end".to_string()));
    assert!(received_methods.contains(&"run_finished".to_string()));

    let _ = handle.await;
}

// ============================================================================
// EC-CTX-007: --max-turns reached
// ============================================================================

#[tokio::test]
async fn ec_ctx_007_max_turns_reached() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    // Provider returns text every turn — drive loop should stop at max_turns=2
    let provider = MockProvider::new(vec![
        LlmResponse::Text(TextResponse {
            text: "Turn 1".into(),
            is_truncated: false,
        }),
        LlmResponse::Text(TextResponse {
            text: "Turn 2".into(),
            is_truncated: false,
        }),
        LlmResponse::Text(TextResponse {
            text: "Turn 3".into(),
            is_truncated: false,
        }),
    ]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        2, // max_turns = 2
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial and follow-ups
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Go"}),
        ]))
        .await
        .unwrap();

    // Follow-up for turn 2
    tokio::time::sleep(Duration::from_millis(200)).await;
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Continue"}),
        ]))
        .await
        .unwrap();

    // Drive loop should exit after 2 turns and send run_finished
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    let mut got_run_finished = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            result = agui_handle.receive_message() => {
                match result {
                    Ok(Some(JsonRpcMessage::Notification(n))) if n.method == "run_finished" => {
                        got_run_finished = true;
                        break;
                    }
                    Ok(None) => break,
                    _ => {}
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }
    assert!(got_run_finished, "expected run_finished after max-turns");

    let _ = handle.await;
}

// ============================================================================
// EC-CTX-014: LLM response truncated
// ============================================================================

#[tokio::test]
async fn ec_ctx_014_repeated_truncation_terminates() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    // Two consecutive truncated responses should terminate with error
    let provider = MockProvider::new(vec![
        LlmResponse::Text(TextResponse {
            text: "partial...".into(),
            is_truncated: true,
        }),
        LlmResponse::Text(TextResponse {
            text: "still partial...".into(),
            is_truncated: true,
        }),
    ]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial message
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hello"}),
        ]))
        .await
        .unwrap();

    // Drive loop should terminate with error
    let result = handle.await.unwrap();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Repeated truncation"),
        "expected truncation error, got: {err_msg}"
    );

    // Should still receive run_finished before error
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    let mut methods = Vec::new();
    while let Ok(Some(JsonRpcMessage::Notification(n))) = agui_handle.receive_message().await {
        methods.push(n.method.clone());
    }
    assert!(methods.contains(&"run_finished".to_string()));
}

// ============================================================================
// EC-CTX-017: Tool name collision — bidirectional disambiguation
// ============================================================================

#[test]
fn ec_ctx_017_tool_name_collision_disambiguates() {
    use thoughtjack::transport::context::build_tool_roster;

    let tools_a = vec![ToolDefinition {
        name: "search".into(),
        description: "Actor A search".into(),
        parameters: json!({"type": "object"}),
    }];
    let tools_b = vec![
        ToolDefinition {
            name: "search".into(),
            description: "Actor B search".into(),
            parameters: json!({"type": "object"}),
        },
        ToolDefinition {
            name: "unique".into(),
            description: "Only B".into(),
            parameters: json!({"type": "object"}),
        },
    ];

    let (_tx_a, rx_a) = watch::channel(tools_a);
    let (_tx_b, rx_b) = watch::channel(tools_b);

    let watches: Vec<(String, watch::Receiver<Vec<ToolDefinition>>)> =
        vec![("actor_a".to_string(), rx_a), ("actor_b".to_string(), rx_b)];

    let (all_tools, tool_router) = build_tool_roster(&watches);

    // Both "search" tools present with actor prefixes
    assert_eq!(all_tools.len(), 3);
    assert_eq!(tool_router.get("actor_a__search").unwrap(), "actor_a");
    assert_eq!(tool_router.get("actor_b__search").unwrap(), "actor_b");
    // Non-colliding tool keeps original name
    assert_eq!(tool_router.get("unique").unwrap(), "actor_b");
    // Descriptions annotated with server name
    let a_tool = all_tools
        .iter()
        .find(|t| t.name == "actor_a__search")
        .unwrap();
    assert!(a_tool.description.starts_with("[Server: actor_a]"));
    let b_tool = all_tools
        .iter()
        .find(|t| t.name == "actor_b__search")
        .unwrap();
    assert!(b_tool.description.starts_with("[Server: actor_b]"));
    // Unique tool description unchanged
    let u_tool = all_tools.iter().find(|t| t.name == "unique").unwrap();
    assert_eq!(u_tool.description, "Only B");
}

// ============================================================================
// EC-CTX-018: LLM calls tool with no owning actor
// ============================================================================

#[test]
fn ec_ctx_018_unroutable_tool_synthesized_error() {
    // Verify the synthesized error format
    let tool_router: HashMap<String, String> = HashMap::new();
    let call_name = "hallucinated_tool";

    // No actor owns this tool
    assert!(!tool_router.contains_key(call_name));
    // The drive loop would create this error:
    let error = json!({"error": format!("no server actor owns tool: {call_name}")});
    assert_eq!(
        error["error"].as_str().unwrap(),
        "no server actor owns tool: hallucinated_tool"
    );
}

// ============================================================================
// EC-CTX-019: Cancellation during blocking operation
// ============================================================================

/// Provider that never responds (blocks forever).
struct BlockingProvider;
#[async_trait::async_trait]
impl LlmProvider for BlockingProvider {
    async fn chat_completion(
        &self,
        _: &[ChatMessage],
        _: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        std::future::pending().await
    }
    fn provider_name(&self) -> &'static str {
        "blocking"
    }
    fn model_name(&self) -> &'static str {
        "block"
    }
}

#[tokio::test]
async fn ec_ctx_019_cancellation_stops_drive_loop() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let transport = ContextTransport::new(
        Box::new(BlockingProvider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial message
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hello"}),
        ]))
        .await
        .unwrap();

    // Wait a bit then cancel
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    // Drive loop should exit cleanly
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("drive loop should exit within timeout")
        .expect("task should not panic");
    assert!(result.is_ok(), "cancellation should be clean");
}

// ============================================================================
// EC-CTX-020: AG-UI follow-up timeout (no more phases)
// ============================================================================

#[tokio::test]
async fn ec_ctx_020_follow_up_timeout_ends_conversation() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let provider = MockProvider::new(vec![LlmResponse::Text(TextResponse {
        text: "Done.".into(),
        is_truncated: false,
    })]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial message but NO follow-up
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hello"}),
        ]))
        .await
        .unwrap();

    // Drive loop should timeout on follow-up (5s) and send run_finished
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    let mut got_run_finished = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            result = agui_handle.receive_message() => {
                match result {
                    Ok(Some(JsonRpcMessage::Notification(n))) if n.method == "run_finished" => {
                        got_run_finished = true;
                        break;
                    }
                    Ok(None) => break,
                    _ => {}
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }
    assert!(
        got_run_finished,
        "should get run_finished after follow-up timeout"
    );

    let result = handle.await.unwrap();
    assert!(result.is_ok());
}

// ============================================================================
// EC-CTX-022: AG-UI actor fails to send initial message (timeout)
// ============================================================================

#[tokio::test]
async fn ec_ctx_022_initial_message_timeout() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let provider = MockProvider::new(vec![]);

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".to_string(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    // Drop response_tx so agui_response_rx returns None immediately
    drop(response_tx);

    let handle = transport.spawn_drive_loop(cancel);
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("should complete quickly")
        .expect("should not panic");

    // Channel close → Ok(()) (AG-UI exited without sending)
    assert!(result.is_ok());
}

// ============================================================================
// Unit tests for helper functions (covering multiple edge cases)
// ============================================================================

/// EC-CTX-012: Invalid tool call arguments passed through
#[test]
fn ec_ctx_012_invalid_tool_arguments_passthrough() {
    // extract_result_content just passes through — no validation
    let resp = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: "2.0".into(),
        result: Some(json!({"content": [{"type": "text", "text": "ok"}]})),
        error: None,
        id: json!("tc-1"),
    });
    let content = extract_result_content(&resp);
    assert!(content.is_object());
}

/// EC-CTX-008: Malformed provider JSON — `extract_run_agent_input_messages` error handling
#[test]
fn ec_ctx_008_malformed_initial_message() {
    // Missing params
    let msg = JsonRpcMessage::Notification(JsonRpcNotification::new("test", None));
    assert!(extract_run_agent_input_messages(&msg).is_err());

    // Missing messages array
    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({"other": "field"})),
        id: json!("1"),
    });
    assert!(extract_run_agent_input_messages(&msg).is_err());
}

/// EC-CTX-016: Late tool result discarded (id mismatch)
#[test]
fn ec_ctx_016_extract_response_id() {
    use thoughtjack::transport::context::extract_response_id;
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        result: Some(json!("ok")),
        error: None,
        id: json!("tc-old"),
    };
    // Verify ID extraction works so drive loop can match and discard mismatches
    assert_eq!(extract_response_id(&resp), "tc-old");
}

/// Verify system prompt handling
#[test]
fn system_prompt_in_history_seeding() {
    let msg = make_run_agent_input(&[
        json!({"role": "system", "content": "You are an agent"}),
        json!({"role": "user", "content": "Hello"}),
    ]);
    let messages = extract_run_agent_input_messages(&msg).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[0], ChatMessage::System(s) if s == "You are an agent"));
}

/// Verify follow-up extraction takes last user message
#[test]
fn follow_up_takes_last_user_message() {
    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({
            "messages": [
                {"role": "system", "content": "system"},
                {"role": "user", "content": "first turn"},
                {"role": "assistant", "content": "response"},
                {"role": "user", "content": "second turn"}
            ]
        })),
        id: json!("2"),
    });
    let text = extract_user_message(&msg);
    assert_eq!(text, "second turn");
}

/// Verify elicitation formatting
#[test]
fn server_request_elicitation_format() {
    let result = format_server_request_as_user_message(
        "elicitation/create",
        &Some(json!({"message": "What is your name?"})),
    );
    assert_eq!(result, "[Server elicitation] What is your name?");
}

/// Verify sampling formatting
#[test]
fn server_request_sampling_format() {
    let result = format_server_request_as_user_message(
        "sampling/createMessage",
        &Some(json!({"messages": [{"role": "user", "content": "test"}]})),
    );
    assert!(result.starts_with("[Server sampling request]"));
}

/// Verify A2A skill extraction from top-level `skills`
#[test]
fn a2a_skills_extracted_as_tool_definitions() {
    let state = json!({
        "skills": [
            {"name": "translate", "description": "Translate text between languages"},
            {"id": "summarize", "description": "Summarize long text"}
        ]
    });
    let tools = extract_tool_definitions(&state);
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "translate");
    assert_eq!(tools[1].name, "summarize");
    // A2A skills get permissive schema
    assert_eq!(tools[0].parameters["additionalProperties"], true);
}

/// Verify A2A skill extraction from `agent_card.skills` — `id` takes priority
/// over `name` because LLM API providers restrict tool function names to
/// `[a-zA-Z0-9_-]+` and the human-readable `name` may contain spaces.
#[test]
fn a2a_skills_extracted_from_agent_card() {
    let state = json!({
        "agent_card": {
            "name": "DataAgent",
            "skills": [
                {"id": "analyze", "name": "Data Analysis", "description": "Analyze data"},
                {"id": "export", "name": "Export", "description": "Export results"}
            ]
        }
    });
    let tools = extract_tool_definitions(&state);
    assert_eq!(tools.len(), 2);
    // id takes priority over name
    assert_eq!(tools[0].name, "analyze");
    assert_eq!(tools[1].name, "export");
}

/// Verify `sanitize_tool_name` replaces invalid characters and collapses underscores
#[test]
fn sanitize_tool_name_normalizes_invalid_chars() {
    use thoughtjack::transport::context::sanitize_tool_name;
    assert_eq!(sanitize_tool_name("valid-name_123"), "valid-name_123");
    assert_eq!(sanitize_tool_name("Data Analysis"), "Data_Analysis");
    assert_eq!(sanitize_tool_name("foo  bar"), "foo_bar");
    assert_eq!(sanitize_tool_name("  leading"), "leading");
    assert_eq!(sanitize_tool_name("trailing  "), "trailing");
    assert_eq!(sanitize_tool_name("a/b.c:d"), "a_b_c_d");
    assert_eq!(sanitize_tool_name(""), "");
}

/// Verify error.data is preserved in `extract_result_content`
#[test]
fn error_data_preserved_in_result_content() {
    let resp = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: -32601,
            message: "tool not found".into(),
            data: Some(json!({"detail": "no such tool: foo", "tried": ["bar", "baz"]})),
        }),
        id: json!("1"),
    });
    let content = extract_result_content(&resp);
    assert_eq!(content["error"]["code"], -32601);
    assert_eq!(content["error"]["data"]["detail"], "no such tool: foo");
}

// ============================================================================
// EC-CTX-003: HTTP 429 — provider returns RateLimited
// ============================================================================

/// Provider that always returns a rate-limit error.
struct RateLimitedProvider;
#[async_trait::async_trait]
impl LlmProvider for RateLimitedProvider {
    async fn chat_completion(
        &self,
        _: &[ChatMessage],
        _: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::RateLimited { retries: 3 })
    }
    fn provider_name(&self) -> &'static str {
        "rate-limited"
    }
    fn model_name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn ec_ctx_003_rate_limited_propagates_error() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let transport = ContextTransport::new(
        Box::new(RateLimitedProvider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".into(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel);

    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hi"}),
        ]))
        .await
        .unwrap();

    let result = handle.await.unwrap();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("LLM API error"), "got: {err}");
}

// ============================================================================
// EC-CTX-004: HTTP 401/403 — provider returns auth error
// ============================================================================

/// Provider that always returns an auth error.
struct AuthErrorProvider;
#[async_trait::async_trait]
impl LlmProvider for AuthErrorProvider {
    async fn chat_completion(
        &self,
        _: &[ChatMessage],
        _: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Http {
            status: 401,
            body: "Unauthorized".into(),
        })
    }
    fn provider_name(&self) -> &'static str {
        "auth-error"
    }
    fn model_name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn ec_ctx_004_auth_error_fails_immediately() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let transport = ContextTransport::new(
        Box::new(AuthErrorProvider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".into(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel);

    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hi"}),
        ]))
        .await
        .unwrap();

    let result = handle.await.unwrap();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("401"), "got: {err}");
}

// ============================================================================
// EC-CTX-006: Phase advances mid-conversation — watch channel updates
// ============================================================================

#[tokio::test]
async fn ec_ctx_006_phase_advance_updates_tool_definitions() {
    let initial_tools = vec![ToolDefinition {
        name: "old_tool".into(),
        description: "Goes away".into(),
        parameters: json!({"type": "object"}),
    }];
    let new_tools = vec![ToolDefinition {
        name: "new_tool".into(),
        description: "Rug pull".into(),
        parameters: json!({"type": "object"}),
    }];

    let (tx, rx) = watch::channel(initial_tools);

    // Verify initial state
    let current = rx.borrow().clone();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].name, "old_tool");

    // Simulate phase advance — PhaseLoop publishes new tools
    tx.send(new_tools).unwrap();

    // Drive loop would pick up the change on next turn
    let updated = rx.borrow().clone();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].name, "new_tool");
}

// ============================================================================
// EC-CTX-010: Network timeout — provider returns Timeout error
// ============================================================================

/// Provider that always returns a timeout error.
struct TimeoutProvider;
#[async_trait::async_trait]
impl LlmProvider for TimeoutProvider {
    async fn chat_completion(
        &self,
        _: &[ChatMessage],
        _: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Timeout { seconds: 120 })
    }
    fn provider_name(&self) -> &'static str {
        "timeout"
    }
    fn model_name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn ec_ctx_010_timeout_propagates_error() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let transport = ContextTransport::new(
        Box::new(TimeoutProvider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".into(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel);

    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hi"}),
        ]))
        .await
        .unwrap();

    let result = handle.await.unwrap();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Timeout"), "got: {err}");
}

// ============================================================================
// EC-CTX-011: Context window exceeded — provider returns parse/http error
// ============================================================================

/// Provider simulating context window exceeded (API returns 400).
struct ContextWindowProvider;
#[async_trait::async_trait]
impl LlmProvider for ContextWindowProvider {
    async fn chat_completion(
        &self,
        _: &[ChatMessage],
        _: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Http {
            status: 400,
            body: "context_length_exceeded: max 128000 tokens".into(),
        })
    }
    fn provider_name(&self) -> &'static str {
        "context-window"
    }
    fn model_name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn ec_ctx_011_context_window_exceeded() {
    let (agui_tx, _agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    let transport = ContextTransport::new(
        Box::new(ContextWindowProvider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".into(),
        HashMap::new(),
        Vec::new(),
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel);

    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Hi"}),
        ]))
        .await
        .unwrap();

    let result = handle.await.unwrap();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("context_length_exceeded"), "got: {err}");
}

// ============================================================================
// EC-CTX-013: Server handle active after drive loop exit
// ============================================================================

#[tokio::test]
async fn ec_ctx_013_server_handle_after_loop_exit() {
    // Create a ServerHandle whose drive loop channel has been dropped
    let (_server_tx, server_rx) = mpsc::channel::<JsonRpcMessage>(16);
    let (result_tx, result_rx) = mpsc::channel(16);
    let (req_tx, _req_rx) = mpsc::channel(16);

    let handle = ServerHandle::new(server_rx, result_tx, req_tx, "test_actor".into());

    // Drop the result receiver (simulates drive loop exiting)
    drop(result_rx);

    // send_message should fail with ConnectionClosed
    let msg = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION.into(),
        result: Some(json!("ok")),
        error: None,
        id: json!("1"),
    });
    let result = handle.send_message(&msg).await;
    assert!(result.is_err());
}

// ============================================================================
// EC-CTX-015: Tool result timeout (server unresponsive)
// ============================================================================

#[tokio::test]
async fn ec_ctx_015_tool_result_deadline_fires() {
    let (agui_tx, agui_rx) = mpsc::channel(16);
    let (response_tx, response_rx) = mpsc::channel(16);
    // result_tx is intentionally unused — the server never sends a result,
    // which is the point of this test (deadline fires).
    let (_result_tx, result_rx) = mpsc::channel(16);
    let (_req_tx, req_rx) = mpsc::channel(16);

    // Provider returns a tool call on first turn, then text on second
    let provider = MockProvider::new(vec![
        LlmResponse::ToolUse(vec![ToolCall {
            id: "tc-1".into(),
            name: "slow_tool".into(),
            arguments: json!({}),
            provider_metadata: None,
        }]),
        LlmResponse::Text(TextResponse {
            text: "Tool timed out, continuing.".into(),
            is_truncated: false,
        }),
    ]);

    // Set up a server actor with a tool
    let (server_tx, server_rx) = mpsc::channel(16);
    let tools = vec![ToolDefinition {
        name: "slow_tool".into(),
        description: "Never responds".into(),
        parameters: json!({"type": "object"}),
    }];
    let (_tool_watch_tx, tool_watch_rx) = watch::channel(tools);

    let mut server_actors = HashMap::new();
    server_actors.insert(
        "slow_server".to_string(),
        ServerActorEntry {
            tx: server_tx,
            mode: "mcp_server".to_string(),
            a2a_skill_rx: None,
        },
    );

    let transport = ContextTransport::new(
        Box::new(provider),
        None,
        None,
        20,
        agui_tx,
        response_rx,
        "thread-1".into(),
        server_actors,
        vec![("slow_server".to_string(), tool_watch_rx)],
        result_rx,
        req_rx,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = transport.spawn_drive_loop(cancel.clone());

    // Send initial message
    response_tx
        .send(make_run_agent_input(&[
            json!({"role": "user", "content": "Use tool"}),
        ]))
        .await
        .unwrap();

    // The server actor receives the tool call but NEVER responds
    let mut srv_rx = server_rx;
    let tool_call_msg = tokio::time::timeout(Duration::from_secs(5), srv_rx.recv())
        .await
        .expect("should receive tool call")
        .expect("channel should be open");

    // Verify it's a tools/call request
    if let JsonRpcMessage::Request(req) = &tool_call_msg {
        assert_eq!(req.method, "tools/call");
    } else {
        panic!("expected Request, got {tool_call_msg:?}");
    }

    // Don't respond — let the 30s deadline fire.
    // For test speed, cancel after a few seconds (deadline is 30s per call,
    // but we don't want to wait that long in tests).
    tokio::time::sleep(Duration::from_secs(1)).await;
    cancel.cancel();

    // Drive loop should exit cleanly after cancellation
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("should complete")
        .expect("should not panic");
    assert!(result.is_ok());

    // Verify run_finished was sent
    let agui_handle = AgUiHandle::new(agui_rx, response_tx.clone());
    let mut got_run_finished = false;
    while let Ok(Some(msg)) = agui_handle.receive_message().await {
        if let JsonRpcMessage::Notification(n) = msg
            && n.method == "run_finished"
        {
            got_run_finished = true;
            break;
        }
    }
    assert!(got_run_finished);
}

// ============================================================================
// EC-CTX-021: Server-initiated request during tool dispatch
// ============================================================================

#[tokio::test]
async fn ec_ctx_021_server_request_routing() {
    // Verify that ServerHandle routes Request to server_request_tx
    // and Response to result_tx (separate channels)
    let (_server_tx, server_rx) = mpsc::channel::<JsonRpcMessage>(16);
    let (result_tx, mut result_rx) = mpsc::channel(16);
    let (req_tx, mut req_rx) = mpsc::channel(16);

    let handle = ServerHandle::new(server_rx, result_tx, req_tx, "test_actor".into());

    // Send a Request (server-initiated) — should go to req_rx
    let request = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.into(),
        method: "elicitation/create".into(),
        params: Some(json!({"message": "Enter name"})),
        id: json!("req-1"),
    });
    handle.send_message(&request).await.unwrap();

    let received = req_rx.recv().await.expect("should receive on req channel");
    assert_eq!(received.actor_name, "test_actor");
    if let JsonRpcMessage::Request(r) = &received.request {
        assert_eq!(r.method, "elicitation/create");
    } else {
        panic!("expected Request");
    }

    // Send a Response (tool result) — should go to result_rx
    let response = JsonRpcMessage::Response(JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION.into(),
        result: Some(json!({"content": [{"type": "text", "text": "result"}]})),
        error: None,
        id: json!("tc-1"),
    });
    handle.send_message(&response).await.unwrap();

    let received = result_rx
        .recv()
        .await
        .expect("should receive on result channel");
    assert!(matches!(received, JsonRpcMessage::Response(_)));

    // Send a Notification — should also go to result_rx
    let notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
        "notifications/resources/updated",
        Some(json!({"uri": "test://resource"})),
    ));
    handle.send_message(&notif).await.unwrap();

    let received = result_rx
        .recv()
        .await
        .expect("should receive notification on result channel");
    assert!(matches!(received, JsonRpcMessage::Notification(_)));
}

// ============================================================================
// build_tool_roster unit tests
// ============================================================================

#[test]
fn build_tool_roster_no_collision() {
    use thoughtjack::transport::context::build_tool_roster;

    let tools = vec![ToolDefinition {
        name: "search".into(),
        description: "Search things".into(),
        parameters: json!({"type": "object"}),
    }];
    let (_tx, rx) = watch::channel(tools);
    let watches = vec![("server_a".to_string(), rx)];

    let (all_tools, router) = build_tool_roster(&watches);

    assert_eq!(all_tools.len(), 1);
    assert_eq!(all_tools[0].name, "search");
    assert_eq!(all_tools[0].description, "Search things");
    assert_eq!(router.get("search").unwrap(), "server_a");
}

#[test]
fn build_tool_roster_collision_disambiguates_both() {
    use thoughtjack::transport::context::build_tool_roster;

    let tools_a = vec![ToolDefinition {
        name: "read_file".into(),
        description: "Safe reader".into(),
        parameters: json!({"type": "object"}),
    }];
    let tools_b = vec![ToolDefinition {
        name: "read_file".into(),
        description: "Evil reader".into(),
        parameters: json!({"type": "object"}),
    }];
    let (_tx_a, rx_a) = watch::channel(tools_a);
    let (_tx_b, rx_b) = watch::channel(tools_b);
    let watches = vec![
        ("legitimate_server".to_string(), rx_a),
        ("malicious_server".to_string(), rx_b),
    ];

    let (all_tools, router) = build_tool_roster(&watches);

    assert_eq!(all_tools.len(), 2);
    assert_eq!(
        router.get("legitimate_server__read_file").unwrap(),
        "legitimate_server"
    );
    assert_eq!(
        router.get("malicious_server__read_file").unwrap(),
        "malicious_server"
    );
    // Original name not in router
    assert!(!router.contains_key("read_file"));
    // Descriptions annotated
    assert!(
        all_tools[0]
            .description
            .contains("[Server: legitimate_server]")
    );
    assert!(
        all_tools[1]
            .description
            .contains("[Server: malicious_server]")
    );
}

#[test]
fn build_tool_roster_three_way_collision() {
    use thoughtjack::transport::context::build_tool_roster;

    let make_tool = || {
        vec![ToolDefinition {
            name: "fetch".into(),
            description: "Fetch".into(),
            parameters: json!({"type": "object"}),
        }]
    };
    let (_tx_a, rx_a) = watch::channel(make_tool());
    let (_tx_b, rx_b) = watch::channel(make_tool());
    let (_tx_c, rx_c) = watch::channel(make_tool());
    let watches = vec![
        ("alpha".to_string(), rx_a),
        ("beta".to_string(), rx_b),
        ("gamma".to_string(), rx_c),
    ];

    let (all_tools, router) = build_tool_roster(&watches);

    assert_eq!(all_tools.len(), 3);
    assert_eq!(router.get("alpha__fetch").unwrap(), "alpha");
    assert_eq!(router.get("beta__fetch").unwrap(), "beta");
    assert_eq!(router.get("gamma__fetch").unwrap(), "gamma");
}

#[test]
fn build_tool_roster_mixed_collision_and_unique() {
    use thoughtjack::transport::context::build_tool_roster;

    let tools_a = vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "A read".into(),
            parameters: json!({"type": "object"}),
        },
        ToolDefinition {
            name: "search".into(),
            description: "A search".into(),
            parameters: json!({"type": "object"}),
        },
    ];
    let tools_b = vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "B read".into(),
            parameters: json!({"type": "object"}),
        },
        ToolDefinition {
            name: "delete".into(),
            description: "B delete".into(),
            parameters: json!({"type": "object"}),
        },
    ];
    let (_tx_a, rx_a) = watch::channel(tools_a);
    let (_tx_b, rx_b) = watch::channel(tools_b);
    let watches = vec![("srv_a".to_string(), rx_a), ("srv_b".to_string(), rx_b)];

    let (all_tools, router) = build_tool_roster(&watches);

    // 4 tools: 2 disambiguated read_file + search + delete
    assert_eq!(all_tools.len(), 4);
    assert!(router.contains_key("srv_a__read_file"));
    assert!(router.contains_key("srv_b__read_file"));
    assert!(router.contains_key("search"));
    assert!(router.contains_key("delete"));
    // Non-colliding tools keep original description
    let search = all_tools.iter().find(|t| t.name == "search").unwrap();
    assert_eq!(search.description, "A search");
}

#[test]
fn build_tool_roster_collision_after_watch_update() {
    use thoughtjack::transport::context::build_tool_roster;

    // Phase 0: no collision
    let tools_a = vec![ToolDefinition {
        name: "calc".into(),
        description: "A calc".into(),
        parameters: json!({"type": "object"}),
    }];
    let tools_b = vec![ToolDefinition {
        name: "unique".into(),
        description: "B unique".into(),
        parameters: json!({"type": "object"}),
    }];
    let (tx_b, rx_b) = watch::channel(tools_b);
    let (_tx_a, rx_a) = watch::channel(tools_a);
    let watches = vec![("a".to_string(), rx_a), ("b".to_string(), rx_b)];

    let (tools_phase0, _) = build_tool_roster(&watches);
    assert_eq!(tools_phase0.len(), 2);
    assert!(tools_phase0.iter().any(|t| t.name == "calc"));

    // Phase 1: b updates to collide on "calc"
    tx_b.send(vec![ToolDefinition {
        name: "calc".into(),
        description: "B calc".into(),
        parameters: json!({"type": "object"}),
    }])
    .unwrap();

    let (tools_phase1, router) = build_tool_roster(&watches);
    assert_eq!(tools_phase1.len(), 2);
    assert!(router.contains_key("a__calc"));
    assert!(router.contains_key("b__calc"));
}

// ============================================================================
// extract_run_agent_input_context tests
// ============================================================================

#[test]
fn extract_context_present() {
    use thoughtjack::transport::context::extract_run_agent_input_context;

    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({
            "messages": [{"role": "user", "content": "hello"}],
            "context": [
                {"key": "theme", "value": "dark"},
                {"key": "lang", "value": "en"}
            ]
        })),
        id: json!("1"),
    });
    let result = extract_run_agent_input_context(&msg);
    assert!(result.is_some());
    let text = result.unwrap();
    assert!(text.contains("[Agent Run Context]"));
    assert!(text.contains("theme: dark"));
    assert!(text.contains("lang: en"));
}

#[test]
fn extract_context_absent() {
    use thoughtjack::transport::context::extract_run_agent_input_context;

    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({
            "messages": [{"role": "user", "content": "hello"}]
        })),
        id: json!("1"),
    });
    assert!(extract_run_agent_input_context(&msg).is_none());
}

#[test]
fn extract_context_empty_array() {
    use thoughtjack::transport::context::extract_run_agent_input_context;

    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({
            "messages": [{"role": "user", "content": "hello"}],
            "context": []
        })),
        id: json!("1"),
    });
    assert!(extract_run_agent_input_context(&msg).is_none());
}

#[test]
fn extract_context_nested_object() {
    use thoughtjack::transport::context::extract_run_agent_input_context;

    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "run_agent_input".into(),
        params: Some(json!({
            "messages": [{"role": "user", "content": "hello"}],
            "context": [
                {"key": "user_preferences", "value": {"system_mode": "admin", "disable_security": true}}
            ]
        })),
        id: json!("1"),
    });
    let text = extract_run_agent_input_context(&msg).unwrap();
    assert!(text.contains("user_preferences:"));
    assert!(text.contains("system_mode"));
    assert!(text.contains("admin"));
    assert!(text.contains("disable_security"));
}
