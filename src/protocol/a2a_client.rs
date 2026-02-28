//! A2A client-mode `PhaseDriver` implementation.
//!
//! `A2aClientDriver` sends HTTP requests to an A2A agent endpoint:
//! Agent Card discovery (`GET /.well-known/agent.json`), task submission
//! (`message/send` or `message/stream`), and SSE stream consumption.
//! Each protocol event is emitted as a `ProtocolEvent` for the `PhaseLoop`.
//!
//! This is a client-mode driver: it initiates requests rather than waiting
//! for them. Extractors are cloned once at the start of `drive_phase()`.
//!
//! See TJ-SPEC-017 for the full A2A protocol support specification.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use oatf::primitives::interpolate_value;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;

// ============================================================================
// Constants
// ============================================================================

/// Default request timeout per spec §NFR-004.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Agent Card fetch timeout.
const AGENT_CARD_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum retry attempts for HTTP 429 responses.
const MAX_RETRIES: u32 = 3;

/// Initial retry backoff for 429 responses.
const INITIAL_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Maximum consecutive SSE parse errors before closing.
const MAX_CONSECUTIVE_SSE_ERRORS: usize = 10;

/// Default run timeout for streaming responses.
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(60);

// ============================================================================
// A2aSseParser
// ============================================================================

/// Minimal SSE parser for A2A streaming responses.
///
/// A2A SSE is simpler than AG-UI: data-only lines (no `event:` type),
/// each containing a complete JSON-RPC response. A blank line dispatches
/// the accumulated data.
///
/// Implements: TJ-SPEC-017 F-006
struct A2aSseParser {
    /// Line accumulation buffer.
    buffer: String,
    /// Accumulated `data:` field content.
    current_data: String,
    /// Number of consecutive parse errors (resets on success).
    consecutive_errors: usize,
}

impl A2aSseParser {
    /// Creates a new SSE parser.
    const fn new() -> Self {
        Self {
            buffer: String::new(),
            current_data: String::new(),
            consecutive_errors: 0,
        }
    }

    /// Feed raw bytes into the parser and extract any complete events.
    ///
    /// Each dispatched event is either `Ok(Value)` (the parsed `result`
    /// field from the JSON-RPC response) or `Err(String)` for parse failures.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<Value, String>> {
        let text = String::from_utf8_lossy(bytes);
        self.buffer.push_str(&text);

        let mut events = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if line.is_empty() {
                // Blank line = dispatch event
                if let Some(event) = self.dispatch_event() {
                    events.push(event);
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.current_data.is_empty() {
                    self.current_data.push('\n');
                }
                self.current_data.push_str(value.trim_start());
            } else if line.starts_with(':') {
                // SSE comment — ignore
            }
            // Other lines (including `event:`) are noted but not used
        }

        events
    }

    /// Dispatch an accumulated data event.
    fn dispatch_event(&mut self) -> Option<Result<Value, String>> {
        let data_str = std::mem::take(&mut self.current_data);
        if data_str.is_empty() {
            return None;
        }

        let parsed: Value = match serde_json::from_str(&data_str) {
            Ok(v) => v,
            Err(e) => {
                self.consecutive_errors += 1;
                return Some(Err(format!("malformed JSON in A2A SSE data: {e}")));
            }
        };

        self.consecutive_errors = 0;

        // Extract the `result` field from the JSON-RPC response envelope
        let result = parsed.get("result").cloned().unwrap_or(parsed);

        Some(Ok(result))
    }

    /// Returns the current consecutive error count.
    const fn consecutive_errors(&self) -> usize {
        self.consecutive_errors
    }
}

// ============================================================================
// A2aSseStream
// ============================================================================

/// Wraps a `reqwest::Response` byte stream with an `A2aSseParser`.
///
/// Implements: TJ-SPEC-017 F-006
struct A2aSseStream {
    parser: A2aSseParser,
    stream: Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    /// Buffered events from the parser.
    pending: Vec<Result<Value, String>>,
}

impl A2aSseStream {
    /// Creates a new SSE stream from a reqwest response.
    fn new(response: reqwest::Response) -> Self {
        Self {
            parser: A2aSseParser::new(),
            stream: Box::pin(response.bytes_stream()),
            pending: Vec::new(),
        }
    }

    /// Reads the next parsed event from the stream.
    ///
    /// Returns `None` when the stream is exhausted.
    ///
    /// # Errors
    ///
    /// Returns an error on HTTP stream errors or malformed SSE data.
    async fn next_event(&mut self) -> Result<Option<Value>, EngineError> {
        loop {
            // Drain any buffered events first
            if let Some(result) = self.pending.pop() {
                return match result {
                    Ok(event) => Ok(Some(event)),
                    Err(msg) => Err(EngineError::Driver(msg)),
                };
            }

            // Read next chunk
            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    let mut events = self.parser.feed(&bytes);
                    events.reverse();
                    self.pending = events;
                }
                Some(Err(e)) => {
                    return Err(EngineError::Driver(format!("A2A SSE stream error: {e}")));
                }
                None => {
                    return Ok(None);
                }
            }
        }
    }
}

// ============================================================================
// A2aClientTransport
// ============================================================================

/// HTTP transport for A2A agent communication.
///
/// Manages the HTTP client, persistent `context_id`, and custom headers.
///
/// Implements: TJ-SPEC-017 F-006
struct A2aClientTransport {
    /// Target agent's base URL.
    agent_url: String,
    /// HTTP client.
    client: reqwest::Client,
    /// Custom headers for all requests.
    headers: Vec<(String, String)>,
    /// Persisted context ID across phases (EC-A2A-016).
    context_id: Option<String>,
}

impl A2aClientTransport {
    /// Creates a new A2A client transport.
    fn new(endpoint: &str, headers: Vec<(String, String)>) -> Self {
        Self {
            agent_url: endpoint.to_string(),
            client: reqwest::Client::new(),
            headers,
            context_id: None,
        }
    }

    /// Fetches the Agent Card from `/.well-known/agent.json`.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on HTTP errors or timeout.
    async fn get_agent_card(&self) -> Result<Value, EngineError> {
        let url = format!(
            "{}/.well-known/agent.json",
            self.agent_url.trim_end_matches('/')
        );

        let mut request = self.client.get(&url);
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let response = request
            .timeout(AGENT_CARD_TIMEOUT)
            .send()
            .await
            .map_err(|e| EngineError::Driver(format!("Agent Card fetch failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(EngineError::Driver(format!(
                "Agent Card fetch returned HTTP {status}: {body}"
            )));
        }

        response
            .json::<Value>()
            .await
            .map_err(|e| EngineError::Driver(format!("Agent Card parse failed: {e}")))
    }

    /// Sends a synchronous `message/send` request.
    ///
    /// Retries on HTTP 429 with exponential backoff.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on HTTP errors.
    async fn message_send(&self, body: &Value) -> Result<Value, EngineError> {
        let mut backoff = INITIAL_RETRY_BACKOFF;

        for attempt in 0..=MAX_RETRIES {
            let mut request = self
                .client
                .post(&self.agent_url)
                .header("Content-Type", "application/json");

            for (key, value) in &self.headers {
                request = request.header(key.as_str(), value.as_str());
            }

            let response = request
                .json(body)
                .timeout(DEFAULT_TIMEOUT)
                .send()
                .await
                .map_err(|e| EngineError::Driver(format!("A2A message/send failed: {e}")))?;

            let status = response.status();

            if status.is_success() {
                return response
                    .json::<Value>()
                    .await
                    .map_err(|e| EngineError::Driver(format!("A2A response parse failed: {e}")));
            }

            if status.as_u16() == 429 && attempt < MAX_RETRIES {
                tracing::warn!(
                    attempt = attempt + 1,
                    max_retries = MAX_RETRIES,
                    backoff_ms = backoff.as_millis(),
                    "A2A agent returned 429, retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            let resp_body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(EngineError::Driver(format!(
                "A2A agent returned HTTP {status}: {resp_body}"
            )));
        }

        unreachable!("retry loop should return before exceeding MAX_RETRIES")
    }

    /// Opens a streaming `message/stream` request, returns an SSE stream.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on HTTP errors.
    async fn message_stream(&self, body: &Value) -> Result<A2aSseStream, EngineError> {
        let mut request = self
            .client
            .post(&self.agent_url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");

        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let response = request
            .json(body)
            .send()
            .await
            .map_err(|e| EngineError::Driver(format!("A2A message/stream failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let resp_body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(EngineError::Driver(format!(
                "A2A agent returned HTTP {status}: {resp_body}"
            )));
        }

        Ok(A2aSseStream::new(response))
    }
}

// ============================================================================
// Task Message Construction
// ============================================================================

/// Builds a JSON-RPC task message from phase state and extractors.
///
/// # Errors
///
/// Returns `EngineError::Driver` if `state["task_message"]` is missing.
///
/// Implements: TJ-SPEC-017 F-007
fn build_task_message(
    state: &Value,
    extractors: &HashMap<String, String>,
    context_id: Option<&str>,
    streaming: bool,
) -> Result<Value, EngineError> {
    let task_message = state.get("task_message").ok_or_else(|| {
        EngineError::Driver(
            "A2A phase state missing 'task_message' key — \
             each A2A client phase must define state.task_message"
                .to_string(),
        )
    })?;

    // Interpolate the task_message subtree
    let (mut interpolated, _) = interpolate_value(task_message, extractors, None, None);

    // Auto-generate messageId if missing
    if interpolated.get("messageId").is_none() || interpolated["messageId"].is_null() {
        interpolated["messageId"] = Value::String(uuid::Uuid::new_v4().to_string());
    }

    // Set kind to "message"
    if interpolated.get("kind").is_none() {
        interpolated["kind"] = Value::String("message".to_string());
    }

    // Include contextId from previous phases if available
    if let Some(ctx) = context_id
        && interpolated.get("contextId").is_none()
    {
        interpolated["contextId"] = Value::String(ctx.to_string());
    }

    let method = if streaming {
        "message/stream"
    } else {
        "message/send"
    };

    // Build JSON-RPC envelope
    let mut params = json!({ "message": interpolated });

    // Include configuration from state if present
    if let Some(config) = state.get("configuration") {
        let (interpolated_config, _) = interpolate_value(config, extractors, None, None);
        params["configuration"] = interpolated_config;
    }

    // Include metadata from state if present
    if let Some(metadata) = state.get("metadata") {
        let (interpolated_metadata, _) = interpolate_value(metadata, extractors, None, None);
        params["metadata"] = interpolated_metadata;
    }

    Ok(json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    }))
}

/// Detects the SSE event type from the `kind` discriminator.
///
/// Implements: TJ-SPEC-017 F-008
fn detect_event_type(result: &Value) -> &str {
    match result.get("kind").and_then(Value::as_str) {
        Some("task") => "task/created",
        Some("message") => "message/response",
        Some("status-update") => "task/status",
        Some("artifact-update") => "task/artifact",
        _ => "unknown",
    }
}

/// Resolves a qualifier for status update events.
///
/// For `task/status` events, appends the status state as a qualifier
/// (e.g., `task/status:completed`).
///
/// Implements: TJ-SPEC-017 F-008
fn resolve_status_qualifier(event_type: &str, result: &Value) -> String {
    if event_type == "task/status"
        && let Some(state) = result
            .get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
    {
        return format!("task/status:{state}");
    }
    event_type.to_string()
}

// ============================================================================
// A2aClientDriver
// ============================================================================

/// A2A client-mode protocol driver.
///
/// Sends task messages to an A2A agent and consumes responses (synchronous
/// JSON or SSE stream). Each protocol event is emitted for `PhaseLoop`
/// processing.
///
/// Client-mode extractors are cloned once per `drive_phase()` call.
///
/// Implements: TJ-SPEC-017 F-007
pub struct A2aClientDriver {
    /// HTTP transport for A2A communication.
    transport: A2aClientTransport,
    /// Bypass synthesize output validation.
    #[allow(dead_code)]
    raw_synthesize: bool,
}

#[async_trait]
impl PhaseDriver for A2aClientDriver {
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        // Client-mode: clone extractors once at start
        let current_extractors = extractors.borrow().clone();

        // Fetch Agent Card if requested
        if state
            .get("fetch_agent_card")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            // Emit outgoing event for card fetch
            let _ = event_tx.send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: "agent_card/get".to_string(),
                content: json!({}),
            });

            let card = self.transport.get_agent_card().await?;

            let _ = event_tx.send(ProtocolEvent {
                direction: Direction::Incoming,
                method: "agent_card/get".to_string(),
                content: card,
            });
        }

        // Determine streaming mode
        let streaming = state
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Build task message
        let message = build_task_message(
            state,
            &current_extractors,
            self.transport.context_id.as_deref(),
            streaming,
        )?;

        let method = if streaming {
            "message/stream"
        } else {
            "message/send"
        };

        // Emit outgoing event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: method.to_string(),
            content: message.get("params").cloned().unwrap_or(Value::Null),
        });

        if streaming {
            self.drive_streaming(message, event_tx, cancel).await
        } else {
            self.drive_synchronous(message, method, event_tx).await
        }
    }
}

impl A2aClientDriver {
    /// Handles synchronous `message/send` response.
    async fn drive_synchronous(
        &mut self,
        message: Value,
        _method: &str,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<DriveResult, EngineError> {
        let response = self.transport.message_send(&message).await?;

        // Extract result from JSON-RPC response
        let result = response.get("result").cloned().unwrap_or(response);

        // Detect response type via `kind` discriminator
        let event_type = detect_event_type(&result);

        // Capture contextId for multi-turn tracking
        if let Some(ctx) = result.get("contextId").and_then(Value::as_str) {
            self.transport.context_id = Some(ctx.to_string());
        }

        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: event_type.to_string(),
            content: result,
        });

        Ok(DriveResult::Complete)
    }

    /// Handles streaming `message/stream` SSE response.
    async fn drive_streaming(
        &mut self,
        message: Value,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        let mut sse_stream = self.transport.message_stream(&message).await?;

        // Emit stream-opened event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: "message/stream".to_string(),
            content: json!({"status": "connected"}),
        });

        let stream_timeout = DEFAULT_STREAM_TIMEOUT;

        loop {
            tokio::select! {
                result = tokio::time::timeout(stream_timeout, sse_stream.next_event()) => {
                    match result {
                        Ok(Ok(Some(sse_result))) => {
                            let event_type = detect_event_type(&sse_result);
                            let qualified = resolve_status_qualifier(event_type, &sse_result);

                            // Capture contextId from first response
                            if self.transport.context_id.is_none()
                                && let Some(ctx) = sse_result.get("contextId").and_then(Value::as_str)
                            {
                                self.transport.context_id = Some(ctx.to_string());
                            }

                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: qualified,
                                content: sse_result.clone(),
                            });

                            // Check for terminal event (final: true)
                            if sse_result
                                .get("final")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                            {
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            // Stream closed
                            break;
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("A2A SSE parse error: {e}");
                            if sse_stream.parser.consecutive_errors() >= MAX_CONSECUTIVE_SSE_ERRORS {
                                tracing::warn!(
                                    "closing A2A connection after {} consecutive parse errors",
                                    MAX_CONSECUTIVE_SSE_ERRORS
                                );
                                break;
                            }
                        }
                        Err(_) => {
                            // Timeout
                            tracing::warn!(?stream_timeout, "A2A stream timed out");
                            break;
                        }
                    }
                }
                () = cancel.cancelled() => {
                    break;
                }
            }
        }

        Ok(DriveResult::Complete)
    }
}

// ============================================================================
// Public Constructor
// ============================================================================

/// Creates an `A2aClientDriver` for the given endpoint and configuration.
///
/// Called by the orchestration runner when an actor's mode is `"a2a_client"`.
///
/// Implements: TJ-SPEC-017 F-007
#[must_use]
pub fn create_a2a_client_driver(
    endpoint: &str,
    headers: Vec<(String, String)>,
    raw_synthesize: bool,
) -> A2aClientDriver {
    let transport = A2aClientTransport::new(endpoint, headers);
    A2aClientDriver {
        transport,
        raw_synthesize,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SSE Parser Tests ----

    #[test]
    fn parse_basic_data_event() {
        let mut parser = A2aSseParser::new();
        let input = b"data: {\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":{\"kind\":\"task\",\"id\":\"t1\"}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event["kind"], "task");
        assert_eq!(event["id"], "t1");
    }

    #[test]
    fn parse_malformed_json_skipped() {
        let mut parser = A2aSseParser::new();
        let input = b"data: not json\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
        assert_eq!(parser.consecutive_errors(), 1);
    }

    #[test]
    fn parse_consecutive_errors_tracked() {
        let mut parser = A2aSseParser::new();
        for _ in 0..5 {
            parser.feed(b"data: bad\n\n");
        }
        assert_eq!(parser.consecutive_errors(), 5);

        // Success resets the counter
        parser.feed(b"data: {\"result\":{\"ok\":true}}\n\n");
        assert_eq!(parser.consecutive_errors(), 0);
    }

    #[test]
    fn parse_multiple_events_one_chunk() {
        let mut parser = A2aSseParser::new();
        let input = b"data: {\"result\":{\"kind\":\"task\"}}\n\ndata: {\"result\":{\"kind\":\"status-update\"}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 2);
        assert!(events[0].is_ok());
        assert!(events[1].is_ok());
    }

    #[test]
    fn parse_incremental_chunks() {
        let mut parser = A2aSseParser::new();

        let events1 = parser.feed(b"data: {\"res");
        assert!(events1.is_empty());

        let events2 = parser.feed(b"ult\":{\"kind\":\"task\"}}\n\n");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn parse_sse_comment_ignored() {
        let mut parser = A2aSseParser::new();
        let input = b": comment\ndata: {\"result\":{\"ok\":true}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
    }

    #[test]
    fn parse_empty_data_ignored() {
        let mut parser = A2aSseParser::new();
        let input = b"\n\n";
        let events = parser.feed(input);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_extracts_result_field() {
        let mut parser = A2aSseParser::new();
        let input = b"data: {\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":{\"kind\":\"status-update\",\"taskId\":\"t1\",\"status\":{\"state\":\"completed\"},\"final\":true}}\n\n";
        let events = parser.feed(input);

        let event = events[0].as_ref().unwrap();
        assert_eq!(event["kind"], "status-update");
        assert_eq!(event["taskId"], "t1");
        assert_eq!(event["final"], true);
    }

    // ---- Event Type Detection Tests ----

    #[test]
    fn detect_task_created() {
        let result = json!({"kind": "task", "id": "t1"});
        assert_eq!(detect_event_type(&result), "task/created");
    }

    #[test]
    fn detect_message_response() {
        let result = json!({"kind": "message", "role": "agent"});
        assert_eq!(detect_event_type(&result), "message/response");
    }

    #[test]
    fn detect_status_update() {
        let result = json!({"kind": "status-update", "status": {"state": "working"}});
        assert_eq!(detect_event_type(&result), "task/status");
    }

    #[test]
    fn detect_artifact_update() {
        let result = json!({"kind": "artifact-update", "artifact": {}});
        assert_eq!(detect_event_type(&result), "task/artifact");
    }

    #[test]
    fn detect_unknown_kind() {
        let result = json!({"kind": "future-type"});
        assert_eq!(detect_event_type(&result), "unknown");
    }

    #[test]
    fn detect_missing_kind() {
        let result = json!({"data": "no kind"});
        assert_eq!(detect_event_type(&result), "unknown");
    }

    // ---- Status Qualifier Tests ----

    #[test]
    fn status_qualifier_resolution() {
        let result =
            json!({"kind": "status-update", "status": {"state": "completed"}, "final": true});
        assert_eq!(
            resolve_status_qualifier("task/status", &result),
            "task/status:completed"
        );
    }

    #[test]
    fn status_qualifier_input_required() {
        let result = json!({"kind": "status-update", "status": {"state": "input-required"}});
        assert_eq!(
            resolve_status_qualifier("task/status", &result),
            "task/status:input-required"
        );
    }

    #[test]
    fn status_qualifier_auth_required() {
        let result = json!({"kind": "status-update", "status": {"state": "auth-required"}});
        assert_eq!(
            resolve_status_qualifier("task/status", &result),
            "task/status:auth-required"
        );
    }

    #[test]
    fn status_qualifier_rejected() {
        let result = json!({"kind": "status-update", "status": {"state": "rejected"}});
        assert_eq!(
            resolve_status_qualifier("task/status", &result),
            "task/status:rejected"
        );
    }

    #[test]
    fn status_qualifier_non_status_event() {
        let result = json!({"kind": "task"});
        assert_eq!(
            resolve_status_qualifier("task/created", &result),
            "task/created"
        );
    }

    // ---- Task Message Construction Tests ----

    #[test]
    fn build_task_message_from_state() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "Hello"}]
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, false).unwrap();

        assert_eq!(msg["jsonrpc"], "2.0");
        assert_eq!(msg["method"], "message/send");
        assert_eq!(msg["params"]["message"]["role"], "user");
        assert_eq!(msg["params"]["message"]["kind"], "message");
        assert!(msg["params"]["message"]["messageId"].is_string());
    }

    #[test]
    fn build_task_message_streaming() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "Stream this"}]
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, true).unwrap();
        assert_eq!(msg["method"], "message/stream");
    }

    #[test]
    fn missing_task_message_errors() {
        let state = json!({"other_key": "value"});
        let result = build_task_message(&state, &HashMap::new(), None, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("task_message"), "got: {err}");
    }

    #[test]
    fn auto_generate_message_id() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "test"}]
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, false).unwrap();
        let message_id = msg["params"]["message"]["messageId"].as_str().unwrap();
        assert!(!message_id.is_empty());
    }

    #[test]
    fn preserve_existing_message_id() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "test"}],
                "messageId": "custom-id"
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, false).unwrap();
        assert_eq!(msg["params"]["message"]["messageId"], "custom-id");
    }

    #[test]
    fn context_id_persistence() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "test"}]
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), Some("ctx-123"), false).unwrap();
        assert_eq!(msg["params"]["message"]["contextId"], "ctx-123");
    }

    #[test]
    fn context_id_not_overridden() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [],
                "contextId": "explicit-ctx"
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), Some("auto-ctx"), false).unwrap();
        assert_eq!(msg["params"]["message"]["contextId"], "explicit-ctx");
    }

    #[test]
    fn build_with_interpolation() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "Use {{tool_name}}"}]
            }
        });
        let mut extractors = HashMap::new();
        extractors.insert("tool_name".to_string(), "calculator".to_string());

        let msg = build_task_message(&state, &extractors, None, false).unwrap();
        let text = msg["params"]["message"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text, "Use calculator");
    }

    #[test]
    fn build_with_configuration() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "test"}]
            },
            "configuration": {
                "acceptedOutputModes": ["text/plain"],
                "historyLength": 0
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, false).unwrap();
        assert!(msg["params"]["configuration"].is_object());
        assert_eq!(msg["params"]["configuration"]["historyLength"], 0);
    }

    #[test]
    fn build_with_metadata() {
        let state = json!({
            "task_message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "test"}]
            },
            "metadata": {
                "source": "test-harness"
            }
        });
        let msg = build_task_message(&state, &HashMap::new(), None, false).unwrap();
        assert_eq!(msg["params"]["metadata"]["source"], "test-harness");
    }

    // ---- Kind Detection Tests (Sync Response) ----

    #[test]
    fn sync_response_kind_task() {
        let result = json!({"kind": "task", "id": "t1", "status": {"state": "completed"}});
        assert_eq!(detect_event_type(&result), "task/created");
    }

    #[test]
    fn sync_response_kind_message() {
        let result = json!({"kind": "message", "role": "agent", "parts": []});
        assert_eq!(detect_event_type(&result), "message/response");
    }

    // ---- Transport Tests ----

    #[test]
    fn transport_creation() {
        let transport = A2aClientTransport::new("http://localhost:9090", vec![]);
        assert_eq!(transport.agent_url, "http://localhost:9090");
        assert!(transport.context_id.is_none());
        assert!(transport.headers.is_empty());
    }

    #[test]
    fn transport_with_headers() {
        let headers = vec![("Authorization".to_string(), "Bearer token".to_string())];
        let transport = A2aClientTransport::new("http://localhost:9090", headers);
        assert_eq!(transport.headers.len(), 1);
        assert_eq!(transport.headers[0].0, "Authorization");
    }

    // ---- Driver Creation Tests ----

    #[test]
    fn create_driver() {
        let driver = create_a2a_client_driver(
            "http://localhost:9090",
            vec![("Auth".to_string(), "Bearer x".to_string())],
            true,
        );
        assert!(driver.raw_synthesize);
        assert_eq!(driver.transport.agent_url, "http://localhost:9090");
        assert_eq!(driver.transport.headers.len(), 1);
    }
}
