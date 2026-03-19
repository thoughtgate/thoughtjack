//! AG-UI client ↔ HTTP mock integration tests.
//!
//! Each test starts an axum mock server returning crafted SSE, creates an
//! `AgUiDriver` via `create_agui_driver`, wires it into a `PhaseLoop`, and
//! verifies the resulting trace.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::routing::post;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use thoughtjack::engine::{
    ExtractorStore, PhaseEngine, PhaseLoop, PhaseLoopConfig, SharedTrace, TerminationReason,
};
use thoughtjack::observability::EventEmitter;
use thoughtjack::protocol::agui::create_agui_driver;

use crate::common::mock_server::{MockServer, sse_data_line, sse_event};

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
    }
}

// ============================================================================
// 1. Happy path: full POST → SSE stream
// ============================================================================

#[tokio::test]
async fn agui_happy_path_sse_stream() {
    let sse_body = format!(
        "{}{}{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event(
            "TEXT_MESSAGE_START",
            &json!({"messageId": "m1", "role": "assistant"})
        ),
        sse_event(
            "TEXT_MESSAGE_CONTENT",
            &json!({"messageId": "m1", "delta": "Hello world"})
        ),
        sse_event("TEXT_MESSAGE_END", &json!({"messageId": "m1"})),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );

    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_happy
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "Hello"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    // outgoing run_agent_input + incoming events + _accumulated_response
    assert!(
        entries.len() >= 6,
        "expected ≥6 trace entries, got {}",
        entries.len()
    );

    // Verify we have the key event types
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"run_agent_input"));
    assert!(methods.contains(&"run_started"));
    assert!(methods.contains(&"run_finished"));
    assert!(methods.contains(&"_accumulated_response"));
}

// ============================================================================
// 2. Malformed SSE event — bad JSON skipped, stream continues (EC-AGUI-001)
// ============================================================================

#[tokio::test]
async fn agui_malformed_sse_event() {
    let sse_body = format!(
        "{}{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        // Malformed JSON line
        "event: TEXT_MESSAGE_CONTENT\ndata: not-valid-json\n\n",
        sse_event(
            "TEXT_MESSAGE_CONTENT",
            &json!({"messageId": "m1", "delta": "survived"})
        ),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_malformed
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // The bad event is skipped; valid events before and after are in the trace
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"run_started"));
    assert!(methods.contains(&"run_finished"));
}

// ============================================================================
// 3. Connection drops mid-stream — partial trace preserved (EC-AGUI-002)
// ============================================================================

#[tokio::test]
async fn agui_connection_drops_mid_stream() {
    // Send 3 events then close (no RUN_FINISHED)
    let sse_body = format!(
        "{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event(
            "TEXT_MESSAGE_START",
            &json!({"messageId": "m1", "role": "assistant"})
        ),
        sse_event(
            "TEXT_MESSAGE_CONTENT",
            &json!({"messageId": "m1", "delta": "partial"})
        ),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_drop
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Partial events plus _accumulated_response
    let entries = trace.snapshot();
    assert!(entries.len() >= 3);
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"run_started"));
    assert!(methods.contains(&"_accumulated_response"));
}

// ============================================================================
// 4. HTTP 429 retry — mock returns 429 twice then 200 (EC-AGUI-004)
// ============================================================================

#[tokio::test]
async fn agui_http_429_retry() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count = call_count.clone();

    let sse_body = format!(
        "{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let body = sse_body.clone();
            async move {
                if n < 2 {
                    // First 2 requests → 429
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("content-type", "text/plain")],
                        "rate limited".to_string(),
                    )
                        .into_response()
                } else {
                    // Third request → 200 with SSE
                    (
                        axum::http::StatusCode::OK,
                        [("content-type", "text/event-stream")],
                        body,
                    )
                        .into_response()
                }
            }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_429
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Mock received 3 requests total (2 × 429 + 1 × 200)
    assert_eq!(call_count.load(Ordering::SeqCst), 3);

    // Trace has the successful events
    let entries = trace.snapshot();
    assert!(
        entries
            .iter()
            .map(|e| e.method.as_str())
            .any(|x| x == "run_finished")
    );
}

use axum::response::IntoResponse;

// ============================================================================
// 5. HTTP 500 no retry — run_error emitted (EC-AGUI-005)
// ============================================================================

#[tokio::test]
async fn agui_http_500_no_retry() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count = call_count.clone();

    let router = Router::new().route(
        "/",
        post(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_500
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // No retry on 500 — exactly 1 request
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // run_error event in trace
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"run_error"),
        "expected run_error in trace, got: {methods:?}"
    );
}

// ============================================================================
// 6. Multi-run phase — trigger on run_finished count: 3 (EC-AGUI-009/010)
// ============================================================================

#[tokio::test]
async fn agui_multi_run_phase() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count = call_count.clone();

    let router = Router::new().route(
        "/",
        post(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let body = format!(
                "{}{}",
                sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
                sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
            );
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_multi_run
  execution:
    mode: ag_ui_client
    phases:
      - name: probe
        state:
          run_agent_input:
            messages:
              - role: user
                content: "test"
        trigger:
          event: run_finished
          count: 3
      - name: terminal
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // 3 SSE requests needed to trigger count: 3
    assert_eq!(call_count.load(Ordering::SeqCst), 3);

    // Trace should have 3 run_finished events
    let entries = trace.snapshot();
    let run_finished_count = entries
        .iter()
        .filter(|e| e.method == "run_finished")
        .count();
    assert_eq!(run_finished_count, 3);
}

// ============================================================================
// 7. Canonical data-only SSE (no event: lines) — type from data["type"]
// ============================================================================

#[tokio::test]
async fn agui_canonical_data_only_sse() {
    // AG-UI canonical encoder: only `data:` lines, type in JSON payload
    let sse_body = format!(
        "{}{}{}",
        sse_data_line(&json!({"type": "RUN_STARTED", "threadId": "t1"})),
        sse_data_line(
            &json!({"type": "TEXT_MESSAGE_CHUNK", "messageId": "m1", "role": "assistant", "delta": "Hi"})
        ),
        sse_data_line(&json!({"type": "RUN_FINISHED", "threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_canonical
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Events resolved from data["type"] should appear mapped to snake_case
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"run_started"));
    assert!(methods.contains(&"run_finished"));
}

// ============================================================================
// 8. Tool call with streamed arguments (EC-AGUI-014)
// ============================================================================

#[tokio::test]
async fn agui_tool_call_streamed_args() {
    let sse_body = format!(
        "{}{}{}{}{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event(
            "TOOL_CALL_START",
            &json!({"toolCallId": "tc1", "toolCallName": "calculator", "parentMessageId": "m1"})
        ),
        sse_event(
            "TOOL_CALL_ARGS",
            &json!({"toolCallId": "tc1", "delta": "{\"a\":"})
        ),
        sse_event(
            "TOOL_CALL_ARGS",
            &json!({"toolCallId": "tc1", "delta": " 1, "})
        ),
        sse_event(
            "TOOL_CALL_ARGS",
            &json!({"toolCallId": "tc1", "delta": "\"b\": 2}"})
        ),
        sse_event("TOOL_CALL_END", &json!({"toolCallId": "tc1"})),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_tool_call
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"tool_call_start"));
    assert!(methods.contains(&"tool_call_end"));

    // Verify tool_call_start contains the expected metadata
    let tc_start = entries
        .iter()
        .find(|e| e.method == "tool_call_start")
        .expect("tool_call_start missing");
    assert_eq!(tc_start.content["toolCallId"], "tc1");
    assert_eq!(tc_start.content["toolCallName"], "calculator");

    // Verify tool_call_args deltas were captured in the trace
    let arg_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.method == "tool_call_args")
        .collect();
    assert!(
        arg_entries.len() >= 3,
        "expected at least 3 TOOL_CALL_ARGS events, got {}",
        arg_entries.len()
    );
    // Concatenated deltas should form valid JSON: {"a": 1, "b": 2}
    let concatenated: String = arg_entries
        .iter()
        .filter_map(|e| e.content.get("delta").and_then(|d| d.as_str()))
        .collect();
    assert!(
        concatenated.contains("\"a\"") && concatenated.contains("\"b\""),
        "concatenated tool-call args should contain a and b, got: {concatenated}"
    );
}

// ============================================================================
// 9. Custom/unknown event passthrough (EC-AGUI-012)
// ============================================================================

#[tokio::test]
async fn agui_custom_event_passthrough() {
    let sse_body = format!(
        "{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event(
            "CUSTOM_DEBUG",
            &json!({"data": "internal state", "level": "debug"})
        ),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_custom
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Unknown event type passes through as-is
    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"CUSTOM_DEBUG"),
        "unknown event should pass through, got: {methods:?}"
    );
}

// ============================================================================
// 10. Reasoning events with content (EC-AGUI-013)
// ============================================================================

#[tokio::test]
async fn agui_reasoning_events() {
    let sse_body = format!(
        "{}{}{}{}{}",
        sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
        sse_event("REASONING_MESSAGE_START", &json!({"messageId": "r1"})),
        sse_event(
            "REASONING_MESSAGE_CONTENT",
            &json!({"messageId": "r1", "delta": "thinking about this..."})
        ),
        sse_event("REASONING_MESSAGE_END", &json!({"messageId": "r1"})),
        sse_event("RUN_FINISHED", &json!({"threadId": "t1"})),
    );

    let router = Router::new().route(
        "/",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let mock = MockServer::start(router).await;

    let driver = create_agui_driver(&mock.url(), vec![], false);
    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: agui_reasoning
  execution:
    mode: ag_ui_client
    state:
      run_agent_input:
        messages:
          - role: user
            content: "test"
"#,
    );
    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace.clone()));

    let result = phase_loop.run().await.unwrap();
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"reasoning_message_start"));
    assert!(methods.contains(&"reasoning_message_content"));
    assert!(methods.contains(&"reasoning_message_end"));

    // Accumulated response should contain reasoning
    let acc = entries
        .iter()
        .find(|e| e.method == "_accumulated_response")
        .expect("_accumulated_response missing");
    let reasoning = &acc.content["reasoning"];
    assert!(reasoning.is_array());
    let reasoning_arr = reasoning.as_array().unwrap();
    assert!(!reasoning_arr.is_empty());
    assert_eq!(
        reasoning_arr[0]["content"].as_str().unwrap(),
        "thinking about this..."
    );
}
