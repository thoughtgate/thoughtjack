//! A2A client ↔ HTTP mock integration tests.
//!
//! Each test starts an axum mock server simulating an A2A agent, creates
//! an `A2aClientDriver`, wires it into a `PhaseLoop`, and verifies the
//! trace.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use thoughtjack::engine::{
    ExtractorStore, PhaseEngine, PhaseLoop, PhaseLoopConfig, SharedTrace, TerminationReason,
};
use thoughtjack::observability::EventEmitter;
use thoughtjack::protocol::a2a_client::create_a2a_client_driver;

use crate::common::mock_server::MockServer;

/// Helper: load an oatf Document from inline YAML.
fn load_doc(yaml: &str) -> oatf::Document {
    oatf::load(yaml)
        .expect("test YAML should be valid")
        .document
}

/// Helper: build a default `PhaseLoopConfig`.
fn test_config(trace: SharedTrace) -> PhaseLoopConfig {
    PhaseLoopConfig {
        trace,
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: CancellationToken::new(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
        a2a_skill_tx: None,
        context_mode: false,
    }
}

/// Creates an axum Router that serves a mock A2A agent.
/// Returns an Agent Card on GET /.well-known/agent.json and handles
/// POST / for JSON-RPC dispatch.
fn mock_a2a_router(task_response: serde_json::Value) -> Router {
    let agent_card = json!({
        "name": "Mock Agent",
        "description": "A mock A2A agent for testing",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {},
        "skills": [
            {
                "id": "general",
                "name": "General",
                "description": "General purpose"
            }
        ],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let resp = task_response.clone();
                async move { axum::Json(resp) }
            }),
        )
}

// ============================================================================
// 1. Send task — basic happy path
// ============================================================================

#[tokio::test]
async fn a2a_client_send_task() {
    let task_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "kind": "task",
            "id": "task-1",
            "contextId": "ctx-1",
            "status": {
                "state": "completed"
            },
            "artifacts": [
                {
                    "parts": [{"kind": "text", "text": "Hello back!"}]
                }
            ]
        }
    });

    let router = mock_a2a_router(task_response);
    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_client_send
  execution:
    mode: a2a_client
    state:
      fetch_agent_card: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Hello"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    // Should have: outgoing agent_card/get, incoming agent_card/get,
    // outgoing message/send, incoming task response
    assert!(
        entries.len() >= 4,
        "expected ≥4 trace entries, got {}",
        entries.len()
    );

    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"agent_card/get"));
    assert!(methods.contains(&"message/send"));
}

// ============================================================================
// 2. Streaming with final: true
// ============================================================================

#[tokio::test]
async fn a2a_client_streaming_final() {
    // Mock returns SSE with status updates and a final event
    let sse_body = [
        format!(
            "data: {}\n\n",
            json!({
                "jsonrpc": "2.0",
                "id": "stream-1",
                "result": {
                    "kind": "status-update",
                    "taskId": "task-1",
                    "contextId": "ctx-1",
                    "status": {"state": "working"}
                }
            })
        ),
        format!(
            "data: {}\n\n",
            json!({
                "jsonrpc": "2.0",
                "id": "stream-1",
                "result": {
                    "kind": "status-update",
                    "taskId": "task-1",
                    "contextId": "ctx-1",
                    "status": {"state": "completed"},
                    "final": true
                }
            })
        ),
    ]
    .join("");

    let agent_card = json!({
        "name": "Stream Agent",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {"streaming": true},
        "skills": [],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    let router = Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let body = sse_body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );

    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_streaming
  execution:
    mode: a2a_client
    state:
      streaming: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Stream this"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Trace should have streaming events
    let entries = trace.snapshot();
    assert!(
        entries.len() >= 3,
        "expected ≥3 trace entries for streaming, got {}",
        entries.len()
    );
}

// ============================================================================
// 3. Stream never sends final — cancel fires (EC-A2A-010)
// ============================================================================

#[tokio::test]
async fn a2a_client_stream_never_final() {
    // Mock returns an SSE stream that never sends final: true
    let sse_body = format!(
        "data: {}\n\n",
        json!({
            "jsonrpc": "2.0",
            "id": "stream-1",
            "result": {
                "kind": "status-update",
                "taskId": "task-1",
                "contextId": "ctx-1",
                "status": {"state": "working"}
            }
        })
    );

    let agent_card = json!({
        "name": "Slow Agent",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {"streaming": true},
        "skills": [],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    let router = Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let body = sse_body.clone();
                // Return just one event then close the stream
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );

    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_never_final
  execution:
    mode: a2a_client
    state:
      streaming: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Will you ever finish?"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let cancel = CancellationToken::new();

    let config = PhaseLoopConfig {
        trace: trace.clone(),
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: cancel.clone(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
        a2a_skill_tx: None,
        context_mode: false,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, config);

    let result = phase_loop.run().await.unwrap();
    // Stream closes without final: true → driver returns TransportClosed
    assert_eq!(result.termination, TerminationReason::TransportClosed);

    // Partial trace should still be captured
    let entries = trace.snapshot();
    assert!(!entries.is_empty());
}

// ============================================================================
// 4. Agent card timeout — connection refused (EC-A2A-008)
// ============================================================================

#[tokio::test]
async fn a2a_client_agent_card_timeout() {
    // Point at a port that nothing is listening on
    let driver = create_a2a_client_driver("http://127.0.0.1:1", vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_timeout
  execution:
    mode: a2a_client
    state:
      fetch_agent_card: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Hello"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    // Should fail with a driver error (connection refused)
    let result = phase_loop.run().await;
    assert!(
        result.is_err(),
        "expected connection error, got: {result:?}"
    );
}

// ============================================================================
// 5. Sync vs streaming mismatch — server returns SSE for message/send (EC-A2A-009)
// ============================================================================

#[tokio::test]
async fn a2a_client_sync_receives_sse() {
    // Mock returns SSE (text/event-stream) even though client sends message/send (sync)
    let sse_body = format!(
        "data: {}\n\n",
        json!({
            "jsonrpc": "2.0",
            "id": "stream-1",
            "result": {
                "kind": "status-update",
                "taskId": "task-1",
                "contextId": "ctx-1",
                "status": {"state": "completed"},
                "final": true
            }
        })
    );

    let agent_card = json!({
        "name": "Mismatch Agent",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {},
        "skills": [],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    let router = Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let body = sse_body.clone();
                // Return SSE content-type for a sync request
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );

    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_sync_mismatch
  execution:
    mode: a2a_client
    state:
      streaming: false
      task_message:
        role: user
        parts:
          - kind: text
            text: "Hello"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    // The sync path calls response.json() on SSE content — should fail to parse
    let result = phase_loop.run().await;
    assert!(
        result.is_err(),
        "expected parse error from SSE-as-JSON, got: {result:?}"
    );
}

// ============================================================================
// 6. Direct Message response instead of Task (EC-A2A-013)
// ============================================================================

#[tokio::test]
async fn a2a_client_direct_message_response() {
    // Mock returns a Message (kind: "message") instead of a Task (kind: "task")
    let task_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "kind": "message",
            "role": "agent",
            "messageId": "agent-msg-1",
            "contextId": "ctx-1",
            "parts": [{"kind": "text", "text": "Direct reply"}]
        }
    });

    let router = mock_a2a_router(task_response);
    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_direct_message
  execution:
    mode: a2a_client
    state:
      task_message:
        role: user
        parts:
          - kind: text
            text: "Hello"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    // Trace should contain a "message/response" event (from detect_event_type)
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"message/response"),
        "expected message/response in trace, got: {methods:?}"
    );
}

// ============================================================================
// 7. auth-required status in SSE stream (EC-A2A-014)
// ============================================================================

#[tokio::test]
async fn a2a_client_auth_required_status() {
    // Mock returns SSE with auth-required status
    let sse_body = [format!(
        "data: {}\n\n",
        json!({
            "jsonrpc": "2.0",
            "id": "stream-1",
            "result": {
                "kind": "status-update",
                "taskId": "task-1",
                "contextId": "ctx-1",
                "status": {"state": "auth-required"},
                "final": true
            }
        })
    )]
    .join("");

    let agent_card = json!({
        "name": "Auth Agent",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {"streaming": true},
        "skills": [],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    let router = Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let body = sse_body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );

    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_auth_required
  execution:
    mode: a2a_client
    state:
      streaming: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Need auth"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Trace should contain a qualified "task/status:auth-required" event
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"task/status:auth-required"),
        "expected task/status:auth-required in trace, got: {methods:?}"
    );
}

// ============================================================================
// 8. rejected status in SSE stream (EC-A2A-015)
// ============================================================================

#[tokio::test]
async fn a2a_client_rejected_status() {
    // Mock returns SSE with rejected status
    let sse_body = format!(
        "data: {}\n\n",
        json!({
            "jsonrpc": "2.0",
            "id": "stream-1",
            "result": {
                "kind": "status-update",
                "taskId": "task-1",
                "contextId": "ctx-1",
                "status": {"state": "rejected"},
                "final": true
            }
        })
    );

    let agent_card = json!({
        "name": "Reject Agent",
        "url": "http://localhost",
        "version": "1.0",
        "capabilities": {"streaming": true},
        "skills": [],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    });

    let router = Router::new()
        .route(
            "/.well-known/agent.json",
            get(move || {
                let card = agent_card.clone();
                async move { axum::Json(card) }
            }),
        )
        .route(
            "/",
            post(move || {
                let body = sse_body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );

    let mock = MockServer::start(router).await;

    let driver = create_a2a_client_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: a2a_rejected
  execution:
    mode: a2a_client
    state:
      streaming: true
      task_message:
        role: user
        parts:
          - kind: text
            text: "Will be rejected"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Trace should contain a qualified "task/status:rejected" event
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"task/status:rejected"),
        "expected task/status:rejected in trace, got: {methods:?}"
    );
}
