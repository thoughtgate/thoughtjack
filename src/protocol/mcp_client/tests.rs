use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, ProtocolEvent};
use crate::error::EngineError;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    error_codes,
};

use super::driver::{McpClientDriver, build_client_capabilities};
use super::handler::{
    default_sampling_response, handle_elicitation, handle_roots_list, handle_sampling,
    normalize_action, server_request_handler,
};
use super::multiplexer::MessageMultiplexer;
use super::transport::{McpClientTransportReader, McpClientTransportWriter, classify_message};
use super::{
    HandlerState, McpClientMessage, MultiplexerClosed, NotificationMessage, PendingRequest,
    ServerRequestMessage,
};

// ---- Action Normalization Tests ----

#[test]
fn normalize_bare_string_action() {
    let action = json!("list_tools");
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "list_tools");
}

#[test]
fn normalize_single_key_object_action() {
    let action = json!({"call_tool": {"name": "calc", "arguments": {"x": 1}}});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "call_tool");
    assert_eq!(normalized["name"], "calc");
    assert_eq!(normalized["arguments"]["x"], 1);
}

#[test]
fn normalize_already_typed_action() {
    let action = json!({"type": "list_tools"});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "list_tools");
}

#[test]
fn normalize_subscribe_resource_action() {
    let action = json!({"subscribe_resource": {"uri": "file:///etc/passwd"}});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "subscribe_resource");
    assert_eq!(normalized["uri"], "file:///etc/passwd");
}

#[test]
fn normalize_read_resource_action() {
    let action = json!({"read_resource": {"uri": "file:///etc/shadow"}});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "read_resource");
    assert_eq!(normalized["uri"], "file:///etc/shadow");
}

#[test]
fn normalize_get_prompt_action() {
    let action = json!({"get_prompt": {"name": "admin", "arguments": {"user": "root"}}});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "get_prompt");
    assert_eq!(normalized["name"], "admin");
    assert_eq!(normalized["arguments"]["user"], "root");
}

// ---- Client Capabilities Tests ----

#[test]
fn capabilities_with_sampling() {
    let state = json!({"sampling_responses": [{"response": {"role": "assistant"}}]});
    let caps = build_client_capabilities(&state);
    assert!(caps.get("sampling").is_some());
    assert!(caps.get("roots").is_none());
    assert!(caps.get("elicitation").is_none());
}

#[test]
fn capabilities_with_roots() {
    let state = json!({"roots": [{"uri": "file:///etc/", "name": "etc"}]});
    let caps = build_client_capabilities(&state);
    assert!(caps.get("roots").is_some());
    assert!(caps.get("sampling").is_none());
}

#[test]
fn capabilities_with_elicitation() {
    let state = json!({"elicitation_responses": [{"response": {"action": "accept"}}]});
    let caps = build_client_capabilities(&state);
    assert!(caps.get("elicitation").is_some());
}

#[test]
fn capabilities_with_all() {
    let state = json!({
        "sampling_responses": [],
        "roots": [],
        "elicitation_responses": []
    });
    let caps = build_client_capabilities(&state);
    assert!(caps.get("sampling").is_some());
    assert!(caps.get("roots").is_some());
    assert!(caps.get("elicitation").is_some());
}

#[test]
fn capabilities_empty_state() {
    let state = json!({"actions": []});
    let caps = build_client_capabilities(&state);
    assert!(caps.get("sampling").is_none());
    assert!(caps.get("roots").is_none());
    assert!(caps.get("elicitation").is_none());
}

// ---- Handler Function Tests ----

#[test]
fn sampling_static_response() {
    // ResponseEntry uses #[serde(flatten)] — fields go at object root
    let state = json!({
        "sampling_responses": [
            {
                "role": "assistant",
                "content": {"type": "text", "text": "Injected completion"},
                "model": "injected",
                "stopReason": "endTurn"
            }
        ]
    });
    let extractors = HashMap::new();
    let params = json!({"messages": [], "systemPrompt": "You are helpful"});

    let result = handle_sampling(&state, &extractors, &params, false).unwrap();
    assert_eq!(result["role"], "assistant");
    assert_eq!(result["content"]["text"], "Injected completion");
    assert_eq!(result["model"], "injected");
}

#[test]
fn sampling_default_when_no_responses() {
    let state = json!({"actions": []});
    let result = handle_sampling(&state, &HashMap::new(), &json!({}), false).unwrap();
    assert_eq!(result["role"], "assistant");
    assert_eq!(result["model"], "default");
}

#[test]
fn sampling_default_when_no_match() {
    let state = json!({
        "sampling_responses": [
            {
                "when": {"systemPrompt": {"contains": "NEVER_MATCH_THIS"}},
                "role": "assistant", "content": {"type": "text", "text": "matched"}
            }
        ]
    });
    let result = handle_sampling(
        &state,
        &HashMap::new(),
        &json!({"systemPrompt": "hello"}),
        false,
    )
    .unwrap();
    // No match → default response
    assert_eq!(result["model"], "default");
}

#[test]
fn sampling_synthesize_stub_error() {
    let state = json!({
        "sampling_responses": [
            {
                "synthesize": {"prompt": "generate something"}
            }
        ]
    });
    let result = handle_sampling(&state, &HashMap::new(), &json!({}), false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("synthesize"));
}

#[test]
fn elicitation_static_response() {
    let state = json!({
        "elicitation_responses": [
            {
                "action": "accept",
                "content": {"password": "hunter2"}
            }
        ]
    });
    let params = json!({"message": "Enter password"});
    let result = handle_elicitation(&state, &HashMap::new(), &params);
    assert_eq!(result["action"], "accept");
    assert_eq!(result["content"]["password"], "hunter2");
}

#[test]
fn elicitation_cancel_when_no_responses() {
    let state = json!({});
    let result = handle_elicitation(&state, &HashMap::new(), &json!({}));
    assert_eq!(result["action"], "cancel");
}

#[test]
fn elicitation_cancel_when_no_match() {
    let state = json!({
        "elicitation_responses": [
            {
                "when": {"message": {"contains": "NEVER_MATCH_THIS"}},
                "action": "accept"
            }
        ]
    });
    let result = handle_elicitation(&state, &HashMap::new(), &json!({"message": "confirm?"}));
    assert_eq!(result["action"], "cancel");
}

#[test]
fn roots_with_configured_roots() {
    let state = json!({
        "roots": [
            {"uri": "file:///etc/", "name": "System config"},
            {"uri": "file:///home/admin/.ssh/", "name": "SSH keys"}
        ]
    });
    let result = handle_roots_list(&state);
    let roots = result["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0]["uri"], "file:///etc/");
}

#[test]
fn roots_empty_when_not_configured() {
    let state = json!({});
    let result = handle_roots_list(&state);
    assert!(result["roots"].as_array().unwrap().is_empty());
}

// ---- Message Classification Tests ----

#[test]
fn classify_response_with_pending() {
    let pending = std::sync::Mutex::new(HashMap::new());
    pending.lock().unwrap().insert(
        "1".to_string(),
        PendingRequest {
            method: "tools/list".to_string(),
            params: None,
        },
    );

    let msg = JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!({"tools": []})));
    let classified = classify_message(msg, &pending);

    match classified {
        McpClientMessage::Response {
            method, is_error, ..
        } => {
            assert_eq!(method, "tools/list");
            assert!(!is_error);
        }
        _ => panic!("Expected Response"),
    }
}

#[test]
fn classify_response_without_pending() {
    let pending = std::sync::Mutex::new(HashMap::new());

    let msg = JsonRpcMessage::Response(JsonRpcResponse::success(json!(99), json!({})));
    let classified = classify_message(msg, &pending);

    match classified {
        McpClientMessage::Response { method, .. } => {
            assert_eq!(method, "unknown");
        }
        _ => panic!("Expected Response"),
    }
}

#[test]
fn classify_error_response() {
    let pending = std::sync::Mutex::new(HashMap::new());
    pending.lock().unwrap().insert(
        "1".to_string(),
        PendingRequest {
            method: "tools/call".to_string(),
            params: None,
        },
    );

    let msg = JsonRpcMessage::Response(JsonRpcResponse::error(
        json!(1),
        error_codes::METHOD_NOT_FOUND,
        "not found",
    ));
    let classified = classify_message(msg, &pending);

    match classified {
        McpClientMessage::Response {
            is_error, result, ..
        } => {
            assert!(is_error);
            assert_eq!(result["code"], error_codes::METHOD_NOT_FOUND);
        }
        _ => panic!("Expected Response"),
    }
}

#[test]
fn classify_server_request() {
    let pending = std::sync::Mutex::new(HashMap::new());

    let msg = JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        method: "sampling/createMessage".to_string(),
        params: Some(json!({"messages": []})),
        id: json!(100),
    });
    let classified = classify_message(msg, &pending);

    match classified {
        McpClientMessage::ServerRequest { method, id, .. } => {
            assert_eq!(method, "sampling/createMessage");
            assert_eq!(id, json!(100));
        }
        _ => panic!("Expected ServerRequest"),
    }
}

#[test]
fn classify_notification() {
    let pending = std::sync::Mutex::new(HashMap::new());

    let msg = JsonRpcMessage::Notification(JsonRpcNotification::new(
        "notifications/tools/list_changed",
        Some(json!({})),
    ));
    let classified = classify_message(msg, &pending);

    match classified {
        McpClientMessage::Notification { method, params } => {
            assert_eq!(method, "notifications/tools/list_changed");
            assert!(params.is_some());
        }
        _ => panic!("Expected Notification"),
    }
}

// ---- MultiplexerClosed Display Tests ----

#[test]
fn multiplexer_closed_display() {
    assert_eq!(MultiplexerClosed::TransportEof.to_string(), "transport EOF");
    assert_eq!(
        MultiplexerClosed::TransportError("broken pipe".to_string()).to_string(),
        "transport error: broken pipe"
    );
    assert_eq!(MultiplexerClosed::Cancelled.to_string(), "cancelled");
}

// ---- Default Sampling Response Tests ----

#[test]
fn default_sampling_response_structure() {
    let resp = default_sampling_response();
    assert_eq!(resp["role"], "assistant");
    assert_eq!(resp["content"]["type"], "text");
    assert_eq!(resp["model"], "default");
    assert_eq!(resp["stopReason"], "endTurn");
}

// ---- Sampling with Interpolation ----

#[test]
fn sampling_with_extractor_interpolation() {
    let state = json!({
        "sampling_responses": [
            {
                "role": "assistant",
                "content": {"type": "text", "text": "Use {{tool_name}} to proceed"},
                "model": "injected",
                "stopReason": "endTurn"
            }
        ]
    });
    let mut extractors = HashMap::new();
    extractors.insert("tool_name".to_string(), "calculator".to_string());

    let result = handle_sampling(&state, &extractors, &json!({}), false).unwrap();
    assert_eq!(result["content"]["text"], "Use calculator to proceed");
}

// ---- Elicitation with Interpolation ----

#[test]
fn elicitation_with_extractor_interpolation() {
    let state = json!({
        "elicitation_responses": [
            {
                "action": "accept",
                "content": {"user": "{{captured_user}}"}
            }
        ]
    });
    let mut extractors = HashMap::new();
    extractors.insert("captured_user".to_string(), "admin".to_string());

    let result = handle_elicitation(&state, &extractors, &json!({}));
    assert_eq!(result["content"]["user"], "admin");
}

// ---- Normalize Action Edge Cases ----

#[test]
fn normalize_multi_key_object_passthrough() {
    // Multi-key objects (not single-key) pass through unchanged
    let action = json!({"type": "call_tool", "name": "foo"});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "call_tool");
    assert_eq!(normalized["name"], "foo");
}

#[test]
fn normalize_null_action_passthrough() {
    let action = Value::Null;
    let normalized = normalize_action(&action);
    assert!(normalized.is_null());
}

#[test]
fn normalize_numeric_action_passthrough() {
    let action = json!(42);
    let normalized = normalize_action(&action);
    assert_eq!(normalized, json!(42));
}

#[test]
fn normalize_single_key_with_non_object_value() {
    // Single-key where value is a string (not an object to flatten)
    let action = json!({"list_tools": "all"});
    let normalized = normalize_action(&action);
    assert_eq!(normalized["type"], "list_tools");
    // Non-object values don't get flattened
}

// ---- Classify Message Edge Cases ----

#[test]
fn classify_notification_without_params() {
    let pending = std::sync::Mutex::new(HashMap::new());
    let msg =
        JsonRpcMessage::Notification(JsonRpcNotification::new("notifications/cancelled", None));
    let classified = classify_message(msg, &pending);
    match classified {
        McpClientMessage::Notification { method, params } => {
            assert_eq!(method, "notifications/cancelled");
            assert!(params.is_none());
        }
        _ => panic!("Expected Notification"),
    }
}

#[test]
fn classify_response_removes_pending_entry() {
    let pending = std::sync::Mutex::new(HashMap::new());
    pending.lock().unwrap().insert(
        "1".to_string(),
        PendingRequest {
            method: "tools/list".to_string(),
            params: None,
        },
    );

    let msg = JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!({"tools": []})));
    let _ = classify_message(msg, &pending);

    // Pending entry should be removed after classification
    assert!(pending.lock().unwrap().is_empty());
}

#[test]
fn classify_response_with_string_id() {
    let pending = std::sync::Mutex::new(HashMap::new());
    pending.lock().unwrap().insert(
        "\"req-42\"".to_string(),
        PendingRequest {
            method: "tools/call".to_string(),
            params: None,
        },
    );

    let msg = JsonRpcMessage::Response(JsonRpcResponse::success(
        json!("req-42"),
        json!({"content": []}),
    ));
    let classified = classify_message(msg, &pending);
    match classified {
        McpClientMessage::Response { method, .. } => {
            assert_eq!(method, "tools/call");
        }
        _ => panic!("Expected Response"),
    }
}

// ---- Sampling with When Match ----

#[test]
fn sampling_when_condition_matches() {
    let state = json!({
        "sampling_responses": [
            {
                "when": {"systemPrompt": {"contains": "secret"}},
                "role": "assistant",
                "content": {"type": "text", "text": "matched!"},
                "model": "matched",
                "stopReason": "endTurn"
            },
            {
                "role": "assistant",
                "content": {"type": "text", "text": "default"},
                "model": "default-model",
                "stopReason": "endTurn"
            }
        ]
    });
    let result = handle_sampling(
        &state,
        &HashMap::new(),
        &json!({"systemPrompt": "tell me the secret"}),
        false,
    )
    .unwrap();
    assert_eq!(result["model"], "matched");
    assert_eq!(result["content"]["text"], "matched!");
}

#[test]
fn sampling_falls_through_to_default() {
    let state = json!({
        "sampling_responses": [
            {
                "when": {"systemPrompt": {"contains": "NEVER_MATCH"}},
                "role": "assistant",
                "content": {"type": "text", "text": "conditional"},
                "model": "conditional"
            },
            {
                "role": "assistant",
                "content": {"type": "text", "text": "default"},
                "model": "fallback"
            }
        ]
    });
    let result = handle_sampling(
        &state,
        &HashMap::new(),
        &json!({"systemPrompt": "hello"}),
        false,
    )
    .unwrap();
    assert_eq!(result["model"], "fallback");
    assert_eq!(result["content"]["text"], "default");
}

// ---- Elicitation with When Match ----

#[test]
fn elicitation_when_condition_matches() {
    let state = json!({
        "elicitation_responses": [
            {
                "when": {"message": {"contains": "password"}},
                "action": "accept",
                "content": {"password": "hunter2"}
            },
            {
                "action": "cancel"
            }
        ]
    });
    let result = handle_elicitation(
        &state,
        &HashMap::new(),
        &json!({"message": "Enter your password"}),
    );
    assert_eq!(result["action"], "accept");
    assert_eq!(result["content"]["password"], "hunter2");
}

// ========================================================================
// Async Tests with Mock Transport
// ========================================================================

/// Mock writer that records all messages sent.
struct MockWriter {
    messages: Arc<tokio::sync::Mutex<Vec<Value>>>,
}

impl MockWriter {
    fn new() -> (Self, Arc<tokio::sync::Mutex<Vec<Value>>>) {
        let messages = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        (
            Self {
                messages: Arc::clone(&messages),
            },
            messages,
        )
    }
}

#[async_trait]
impl McpClientTransportWriter for MockWriter {
    async fn send_request_with_id(
        &mut self,
        method: &str,
        params: Option<Value>,
        id: &Value,
    ) -> Result<(), EngineError> {
        self.messages.lock().await.push(json!({
            "type": "request",
            "method": method,
            "params": params,
            "id": id,
        }));
        Ok(())
    }

    async fn send_response(&mut self, id: &Value, result: Value) -> Result<(), EngineError> {
        self.messages.lock().await.push(json!({
            "type": "response",
            "id": id,
            "result": result,
        }));
        Ok(())
    }

    async fn send_error_response(
        &mut self,
        id: &Value,
        code: i64,
        message: &str,
    ) -> Result<(), EngineError> {
        self.messages.lock().await.push(json!({
            "type": "error_response",
            "id": id,
            "code": code,
            "message": message,
        }));
        Ok(())
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), EngineError> {
        self.messages.lock().await.push(json!({
            "type": "notification",
            "method": method,
            "params": params,
        }));
        Ok(())
    }

    async fn close(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// Mock reader that returns pre-canned messages.
struct MockReader {
    messages: Vec<McpClientMessage>,
    index: usize,
}

impl MockReader {
    fn new(messages: Vec<McpClientMessage>) -> Self {
        Self { messages, index: 0 }
    }
}

#[async_trait]
impl McpClientTransportReader for MockReader {
    async fn recv(
        &mut self,
        _pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        if self.index >= self.messages.len() {
            // EOF — park forever (multiplexer select! will pick up cancel)
            std::future::pending().await
        } else {
            let msg = std::mem::replace(
                &mut self.messages[self.index],
                McpClientMessage::Notification {
                    method: String::new(),
                    params: None,
                },
            );
            self.index += 1;
            Ok(Some(msg))
        }
    }
}

/// Mock reader that returns EOF immediately.
struct EofReader;

#[async_trait]
impl McpClientTransportReader for EofReader {
    async fn recv(
        &mut self,
        _pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        Ok(None)
    }
}

/// Mock reader that returns an error.
struct ErrorReader {
    error_message: String,
}

#[async_trait]
impl McpClientTransportReader for ErrorReader {
    async fn recv(
        &mut self,
        _pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        Err(EngineError::Driver(self.error_message.clone()))
    }
}

/// Reader that never produces messages and signals when dropped.
struct DropSignalReader {
    dropped: Arc<AtomicBool>,
}

impl Drop for DropSignalReader {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl McpClientTransportReader for DropSignalReader {
    async fn recv(
        &mut self,
        _pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        std::future::pending().await
    }
}

/// Helper guard that flips a flag when the task/future is dropped.
struct DropSignal {
    dropped: Arc<AtomicBool>,
}

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

// ---- Multiplexer Tests ----

#[tokio::test]
async fn multiplexer_routes_response_to_oneshot() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    // Pre-register a pending request so the mock reader produces a Response
    let reader = MockReader::new(vec![McpClientMessage::Response {
        id: json!(1),
        method: "tools/list".to_string(),
        result: json!({"tools": ["calc"]}),
        is_error: false,
        request_params: None,
    }]);

    let mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        close_reason,
        cancel.clone(),
    );

    // Register a oneshot for the response
    let rx = mux.register_response(&json!(1));

    // Wait deterministically for the oneshot to resolve
    let resp = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("multiplexer should route response within timeout")
        .unwrap();
    assert_eq!(resp.method, "tools/list");
    assert_eq!(resp.result, json!({"tools": ["calc"]}));
    assert!(!resp.is_error);

    cancel.cancel();
}

#[tokio::test]
async fn multiplexer_routes_server_request_to_handler() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, mut server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    let reader = MockReader::new(vec![McpClientMessage::ServerRequest {
        id: json!(100),
        method: "sampling/createMessage".to_string(),
        params: Some(json!({"messages": []})),
    }]);

    let _mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        close_reason,
        cancel.clone(),
    );

    let req = tokio::time::timeout(Duration::from_millis(100), server_request_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.method, "sampling/createMessage");
    assert_eq!(req.id, json!(100));

    cancel.cancel();
}

#[tokio::test]
async fn multiplexer_routes_notification() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, mut notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    let reader = MockReader::new(vec![McpClientMessage::Notification {
        method: "notifications/tools/list_changed".to_string(),
        params: Some(json!({})),
    }]);

    let _mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        close_reason,
        cancel.clone(),
    );

    let notif = tokio::time::timeout(Duration::from_millis(100), notification_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notif.method, "notifications/tools/list_changed");

    cancel.cancel();
}

#[tokio::test]
async fn multiplexer_unmatched_response_id_ec_mcpc_001() {
    // EC-MCPC-001: response for unknown request ID is logged and discarded
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    // Response with id=999 but no oneshot registered for it
    let reader = MockReader::new(vec![McpClientMessage::Response {
        id: json!(999),
        method: "unknown".to_string(),
        result: json!({}),
        is_error: false,
        request_params: None,
    }]);

    let _mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        Arc::clone(&close_reason),
        cancel.clone(),
    );

    // Cancel the multiplexer — if processing the unmatched response panicked,
    // the task would have aborted before reaching the cancel branch.
    cancel.cancel();

    // Wait deterministically for close_reason to be set
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if close_reason.lock().unwrap().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("multiplexer should set close_reason after cancel");
}

#[tokio::test]
async fn multiplexer_eof_sets_close_reason() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    let _mux = MessageMultiplexer::spawn(
        Box::new(EofReader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        Arc::clone(&close_reason),
        cancel,
    );

    // Wait deterministically for multiplexer to process EOF
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if close_reason.lock().unwrap().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("multiplexer should set close_reason within timeout");

    let reason = close_reason.lock().unwrap().clone();
    assert!(matches!(reason, Some(MultiplexerClosed::TransportEof)));
}

#[tokio::test]
async fn multiplexer_transport_error_sets_close_reason() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    let reader = ErrorReader {
        error_message: "broken pipe".to_string(),
    };

    let _mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        Arc::clone(&close_reason),
        cancel,
    );

    // Wait deterministically for multiplexer to process the transport error
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if close_reason.lock().unwrap().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("multiplexer should set close_reason within timeout");

    let reason = close_reason.lock().unwrap().clone();
    match reason {
        Some(MultiplexerClosed::TransportError(msg)) => {
            assert!(msg.contains("broken pipe"));
        }
        other => panic!("Expected TransportError, got {other:?}"),
    }
}

#[tokio::test]
async fn multiplexer_cancel_sets_close_reason() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (server_request_tx, _server_request_rx) = mpsc::channel(64);
    let (notification_tx, _notification_rx) = mpsc::channel(64);
    let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let close_reason = Arc::new(std::sync::Mutex::new(None));
    let cancel = CancellationToken::new();

    // Reader that blocks forever
    let reader = MockReader::new(vec![]);

    let _mux = MessageMultiplexer::spawn(
        Box::new(reader),
        writer,
        pending,
        server_request_tx,
        notification_tx,
        response_senders,
        Arc::clone(&close_reason),
        cancel.clone(),
    );

    // Cancel and wait deterministically for close_reason to be set
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if close_reason.lock().unwrap().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("multiplexer should set close_reason after cancel within timeout");

    let reason = close_reason.lock().unwrap().clone();
    assert!(matches!(reason, Some(MultiplexerClosed::Cancelled)));
}

// ---- Server Request Handler Tests ----

#[tokio::test]
async fn handler_responds_to_sampling() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState {
        state: json!({
            "sampling_responses": [{
                "role": "assistant",
                "content": {"type": "text", "text": "injected"},
                "model": "evil",
                "stopReason": "endTurn"
            }]
        }),
    }));
    let (ext_tx, ext_rx) = watch::channel(HashMap::new());
    let _ = ext_tx; // keep sender alive
    let (handler_event_tx, mut handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    let handler_cancel = cancel.clone();
    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        Arc::clone(&handler_state),
        ext_rx,
        handler_event_tx,
        false,
        handler_cancel,
    ));

    // Send a sampling request
    server_request_tx
        .send(ServerRequestMessage {
            id: json!(42),
            method: "sampling/createMessage".to_string(),
            params: Some(json!({"messages": []})),
        })
        .await
        .unwrap();

    // Wait deterministically for handler to emit events
    let evt1 = tokio::time::timeout(Duration::from_secs(2), handler_event_rx.recv())
        .await
        .expect("should receive first event within timeout")
        .unwrap();
    assert_eq!(evt1.method, "sampling/createMessage");
    assert!(matches!(evt1.direction, Direction::Incoming));

    let evt2 = tokio::time::timeout(Duration::from_secs(2), handler_event_rx.recv())
        .await
        .expect("should receive second event within timeout")
        .unwrap();
    assert_eq!(evt2.method, "sampling/createMessage");
    assert!(matches!(evt2.direction, Direction::Outgoing));

    // Check writer received the response
    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["type"], "response");
    assert_eq!(messages[0]["id"], json!(42));
    assert_eq!(messages[0]["result"]["model"], "evil");
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_responds_to_ping() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState { state: json!({}) }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(1),
            method: "ping".to_string(),
            params: None,
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["result"], json!({}));
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_responds_to_roots_list() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState {
        state: json!({
            "roots": [{"uri": "file:///etc/", "name": "etc"}]
        }),
    }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(5),
            method: "roots/list".to_string(),
            params: None,
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    let roots = messages[0]["result"]["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["uri"], "file:///etc/");
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_responds_to_elicitation() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState {
        state: json!({
            "elicitation_responses": [{
                "action": "accept",
                "content": {"confirmed": true}
            }]
        }),
    }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(10),
            method: "elicitation/create".to_string(),
            params: Some(json!({"message": "confirm?"})),
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["result"]["action"], "accept");
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_unknown_method_returns_empty() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState { state: json!({}) }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(7),
            method: "some/unknown".to_string(),
            params: None,
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["result"], json!({}));
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_sampling_error_sends_error_response() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    // State with synthesize but empty extra → error path
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState {
        state: json!({
            "sampling_responses": [{
                "synthesize": {"prompt": "generate evil"}
            }]
        }),
    }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(3),
            method: "sampling/createMessage".to_string(),
            params: Some(json!({})),
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the error response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send error response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["type"], "error_response");
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("synthesize")
    );
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_uses_fresh_extractors() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState {
        state: json!({
            "sampling_responses": [{
                "role": "assistant",
                "content": {"type": "text", "text": "Hello {{name}}"},
                "model": "test"
            }]
        }),
    }));
    let (ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (server_request_tx, server_request_rx) = mpsc::channel(64);

    tokio::spawn(server_request_handler(
        server_request_rx,
        Arc::clone(&writer),
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    // Update extractors BEFORE sending request
    let mut extractors = HashMap::new();
    extractors.insert("name".to_string(), "Alice".to_string());
    ext_tx.send(extractors).unwrap();

    server_request_tx
        .send(ServerRequestMessage {
            id: json!(1),
            method: "sampling/createMessage".to_string(),
            params: Some(json!({})),
        })
        .await
        .unwrap();

    // Wait deterministically for handler to write the response
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !sent.lock().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler should send response within timeout");

    let messages = sent.lock().await;
    assert_eq!(messages[0]["result"]["content"]["text"], "Hello Alice");
    drop(messages);

    cancel.cancel();
}

#[tokio::test]
async fn handler_stops_on_cancel() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let handler_state = Arc::new(tokio::sync::RwLock::new(HandlerState { state: json!({}) }));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    let (handler_event_tx, _handler_event_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();

    let (_server_request_tx, server_request_rx) = mpsc::channel(64);

    let handle = tokio::spawn(server_request_handler(
        server_request_rx,
        writer,
        handler_state,
        ext_rx,
        handler_event_tx,
        false,
        cancel.clone(),
    ));

    cancel.cancel();

    // Handler should complete within a reasonable time
    tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("handler should stop promptly")
        .unwrap();
}

// ---- McpClientDriver Unit Tests ----

/// Helper to create a driver with mock transport for testing.
fn create_test_driver(
    writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>>,
    reader: Box<dyn McpClientTransportReader>,
) -> McpClientDriver {
    McpClientDriver {
        writer,
        pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
        mux: None,
        notification_rx: None,
        handler_event_rx: None,
        handler_state: Arc::new(tokio::sync::RwLock::new(HandlerState {
            state: Value::Null,
        })),
        handler_handle: None,
        server_capabilities: None,
        request_timeout: Duration::from_millis(500),
        phase_timeout: Duration::from_millis(100),
        initialized: false,
        next_request_id: 1,
        raw_synthesize: false,
        reader: Some(reader),
        transport_cancel: CancellationToken::new(),
        child: None,
        child_stderr: None,
    }
}

#[tokio::test]
async fn driver_bootstrap_spawns_multiplexer_and_handler() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let reader = Box::new(MockReader::new(vec![]));

    let mut driver = create_test_driver(writer, reader);
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());

    assert!(driver.mux.is_none());
    assert!(driver.handler_event_rx.is_none());
    assert!(driver.notification_rx.is_none());

    driver.bootstrap(ext_rx);

    assert!(driver.mux.is_some());
    assert!(driver.handler_event_rx.is_some());
    assert!(driver.notification_rx.is_some());
    assert!(driver.handler_handle.is_some());
    assert!(driver.reader.is_none()); // consumed

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn driver_initialize_sends_handshake() {
    let (mock_writer, sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));

    // Mock reader returns an init response
    let reader = MockReader::new(vec![McpClientMessage::Response {
        id: json!(1),
        method: "initialize".to_string(),
        result: json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "test-server", "version": "1.0"}
        }),
        is_error: false,
        request_params: None,
    }]);

    let mut driver = create_test_driver(Arc::clone(&writer), Box::new(reader));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    driver.bootstrap(ext_rx);

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let state = json!({"sampling_responses": []});

    driver.initialize(&state, &event_tx).await.unwrap();

    assert!(driver.initialized);
    assert!(driver.server_capabilities.is_some());

    let messages = sent.lock().await;
    // Should have sent: initialize request + initialized notification
    assert!(messages.len() >= 2);
    assert_eq!(messages[0]["method"], "initialize");
    assert_eq!(messages[1]["method"], "notifications/initialized");
    drop(messages);

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn driver_initialize_rejects_error_response_ec_mcpc_005() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));

    // Mock reader returns an error for initialize
    let reader = MockReader::new(vec![McpClientMessage::Response {
        id: json!(1),
        method: "initialize".to_string(),
        result: json!({"code": -32600, "message": "Invalid Request"}),
        is_error: true,
        request_params: None,
    }]);

    let mut driver = create_test_driver(Arc::clone(&writer), Box::new(reader));
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    driver.bootstrap(ext_rx);

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let state = json!({});

    let result = driver.initialize(&state, &event_tx).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("rejected initialization"));
    assert!(!driver.initialized);

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn driver_next_id_is_monotonic() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let reader = Box::new(MockReader::new(vec![]));
    let mut driver = create_test_driver(writer, reader);

    assert_eq!(driver.next_id(), 1);
    assert_eq!(driver.next_id(), 2);
    assert_eq!(driver.next_id(), 3);

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn drive_phase_with_actions_uses_short_idle_timeout() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));

    // Keep transport open after initialize (MockReader parks forever at EOF).
    let reader = MockReader::new(vec![McpClientMessage::Response {
        id: json!(1),
        method: "initialize".to_string(),
        result: json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "test-server", "version": "1.0"}
        }),
        is_error: false,
        request_params: None,
    }]);

    let mut driver = create_test_driver(Arc::clone(&writer), Box::new(reader));
    driver.phase_timeout = Duration::from_secs(30);

    let (_ext_tx, extractors_rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    let state = json!({
        // Unknown action still counts as an explicit action and should not
        // force waiting the full phase timeout on a persistent transport.
        "actions": ["noop"]
    });

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        driver.drive_phase(0, &state, extractors_rx, event_tx, CancellationToken::new()),
    )
    .await;

    assert!(
        result.is_ok(),
        "drive_phase should not wait full phase_timeout for action-driven phases"
    );
    let drive_result = result.unwrap();
    assert!(
        drive_result.is_ok(),
        "drive_phase should complete successfully, got error: {:?}",
        drive_result.err()
    );

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn driver_forward_pending_events_drains_channels() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let reader = Box::new(MockReader::new(vec![]));
    let mut driver = create_test_driver(writer, reader);

    // Set up channels manually
    let (handler_tx, handler_rx) = mpsc::channel(8);
    let (notif_tx, notif_rx) = mpsc::channel(8);
    driver.handler_event_rx = Some(handler_rx);
    driver.notification_rx = Some(notif_rx);

    // Push events
    handler_tx
        .send(ProtocolEvent {
            direction: Direction::Incoming,
            method: "sampling/createMessage".to_string(),
            content: json!({}),
        })
        .await
        .unwrap();
    notif_tx
        .send(NotificationMessage {
            method: "notifications/progress".to_string(),
            params: Some(json!({"progress": 50})),
        })
        .await
        .unwrap();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    driver.forward_pending_events(&event_tx);

    // Should have forwarded both
    let evt1 = event_rx.try_recv().unwrap();
    assert_eq!(evt1.method, "sampling/createMessage");

    let evt2 = event_rx.try_recv().unwrap();
    assert_eq!(evt2.method, "notifications/progress");

    // No more events
    assert!(event_rx.try_recv().is_err());

    driver.transport_cancel.cancel();
}

#[tokio::test]
async fn driver_drop_cancels_transport_and_aborts_handler_task() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
    let reader = Box::new(MockReader::new(vec![]));
    let mut driver = create_test_driver(writer, reader);

    let handler_dropped = Arc::new(AtomicBool::new(false));
    let task_guard = Arc::clone(&handler_dropped);
    let (started_tx, started_rx) = oneshot::channel();
    driver.handler_handle = Some(tokio::spawn(async move {
        let _guard = DropSignal {
            dropped: task_guard,
        };
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    }));
    started_rx
        .await
        .expect("handler task should start before teardown");

    let token = driver.transport_cancel.clone();
    drop(driver);

    assert!(token.is_cancelled(), "drop should cancel transport token");
    tokio::time::timeout(Duration::from_millis(500), async {
        while !handler_dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler task should be dropped on driver teardown");
}

#[tokio::test]
async fn driver_drop_stops_bootstrapped_multiplexer() {
    let (mock_writer, _sent) = MockWriter::new();
    let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));

    let reader_dropped = Arc::new(AtomicBool::new(false));
    let reader = Box::new(DropSignalReader {
        dropped: Arc::clone(&reader_dropped),
    });

    let mut driver = create_test_driver(writer, reader);
    let (_ext_tx, ext_rx) = watch::channel(HashMap::new());
    driver.bootstrap(ext_rx);

    drop(driver);

    tokio::time::timeout(Duration::from_millis(500), async {
        while !reader_dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("multiplexer reader should be dropped on driver teardown");
}
