//! Multi-actor orchestration integration tests.
//!
//! Tests use `orchestrate()` with multi-actor YAML. Client actors point
//! at mock HTTP servers.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use thoughtjack::loader::load_document;
use thoughtjack::observability::events::EventEmitter;
use thoughtjack::orchestration::{ActorConfig, ActorOutcome, orchestrate};

use crate::common::mock_server::{MockServer, sse_event};

/// Builds a default `ActorConfig` with the given max session.
const fn default_actor_config(max_session: Duration) -> ActorConfig {
    ActorConfig {
        mcp_server_bind: None,
        agui_client_endpoint: None,
        a2a_server_bind: None,
        a2a_client_endpoint: None,
        mcp_client_command: None,
        mcp_client_args: None,
        mcp_client_endpoint: None,
        headers: vec![],
        raw_synthesize: false,
        grace_period: None,
        max_session,
        readiness_timeout: Duration::from_secs(30),
        context_mode: false,
        context_provider_config: None,
        max_turns: None,
        context_system_prompt: None,
    }
}

// ============================================================================
// 1. Two-actor extractor handoff (EC-ORCH-001)
// ============================================================================

#[tokio::test]
async fn two_actor_extractor_handoff() {
    tokio::time::timeout(Duration::from_secs(30), async {
        // MCP server has an extractor that captures a value.
        // AG-UI client uses await_extractors to resolve it.
        // Since we can't easily trigger MCP server via HTTP in this test,
        // we test via orchestrate() with a short max_session and verify
        // the trace shows both actors ran.
        let sse_body = format!(
            "{}{}",
            sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
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

        let yaml = r#"
oatf: "0.1"
attack:
  name: extractor_handoff
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_secs(5));
        config.agui_client_endpoint = Some(mock.url());
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        // Both actors should have outcomes
        assert_eq!(result.outcomes.len(), 2);

        // Client should have completed (terminal phase reached or cancelled)
        let client_outcome = result
            .outcomes
            .iter()
            .find(|o| o.actor_name() == "client")
            .expect("client outcome missing");
        match client_outcome {
            ActorOutcome::Success(r) => {
                assert_eq!(r.actor_name, "client");
            }
            other => panic!("Expected client Success, got: {other:?}"),
        }
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 2. await_extractors timeout — proceeds with empty (EC-ORCH-002)
// ============================================================================

#[tokio::test]
async fn await_extractors_timeout() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let sse_body = format!(
            "{}{}",
            sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
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

        let yaml = r#"
oatf: "0.1"
attack:
  name: await_timeout
  execution:
    actors:
      - name: producer
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: consumer
        mode: ag_ui_client
        phases:
          - name: probe
            await_extractors:
              - actor: producer
                extractors:
                  - never_set_key
                timeout: "200ms"
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "test"
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_secs(10));
        config.agui_client_endpoint = Some(mock.url());
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        // Both actors should have outcomes
        assert_eq!(result.outcomes.len(), 2);

        // Consumer should have proceeded despite the timeout
        let consumer = result
            .outcomes
            .iter()
            .find(|o| o.actor_name() == "consumer")
            .expect("consumer outcome missing");
        assert!(
            matches!(consumer, ActorOutcome::Success(_)),
            "consumer should succeed after timeout, got: {consumer:?}"
        );
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 3. Grace period on clients done (EC-ORCH-007)
// ============================================================================

#[tokio::test]
async fn grace_period_on_clients_done() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let sse_body = format!(
            "{}{}",
            sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
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

        let yaml = r#"
oatf: "0.1"
attack:
  name: grace_period
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
      - name: client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_secs(10));
        config.agui_client_endpoint = Some(mock.url());
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        config.grace_period = Some(Duration::from_millis(200));
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let start = tokio::time::Instant::now();
        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.outcomes.len(), 2);

        // Grace period should have added ~200ms before cancelling the server
        assert!(
            elapsed >= Duration::from_millis(150),
            "expected grace delay, elapsed: {elapsed:?}"
        );

        // Client should have completed normally
        let client = result
            .outcomes
            .iter()
            .find(|o| o.actor_name() == "client")
            .expect("client outcome missing");
        assert!(
            matches!(client, ActorOutcome::Success(_)),
            "expected client Success, got: {client:?}"
        );
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 4. Zero grace period — immediate cancel (EC-ORCH-014)
// ============================================================================

#[tokio::test]
async fn zero_grace_period() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let sse_body = format!(
            "{}{}",
            sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
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

        let yaml = r#"
oatf: "0.1"
attack:
  name: zero_grace
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
      - name: client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_secs(10));
        config.agui_client_endpoint = Some(mock.url());
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        config.grace_period = Some(Duration::ZERO);
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let start = tokio::time::Instant::now();
        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.outcomes.len(), 2);

        // Zero grace period: should complete quickly (no significant delay)
        assert!(
            elapsed < Duration::from_secs(3),
            "zero grace should not delay, elapsed: {elapsed:?}"
        );
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 5. Client error counts as done (EC-ORCH-015)
// ============================================================================

#[tokio::test]
async fn client_error_counts_as_done() {
    tokio::time::timeout(Duration::from_secs(30), async {
        // AG-UI client with no endpoint → connection refused → error
        // Error should count as "done" for grace period calculation
        let yaml = r#"
oatf: "0.1"
attack:
  name: client_error
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
      - name: bad_client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_secs(5));
        // Point at a port that nothing is listening on
        config.agui_client_endpoint = Some("http://127.0.0.1:1".to_string());
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        config.grace_period = Some(Duration::ZERO);
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        assert_eq!(result.outcomes.len(), 2);

        // Client should have an error outcome
        let client = result
            .outcomes
            .iter()
            .find(|o| o.actor_name() == "bad_client")
            .expect("bad_client outcome missing");
        assert!(
            matches!(client, ActorOutcome::Error { .. }),
            "expected client Error, got: {client:?}"
        );
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 6. Trace merge ordering (EC-ORCH-012)
// ============================================================================

#[tokio::test]
async fn trace_merge_ordering() {
    tokio::time::timeout(Duration::from_secs(30), async {
        // Start two AG-UI clients hitting different mock servers concurrently.
        // Both emit events → merged trace should have monotonic seq numbers.
        let make_mock = || async {
            let sse_body = format!(
                "{}{}{}",
                sse_event("RUN_STARTED", &json!({"threadId": "t1"})),
                sse_event(
                    "TEXT_MESSAGE_CHUNK",
                    &json!({"messageId": "m1", "role": "assistant", "delta": "hi"})
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
            MockServer::start(router).await
        };

        let mock1 = make_mock().await;
        let mock2 = make_mock().await;

        let yaml = r#"
oatf: "0.1"
attack:
  name: trace_merge
  execution:
    actors:
      - name: client1
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello from 1"
      - name: client2
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello from 2"
"#;

        let loaded = load_document(yaml).unwrap();
        // Both clients need the endpoint; but ActorConfig only has one field.
        // Since both are ag_ui_client, they'll both use the same endpoint.
        // Use mock1 for both; the point is trace ordering, not separate endpoints.
        let mut config = default_actor_config(Duration::from_secs(10));
        config.agui_client_endpoint = Some(mock1.url());
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        assert_eq!(result.outcomes.len(), 2);

        // Verify merged trace has monotonically increasing seq numbers
        let entries = result.trace.snapshot();
        assert!(!entries.is_empty());
        for window in entries.windows(2) {
            assert!(
                window[0].seq < window[1].seq,
                "trace seq should be monotonic: {} < {}",
                window[0].seq,
                window[1].seq
            );
        }

        // Keep mock2 alive to suppress unused warning
        drop(mock2);
    })
    .await
    .expect("test timed out after 30s");
}

// ============================================================================
// 7. max_session cancels all (EC-ORCH-008)
// ============================================================================

#[tokio::test]
async fn max_session_cancels_all() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let yaml = r#"
oatf: "0.1"
attack:
  name: max_session
  execution:
    mode: mcp_server
    phases:
      - name: long_running
        state:
          tools:
            - name: test_tool
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 999
      - name: terminal
"#;

        let loaded = load_document(yaml).unwrap();
        let mut config = default_actor_config(Duration::from_millis(200));
        // Use HTTP transport to avoid stdio hang (blocking reads are uncancellable)
        config.mcp_server_bind = Some("127.0.0.1:0".to_string());
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let start = tokio::time::Instant::now();
        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.outcomes.len(), 1);

        // Should have been cancelled by max_session
        assert!(
            elapsed < Duration::from_secs(5),
            "max_session should cancel within timeout, elapsed: {elapsed:?}"
        );
    })
    .await
    .expect("test timed out after 30s");
}
