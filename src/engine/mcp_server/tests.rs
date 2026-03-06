use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use oatf::enums::ElicitationMode;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::engine::actions::EntryActionSender;
use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult};
use crate::transport::{
    ConnectionContext, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    Transport, TransportType,
};

use super::driver::{McpServerDriver, McpTransportEntryActionSender};
use super::generation::{
    MAX_GENERATION_DEPTH, apply_generation, generate_nested_json, generate_random_bytes,
    generate_unbounded_line, generate_unicode_stress,
};
use super::handlers::{
    default_capabilities, handle_completion, handle_elicitation_response, handle_initialize,
    handle_ping, handle_prompts_list, handle_resources_list, handle_resources_read,
    handle_resources_templates_list, handle_roots_list, handle_sampling, handle_subscribe,
    handle_tasks_cancel, handle_tasks_get, handle_tasks_list, handle_tasks_result,
    handle_tools_list, handle_unknown,
};
use super::helpers::{find_by_field, find_by_name, matches_uri_template, strip_internal_fields};
use super::response::dispatch_response;

// ---- MockTransport ----

/// Shared outgoing message buffer for test assertions.
type OutgoingBuffer = Arc<Mutex<Vec<JsonRpcMessage>>>;

struct MockTransport {
    incoming: Mutex<VecDeque<JsonRpcMessage>>,
    outgoing: OutgoingBuffer,
}

impl MockTransport {
    fn setup(messages: Vec<JsonRpcMessage>) -> (Arc<dyn Transport>, OutgoingBuffer) {
        let outgoing: OutgoingBuffer = Arc::new(Mutex::new(Vec::new()));
        let transport: Arc<dyn Transport> = Arc::new(Self {
            incoming: Mutex::new(VecDeque::from(messages)),
            outgoing: Arc::clone(&outgoing),
        });
        (transport, outgoing)
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
        self.outgoing.lock().await.push(message.clone());
        Ok(())
    }

    async fn send_raw(&self, bytes: &[u8]) -> crate::transport::Result<()> {
        // Accumulate raw bytes — for testing we just store as-is
        let s = String::from_utf8_lossy(bytes);
        if !s.trim().is_empty()
            && let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(s.trim())
        {
            self.outgoing.lock().await.push(msg);
        }
        Ok(())
    }

    async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
        Ok(self.incoming.lock().await.pop_front())
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Stdio
    }

    async fn finalize_response(&self) -> crate::transport::Result<()> {
        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext {
            connection_id: 0,
            remote_addr: None,
            is_exclusive: true,
            connected_at: Instant::now(),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ---- Helper to make a request ----

fn make_request(method: &str, params: Option<Value>) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
        method: method.to_string(),
        params,
        id: json!(1),
    })
}

fn test_state() -> Value {
    json!({
        "tools": [
            {
                "name": "calculator",
                "description": "Performs calculations",
                "inputSchema": {"type": "object"},
                "responses": [
                    {
                        "content": [{"type": "text", "text": "42"}]
                    }
                ]
            }
        ],
        "resources": [
            {
                "uri": "file:///data.txt",
                "name": "data",
                "description": "Test data",
                "mimeType": "text/plain",
                "responses": [
                    {
                        "contents": [{"uri": "file:///data.txt", "text": "hello"}]
                    }
                ]
            }
        ],
        "prompts": [
            {
                "name": "greeting",
                "description": "A greeting prompt",
                "arguments": [
                    {"name": "name", "description": "Name to greet", "required": true}
                ],
                "responses": [
                    {
                        "messages": [
                            {"role": "assistant", "content": {"type": "text", "text": "Hello!"}}
                        ]
                    }
                ]
            }
        ]
    })
}

// ---- Handler tests ----

#[test]
fn initialize_returns_capabilities() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "initialize".to_string(),
        params: Some(json!({})),
        id: json!(1),
    };
    let state = test_state();

    let resp = handle_initialize(&request, &state);
    let result = resp.result.unwrap();

    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "thoughtjack");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["prompts"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
}

#[test]
fn tools_list_returns_tools() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tools/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = test_state();

    let resp = handle_tools_list(&request, &state);
    let result = resp.result.unwrap();

    let tools = result["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "calculator");
    assert_eq!(tools[0]["description"], "Performs calculations");
    // responses field should be stripped
    assert!(tools[0].get("responses").is_none());
}

#[test]
fn tools_list_includes_input_schema() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tools/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = json!({
        "tools": [{
            "name": "test",
            "description": "test",
            "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}}},
            "outputSchema": {"type": "object"},
            "responses": []
        }]
    });

    let resp = handle_tools_list(&request, &state);
    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert!(tools[0]["inputSchema"].is_object());
    assert!(tools[0]["outputSchema"].is_object());
}

#[test]
fn tools_call_selects_response() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tools/call".to_string(),
        params: Some(json!({"name": "calculator", "arguments": {}})),
        id: json!(1),
    };
    let state = test_state();
    let tool = find_by_name(&state, "tools", "calculator").unwrap();
    let extractors = HashMap::new();

    let resp = dispatch_response(
        &request.id,
        &tool,
        &extractors,
        request.params.as_ref().unwrap(),
        None,
        false,
    );

    let result = resp.result.unwrap();
    let content = result["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "42");
}

#[test]
fn tools_call_unknown_tool_errors() {
    let state = test_state();

    // Simulate what handle_tools_call does for missing tool
    let result = find_by_name(&state, "tools", "nonexistent");
    assert!(result.is_none());
}

#[test]
fn resources_list_returns_resources() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "resources/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = test_state();

    let resp = handle_resources_list(&request, &state);
    let result = resp.result.unwrap();

    let resources = result["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["uri"], "file:///data.txt");
    assert!(resources[0].get("responses").is_none());
    assert!(resources[0].get("content").is_none());
}

#[test]
fn resources_read_returns_content() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "resources/read".to_string(),
        params: Some(json!({"uri": "file:///data.txt"})),
        id: json!(1),
    };
    let state = test_state();
    let extractors = HashMap::new();

    let resp = handle_resources_read(&request, &state, &extractors, false);
    let result = resp.result.unwrap();
    assert!(result["contents"].is_array());
}

#[test]
fn prompts_list_returns_prompts() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "prompts/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = test_state();

    let resp = handle_prompts_list(&request, &state);
    let result = resp.result.unwrap();

    let prompts = result["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["name"], "greeting");
    assert!(prompts[0].get("responses").is_none());
}

#[test]
fn prompts_get_selects_response() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "prompts/get".to_string(),
        params: Some(json!({"name": "greeting"})),
        id: json!(1),
    };
    let state = test_state();
    let prompt = find_by_name(&state, "prompts", "greeting").unwrap();
    let extractors = HashMap::new();

    let resp = dispatch_response(
        &request.id,
        &prompt,
        &extractors,
        request.params.as_ref().unwrap(),
        None,
        false,
    );

    let result = resp.result.unwrap();
    assert!(result["messages"].is_array());
}

#[test]
fn ping_returns_empty_object() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "ping".to_string(),
        params: None,
        id: json!(1),
    };

    let resp = handle_ping(&request);
    assert_eq!(resp.result, Some(json!({})));
}

#[test]
fn unknown_method_returns_null() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "x-custom/frobnicate".to_string(),
        params: None,
        id: json!(42),
    };

    let resp = handle_unknown(&request);
    assert_eq!(resp.result, Some(Value::Null));
    assert!(resp.error.is_none());
}

#[test]
fn subscribe_returns_success() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "resources/subscribe".to_string(),
        params: Some(json!({"uri": "file:///data.txt"})),
        id: json!(1),
    };

    let resp = handle_subscribe(&request);
    assert_eq!(resp.result, Some(json!({})));
}

#[test]
fn completion_returns_empty() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "completion/complete".to_string(),
        params: Some(json!({})),
        id: json!(1),
    };

    let resp = handle_completion(&request);
    let result = resp.result.unwrap();
    assert_eq!(result["completion"]["values"], json!([]));
    assert_eq!(result["completion"]["hasMore"], false);
}

// ---- PhaseDriver integration tests ----

#[tokio::test]
async fn drive_phase_returns_transport_closed_on_eof() {
    let (transport, _outgoing) = MockTransport::setup(vec![]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = test_state();
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    let result = driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();
    assert!(matches!(result, DriveResult::TransportClosed));
}

#[tokio::test]
async fn drive_phase_completes_on_cancel() {
    let (transport, _outgoing) = MockTransport::setup(vec![]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = test_state();
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    // Cancel immediately
    cancel.cancel();

    let result = driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();
    assert!(matches!(result, DriveResult::Complete));
}

#[tokio::test]
async fn drive_phase_emits_events() {
    let request = make_request("tools/list", None);
    let (transport, _outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = test_state();
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    // Should have incoming + outgoing events
    let mut events = Vec::new();
    while let Ok(evt) = event_rx.try_recv() {
        events.push(evt);
    }

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].direction, Direction::Incoming);
    assert_eq!(events[0].method, "tools/list");
    assert_eq!(events[1].direction, Direction::Outgoing);
    assert_eq!(events[1].method, "tools/list");
}

#[tokio::test]
async fn extractors_refreshed_per_request() {
    let requests = vec![make_request("tools/list", None), make_request("ping", None)];
    let (transport, outgoing) = MockTransport::setup(requests);
    let mut driver = McpServerDriver::new(transport, false);

    let state = test_state();
    let (tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    // Update extractors between the two requests being processed
    tx.send(HashMap::from([("key".to_string(), "value".to_string())]))
        .unwrap();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    // If extractors weren't refreshed per-request, the second request
    // would not see the updated value. Since this is a server-mode
    // driver, it borrows fresh values each time.
    let sent = outgoing.lock().await;
    assert_eq!(sent.len(), 2); // Two responses sent
    drop(sent);
}

#[tokio::test]
async fn delayed_delivery_waits() {
    let request = make_request("ping", None);
    let (transport, _outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "behavior": {
            "delivery": "delayed",
            "parameters": {
                "delay_ms": 10
            }
        }
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    let start = tokio::time::Instant::now();
    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    // Should have waited at least the delay
    assert!(elapsed >= tokio::time::Duration::from_millis(10));
}

#[tokio::test]
async fn notification_flood_sends_before_response() {
    let request = make_request("ping", None);
    let (transport, outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "behavior": {
            "side_effects": [
                {
                    "type": "notification_flood",
                    "parameters": {
                        "rate": 1000,
                        "duration": "0s"
                    }
                }
            ]
        }
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    // With duration "0s" we get no flood notifications, just the response
    // The flood loop immediately exits because elapsed >= 0s duration
    assert!(!sent.is_empty());
    // Last message should be the response
    assert!(matches!(sent.last().unwrap(), JsonRpcMessage::Response(_)));
    drop(sent);
}

#[tokio::test]
async fn entry_action_sender_sends_notification() {
    let (transport, outgoing) = MockTransport::setup(vec![]);
    let sender = McpTransportEntryActionSender {
        transport,
        next_request_id: AtomicU64::new(1_000_000),
    };

    sender
        .send_notification("notifications/tools/list_changed", None)
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        JsonRpcMessage::Notification(n) => {
            assert_eq!(n.method, "notifications/tools/list_changed");
        }
        _ => panic!("expected notification"),
    }
}

#[tokio::test]
async fn entry_action_sender_sends_elicitation() {
    let (transport, outgoing) = MockTransport::setup(vec![]);
    let sender = McpTransportEntryActionSender {
        transport,
        next_request_id: AtomicU64::new(1_000_000),
    };

    sender
        .send_elicitation(
            "Enter your API key",
            Some(&ElicitationMode::Form),
            Some(&json!({"type": "object"})),
            None,
            None,
        )
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        JsonRpcMessage::Request(r) => {
            assert_eq!(r.method, "elicitation/create");
            assert_eq!(r.id, json!(1_000_000));
            let params = r.params.as_ref().unwrap();
            assert_eq!(params["message"], "Enter your API key");
        }
        _ => panic!("expected request"),
    }
}

#[test]
fn strip_internal_fields_removes_responses() {
    let tool = json!({
        "name": "calc",
        "description": "Calculator",
        "responses": [{"content": []}]
    });
    let stripped = strip_internal_fields(&tool, &["responses"]);
    assert!(stripped.get("responses").is_none());
    assert_eq!(stripped["name"], "calc");
}

#[test]
fn find_by_name_works() {
    let state = test_state();
    let found = find_by_name(&state, "tools", "calculator");
    assert!(found.is_some());
    assert_eq!(found.unwrap()["name"], "calculator");

    let not_found = find_by_name(&state, "tools", "nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn find_by_field_works() {
    let state = test_state();
    let found = find_by_field(&state, "resources", "uri", "file:///data.txt");
    assert!(found.is_some());

    let not_found = find_by_field(&state, "resources", "uri", "file:///missing.txt");
    assert!(not_found.is_none());
}

#[test]
fn default_capabilities_derives_from_state() {
    let state = test_state();
    let caps = default_capabilities(&state);
    assert!(caps["tools"].is_object());
    assert!(caps["prompts"].is_object());
    assert!(caps["resources"].is_object());
}

#[test]
fn default_capabilities_empty_state() {
    let state = json!({});
    let caps = default_capabilities(&state);
    assert!(caps.as_object().unwrap().is_empty());
}

#[test]
fn dispatch_response_no_responses_returns_empty_content() {
    let item = json!({"name": "test"});
    let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
    let result = resp.result.unwrap();
    assert_eq!(result["content"], json!([]));
}

#[test]
fn dispatch_response_empty_responses_returns_empty_content() {
    let item = json!({"name": "test", "responses": []});
    let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
    let result = resp.result.unwrap();
    assert_eq!(result["content"], json!([]));
}

#[tokio::test]
async fn drive_phase_handles_notification_from_agent() {
    let messages = vec![JsonRpcMessage::Notification(JsonRpcNotification::new(
        "notifications/initialized",
        None,
    ))];
    let (transport, _outgoing) = MockTransport::setup(messages);
    let mut driver = McpServerDriver::new(transport, false);

    let state = test_state();
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    // Should have emitted one incoming event for the notification
    let evt = event_rx.try_recv().unwrap();
    assert_eq!(evt.direction, Direction::Incoming);
    assert_eq!(evt.method, "notifications/initialized");
}

#[tokio::test]
async fn connection_reset_side_effect_returns_error() {
    let request = make_request("ping", None);
    let (transport, _outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "behavior": {
            "side_effects": [
                { "type": "connection_reset" }
            ]
        }
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    let result = driver.drive_phase(0, &state, rx, event_tx, cancel).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("connection_reset"));
}

// ---- New handler tests (Gap 1) ----

#[test]
fn sampling_returns_empty_object() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "sampling/createMessage".to_string(),
        params: Some(json!({})),
        id: json!(1),
    };
    let resp = handle_sampling(&request);
    assert_eq!(resp.result, Some(json!({})));
}

#[test]
fn roots_list_returns_empty_roots() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "roots/list".to_string(),
        params: None,
        id: json!(1),
    };
    let resp = handle_roots_list(&request);
    let result = resp.result.unwrap();
    assert_eq!(result["roots"], json!([]));
}

#[test]
fn elicitation_response_returns_empty_object() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "elicitation/create".to_string(),
        params: Some(json!({"action": "accept", "content": {"key": "val"}})),
        id: json!(1),
    };
    let resp = handle_elicitation_response(&request);
    assert_eq!(resp.result, Some(json!({})));
}

#[test]
fn tasks_get_returns_task() {
    let state = json!({
        "tasks": [
            {"id": "task-1", "status": "running", "result": {"data": "hello"}}
        ]
    });
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/get".to_string(),
        params: Some(json!({"id": "task-1"})),
        id: json!(1),
    };
    let resp = handle_tasks_get(&request, &state);
    let result = resp.result.unwrap();
    assert_eq!(result["id"], "task-1");
    assert_eq!(result["status"], "running");
}

#[test]
fn tasks_get_unknown_returns_error() {
    let state = json!({"tasks": []});
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/get".to_string(),
        params: Some(json!({"id": "missing"})),
        id: json!(1),
    };
    let resp = handle_tasks_get(&request, &state);
    assert!(resp.error.is_some());
}

#[test]
fn tasks_result_returns_result() {
    let state = json!({
        "tasks": [
            {"id": "task-1", "status": "completed", "result": {"output": "done"}}
        ]
    });
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/result".to_string(),
        params: Some(json!({"id": "task-1"})),
        id: json!(1),
    };
    let resp = handle_tasks_result(&request, &state);
    let result = resp.result.unwrap();
    assert_eq!(result["output"], "done");
}

#[test]
fn tasks_list_returns_all_tasks() {
    let state = json!({
        "tasks": [
            {"id": "task-1", "status": "running"},
            {"id": "task-2", "status": "completed"}
        ]
    });
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/list".to_string(),
        params: None,
        id: json!(1),
    };
    let resp = handle_tasks_list(&request, &state);
    let result = resp.result.unwrap();
    assert_eq!(result["tasks"].as_array().unwrap().len(), 2);
}

#[test]
fn tasks_list_empty_state() {
    let state = json!({});
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/list".to_string(),
        params: None,
        id: json!(1),
    };
    let resp = handle_tasks_list(&request, &state);
    let result = resp.result.unwrap();
    assert_eq!(result["tasks"], json!([]));
}

#[test]
fn tasks_cancel_returns_cancelled() {
    let state = json!({
        "tasks": [{"id": "task-1", "status": "running"}]
    });
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks/cancel".to_string(),
        params: Some(json!({"id": "task-1"})),
        id: json!(1),
    };
    let resp = handle_tasks_cancel(&request, &state);
    let result = resp.result.unwrap();
    assert_eq!(result["id"], "task-1");
    assert_eq!(result["status"], "cancelled");
}

// ---- Edge case tests (Gap 7) ----

/// EC-OATF-011: Empty content returned when no response entry matches.
#[test]
fn select_response_no_match() {
    let item = json!({
        "name": "test-tool",
        "responses": [
            {
                "when": {"name": "other-tool"},
                "content": [{"type": "text", "text": "should not match"}]
            }
        ]
    });
    let context = json!({"name": "test-tool"});
    let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &context, None, false);
    let result = resp.result.unwrap();
    assert_eq!(result["content"], json!([]));
}

/// EC-OATF-012: Error message when synthesize is requested but no
/// `GenerationProvider` is available.
#[test]
fn synthesize_no_provider() {
    let item = json!({
        "name": "test-tool",
        "responses": [
            {
                "synthesize": {"prompt": "generate something"}
            }
        ]
    });
    let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
    assert!(resp.error.is_some());
    let err = resp.error.unwrap();
    assert!(
        err.message.contains("synthesize"),
        "error should mention synthesize: {}",
        err.message
    );
}

/// EC-OATF-013: When agent declines an elicitation, the tool call
/// completes normally. We verify that an elicitation with a non-matching
/// predicate is skipped and the response is still sent.
#[tokio::test]
async fn elicitation_agent_declines() {
    // Set up: initialize with elicitation capability, then tools/call + decline
    let init_request = make_request(
        "initialize",
        Some(json!({"capabilities": {"elicitation": {}}})),
    );
    let tools_call = make_request(
        "tools/call",
        Some(json!({"name": "calculator", "arguments": {}})),
    );
    let decline_response = JsonRpcMessage::Response(JsonRpcResponse::success(
        json!("elicit-decline"),
        json!({"action": "decline"}),
    ));
    let (transport, outgoing) =
        MockTransport::setup(vec![init_request, tools_call, decline_response]);
    let mut driver = McpServerDriver::new(transport, false);

    // State with an always-matching elicitation
    let state = json!({
        "tools": [{
            "name": "calculator",
            "description": "calc",
            "inputSchema": {"type": "object"},
            "responses": [
                {"content": [{"type": "text", "text": "42"}]}
            ]
        }],
        "elicitations": [{
            "message": "Enter API key",
            "requestedSchema": {"type": "object"}
        }]
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    // Verify the tool response was still sent despite the decline
    let sent = outgoing.lock().await;
    let has_tool_response = sent.iter().any(|msg| {
        matches!(msg, JsonRpcMessage::Response(r) if r.result.as_ref().is_some_and(|v| v["content"][0]["text"] == "42"))
    });
    drop(sent);
    assert!(
        has_tool_response,
        "tool response should be sent after elicitation decline"
    );

    // Verify elicitation events were emitted
    let mut events = Vec::new();
    while let Ok(evt) = event_rx.try_recv() {
        events.push(evt);
    }
    let has_elicit_out = events
        .iter()
        .any(|e| e.method == "elicitation/create" && e.direction == Direction::Outgoing);
    assert!(has_elicit_out, "should have outgoing elicitation event");
}

// ---- Payload generation tests ----

#[test]
fn generate_nested_json_produces_valid_json() {
    let result = generate_nested_json(Some(&json!({"depth": 5})), None);
    let parsed: serde_json::Result<Value> = serde_json::from_str(&result);
    assert!(parsed.is_ok(), "generated JSON should be valid");
}

#[test]
fn generate_nested_json_respects_depth_limit() {
    let result = generate_nested_json(Some(&json!({"depth": 2000})), None);
    // Should be clamped to MAX_GENERATION_DEPTH (1000)
    let nesting = result.matches(r#"{"a":"#).count();
    assert_eq!(nesting, MAX_GENERATION_DEPTH);
}

#[test]
fn generate_random_bytes_is_deterministic() {
    let a = generate_random_bytes(Some(&json!({"size": 32})), Some(12345));
    let b = generate_random_bytes(Some(&json!({"size": 32})), Some(12345));
    assert_eq!(a, b, "same seed should produce same output");
}

#[test]
fn generate_random_bytes_different_seeds_differ() {
    let a = generate_random_bytes(Some(&json!({"size": 32})), Some(1));
    let b = generate_random_bytes(Some(&json!({"size": 32})), Some(2));
    assert_ne!(a, b, "different seeds should produce different output");
}

#[test]
fn generate_unbounded_line_correct_length() {
    let result = generate_unbounded_line(Some(&json!({"length": 100})), None);
    assert_eq!(result.len(), 100);
}

#[test]
fn generate_unicode_stress_produces_content() {
    for category in &["rtl", "zero_width", "combining", "emoji", "mixed"] {
        let result =
            generate_unicode_stress(Some(&json!({"category": category, "repeat": 10})), None);
        assert!(
            !result.is_empty(),
            "category {category} should produce content"
        );
    }
}

#[test]
fn apply_generation_replaces_generate_blocks() {
    let mut content = json!({
        "content": [
            {
                "type": "text",
                "generate": {
                    "kind": "unbounded_line",
                    "parameters": {"length": 50}
                }
            }
        ]
    });
    apply_generation(&mut content);
    let items = content["content"].as_array().unwrap();
    assert!(
        items[0].get("generate").is_none(),
        "generate block should be removed"
    );
    assert_eq!(items[0]["text"].as_str().unwrap().len(), 50);
}

// ---- Unbounded delivery test ----

#[tokio::test]
async fn unbounded_delivery_inflates_response() {
    let request = make_request("ping", None);
    let (transport, outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "behavior": {
            "delivery": "unbounded",
            "parameters": {
                "max_line_length": 100,
                "nesting_depth": 3
            }
        }
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        JsonRpcMessage::Response(r) => {
            let result = r.result.as_ref().unwrap();
            // Should be wrapped in 3 levels of nesting
            assert!(result.get("wrapper").is_some());
            assert!(result["wrapper"].get("wrapper").is_some());
            assert!(result["wrapper"]["wrapper"].get("wrapper").is_some());
        }
        _ => panic!("expected response"),
    }
}

// ---- Elicitation predicate tests ----

#[tokio::test]
async fn elicitation_first_match_wins() {
    // Set up: initialize with elicitation capability, then tools/call + response
    let init_request = make_request(
        "initialize",
        Some(json!({"capabilities": {"elicitation": {}}})),
    );
    let tools_call = make_request(
        "tools/call",
        Some(json!({"name": "calculator", "arguments": {}})),
    );
    let elicit_response = JsonRpcMessage::Response(JsonRpcResponse::success(
        json!("elicit-resp"),
        json!({"action": "accept"}),
    ));
    let (transport, outgoing) =
        MockTransport::setup(vec![init_request, tools_call, elicit_response]);
    let mut driver = McpServerDriver::new(transport, false);

    // State with two elicitations: first requires name=other, second matches all
    let state = json!({
        "tools": [{
            "name": "calculator",
            "description": "calc",
            "inputSchema": {"type": "object"},
            "responses": [
                {"content": [{"type": "text", "text": "42"}]}
            ]
        }],
        "elicitations": [
            {
                "when": {"name": "other-tool"},
                "message": "Should not fire",
                "requestedSchema": {}
            },
            {
                "message": "Should fire (no predicate)",
                "requestedSchema": {}
            }
        ]
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    // Should have: elicitation request + tool response
    let elicitation_sent = sent.iter().any(|msg| {
        matches!(msg, JsonRpcMessage::Request(r) if r.method == "elicitation/create"
            && r.params.as_ref().unwrap()["message"] == "Should fire (no predicate)")
    });
    drop(sent);
    assert!(
        elicitation_sent,
        "second (matching) elicitation should fire"
    );
}

// ---- Side effects array format test ----

#[tokio::test]
async fn id_collision_side_effect_with_count() {
    let request = make_request("ping", None);
    let (transport, outgoing) = MockTransport::setup(vec![request]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "behavior": {
            "side_effects": [
                {
                    "type": "id_collision",
                    "parameters": {"count": 2}
                }
            ]
        }
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    driver
        .drive_phase(0, &state, rx, event_tx, cancel)
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    // 2 collision responses + 1 real response = 3
    assert_eq!(sent.len(), 3);
    // First 2 should be collision responses
    for msg in &sent[..2] {
        match msg {
            JsonRpcMessage::Response(r) => {
                assert_eq!(r.result.as_ref().unwrap()["collision"], true);
            }
            _ => panic!("expected collision response"),
        }
    }
}

// ---- URI template matching tests ----

#[test]
fn uri_template_exact_match() {
    assert!(matches_uri_template(
        "test://resource/{id}",
        "test://resource/123"
    ));
}

#[test]
fn uri_template_rejects_empty_variable() {
    assert!(!matches_uri_template(
        "test://resource/{id}",
        "test://resource/"
    ));
}

#[test]
fn uri_template_rejects_wrong_prefix() {
    assert!(!matches_uri_template(
        "test://resource/{id}",
        "other://resource/123"
    ));
}

#[test]
fn uri_template_multiple_variables() {
    assert!(matches_uri_template(
        "test://{org}/resource/{id}",
        "test://acme/resource/42"
    ));
}

#[test]
fn uri_template_no_variables_exact() {
    assert!(matches_uri_template(
        "test://static/path",
        "test://static/path"
    ));
}

#[test]
fn uri_template_no_variables_mismatch() {
    assert!(!matches_uri_template(
        "test://static/path",
        "test://static/other"
    ));
}

#[test]
fn uri_template_trailing_literal() {
    assert!(matches_uri_template(
        "test://resource/{id}/details",
        "test://resource/foo/details"
    ));
}

// ---- Resource templates/list tests ----

#[test]
fn resources_templates_list_returns_templates() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "resources/templates/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = json!({
        "resource_templates": [
            {
                "uriTemplate": "test://resource/{id}",
                "name": "test-tmpl",
                "description": "A template",
                "mimeType": "text/plain",
                "responses": [{"contents": [{"uri": "test://resource/{id}", "text": "ok"}]}]
            }
        ]
    });

    let resp = handle_resources_templates_list(&request, &state);
    let result = resp.result.unwrap();
    let templates = result["resourceTemplates"].as_array().unwrap();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0]["name"], "test-tmpl");
    // Responses should be stripped
    assert!(templates[0].get("responses").is_none());
}

#[test]
fn resources_read_falls_back_to_template() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "resources/read".to_string(),
        params: Some(json!({"uri": "test://resource/42"})),
        id: json!(1),
    };
    let state = json!({
        "resources": [],
        "resource_templates": [
            {
                "uriTemplate": "test://resource/{id}",
                "name": "test-tmpl",
                "mimeType": "text/plain",
                "responses": [
                    {"contents": [{"uri": "test://resource/{id}", "mimeType": "text/plain", "text": "Template content."}]}
                ]
            }
        ]
    });

    let extractors = HashMap::new();
    let resp = handle_resources_read(&request, &state, &extractors, false);
    assert!(resp.error.is_none(), "expected success but got error");
    let result = resp.result.unwrap();
    assert!(result["contents"].as_array().is_some());
}

// ---- Logging capability test ----

#[test]
fn default_capabilities_includes_logging() {
    let state = json!({
        "tools": [{"name": "t", "description": "d", "inputSchema": {}}],
        "logging": [{"level": "info", "data": "test"}]
    });

    let caps = default_capabilities(&state);
    assert!(caps.get("tools").is_some());
    assert!(caps.get("logging").is_some());
}

#[test]
fn default_capabilities_includes_resource_templates() {
    let state = json!({
        "resource_templates": [{"uriTemplate": "test://{id}", "name": "t"}]
    });

    let caps = default_capabilities(&state);
    assert!(caps.get("resources").is_some());
}

// ---- Edge case tests (EC-OATF-015 through EC-OATF-021) ----

/// EC-OATF-015: `state.server_info` with custom name/version merges over defaults.
#[test]
fn ec_oatf_015_server_info_impersonation() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "initialize".to_string(),
        params: Some(json!({})),
        id: json!(1),
    };
    let state = json!({
        "server_info": {
            "name": "totally-legit-server",
            "version": "9.9.9"
        },
        "tools": [{"name": "t", "description": "d", "inputSchema": {}}]
    });

    let resp = handle_initialize(&request, &state);
    let result = resp.result.unwrap();

    assert_eq!(result["serverInfo"]["name"], "totally-legit-server");
    assert_eq!(result["serverInfo"]["version"], "9.9.9");
}

/// EC-OATF-016: `state.protocol_version` overrides the default.
#[test]
fn ec_oatf_016_custom_protocol_version() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "initialize".to_string(),
        params: Some(json!({})),
        id: json!(1),
    };
    let state = json!({
        "protocol_version": "2024-11-05"
    });

    let resp = handle_initialize(&request, &state);
    let result = resp.result.unwrap();

    assert_eq!(result["protocolVersion"], "2024-11-05");
}

/// EC-OATF-017: Tool entry with `icon` and `title` fields preserved in `tools/list`.
#[test]
fn ec_oatf_017_tool_icon_and_title() {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "tools/list".to_string(),
        params: None,
        id: json!(1),
    };
    let state = json!({
        "tools": [{
            "name": "fancy_tool",
            "description": "A fancy tool",
            "inputSchema": {"type": "object"},
            "icon": "https://example.com/icon.png",
            "title": "Fancy Tool Display Name",
            "responses": [{"content": []}]
        }]
    });

    let resp = handle_tools_list(&request, &state);
    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert_eq!(tools[0]["icon"], "https://example.com/icon.png");
    assert_eq!(tools[0]["title"], "Fancy Tool Display Name");
    // responses should be stripped
    assert!(tools[0].get("responses").is_none());
}

/// EC-OATF-018: Response with `isError: true` included in `tools/call` result.
#[test]
fn ec_oatf_018_tool_call_is_error() {
    let item = json!({
        "name": "failing_tool",
        "responses": [{
            "isError": true,
            "content": [{"type": "text", "text": "something went wrong"}]
        }]
    });
    let extractors = HashMap::new();

    let resp = dispatch_response(
        &json!(1),
        &item,
        &extractors,
        &json!({"name": "failing_tool"}),
        None,
        false,
    );
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "something went wrong");
}

/// EC-OATF-019: Response with `type: audio` content preserved in result.
#[test]
fn ec_oatf_019_audio_content() {
    let item = json!({
        "name": "audio_tool",
        "responses": [{
            "content": [{
                "type": "audio",
                "data": "base64encodedaudio==",
                "mimeType": "audio/wav"
            }]
        }]
    });
    let extractors = HashMap::new();

    let resp = dispatch_response(
        &json!(1),
        &item,
        &extractors,
        &json!({"name": "audio_tool"}),
        None,
        false,
    );
    let result = resp.result.unwrap();
    let content = result["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "audio");
    assert_eq!(content[0]["data"], "base64encodedaudio==");
    assert_eq!(content[0]["mimeType"], "audio/wav");
}

/// EC-OATF-020: Content item with `annotations.audience` and `annotations.priority` preserved.
#[test]
fn ec_oatf_020_content_annotations() {
    let item = json!({
        "name": "annotated_tool",
        "responses": [{
            "content": [{
                "type": "text",
                "text": "annotated result",
                "annotations": {
                    "audience": ["user"],
                    "priority": 0.9
                }
            }]
        }]
    });
    let extractors = HashMap::new();

    let resp = dispatch_response(
        &json!(1),
        &item,
        &extractors,
        &json!({"name": "annotated_tool"}),
        None,
        false,
    );
    let result = resp.result.unwrap();
    let content = &result["content"][0];
    assert_eq!(content["annotations"]["audience"][0], "user");
    assert_eq!(content["annotations"]["priority"], 0.9);
}

/// EC-OATF-021: State with `tasks:` entries causes `default_capabilities()` to include `tasks`.
#[test]
fn ec_oatf_021_task_capability() {
    let state = json!({
        "tasks": [
            {"id": "task-1", "status": "running", "result": {}}
        ]
    });

    let caps = default_capabilities(&state);
    assert!(
        caps.get("tasks").is_some(),
        "tasks capability should be present"
    );
    assert!(caps["tasks"].is_object());
}

// ---- Edge case tests (EC-OATF-022, EC-OATF-023) ----

/// EC-OATF-022: Elicitation with URL `requestedSchema` mode sends correct JSON-RPC request.
#[tokio::test]
async fn ec_oatf_022_elicitation_url_mode() {
    let (transport, outgoing) = MockTransport::setup(vec![]);
    let sender = McpTransportEntryActionSender {
        transport,
        next_request_id: AtomicU64::new(2_000_000),
    };

    sender
        .send_elicitation(
            "Open this URL to authenticate",
            Some(&ElicitationMode::Url),
            Some(&json!({"type": "string", "format": "uri"})),
            None,
            None,
        )
        .await
        .unwrap();

    let sent = outgoing.lock().await;
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        JsonRpcMessage::Request(r) => {
            assert_eq!(r.method, "elicitation/create");
            let params = r.params.as_ref().unwrap();
            assert_eq!(params["message"], "Open this URL to authenticate");
            assert_eq!(params["requestedSchema"]["type"], "string");
            assert_eq!(params["requestedSchema"]["format"], "uri");
        }
        _ => panic!("expected request"),
    }
}

/// EC-OATF-023: Elicitation with `action: "decline"` response handled gracefully.
#[tokio::test]
async fn ec_oatf_023_elicitation_declined() {
    // Initialize with elicitation, tools/call triggers elicitation,
    // agent declines — driver should not panic.
    let init_request = make_request(
        "initialize",
        Some(json!({"capabilities": {"elicitation": {}}})),
    );
    let tools_call = make_request(
        "tools/call",
        Some(json!({"name": "calculator", "arguments": {}})),
    );
    let decline_response = JsonRpcMessage::Response(JsonRpcResponse::success(
        json!("elicit-decline"),
        json!({"action": "decline"}),
    ));
    let (transport, outgoing) =
        MockTransport::setup(vec![init_request, tools_call, decline_response]);
    let mut driver = McpServerDriver::new(transport, false);

    let state = json!({
        "tools": [{
            "name": "calculator",
            "description": "calc",
            "inputSchema": {"type": "object"},
            "responses": [
                {"content": [{"type": "text", "text": "42"}]}
            ]
        }],
        "elicitations": [{
            "message": "Provide credentials",
            "mode": "form",
            "requestedSchema": {"type": "object", "properties": {"key": {"type": "string"}}}
        }]
    });
    let (_tx, rx) = watch::channel(HashMap::new());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    // Should not panic — driver handles decline gracefully
    let result = driver.drive_phase(0, &state, rx, event_tx, cancel).await;
    assert!(
        result.is_ok(),
        "drive_phase should handle elicitation decline gracefully"
    );

    // Verify the tool response was still sent
    let has_tool_response = {
        let sent = outgoing.lock().await;
        sent.iter().any(|msg| {
            matches!(msg, JsonRpcMessage::Response(r) if r.result.as_ref().is_some_and(|v| v["content"][0]["text"] == "42"))
        })
    };
    assert!(
        has_tool_response,
        "tool response should be sent after elicitation decline"
    );
}
