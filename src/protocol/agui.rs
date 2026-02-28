//! AG-UI client-mode `PhaseDriver` implementation.
//!
//! `AgUiDriver` sends HTTP POST requests with `RunAgentInput` payloads to
//! an AG-UI agent endpoint and consumes the SSE response stream. Each SSE
//! event is mapped to an OATF event type and emitted as a `ProtocolEvent`
//! for the `PhaseLoop` to process.
//!
//! This is a client-mode driver: it initiates requests rather than waiting
//! for them. Extractors are cloned once at the start of `drive_phase()`.
//!
//! See TJ-SPEC-016 for the full AG-UI protocol support specification.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use oatf::primitives::interpolate_value;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;

// ============================================================================
// Constants
// ============================================================================

/// Default run timeout per spec §3.4.
const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum consecutive SSE parse errors before closing the connection (§9.2).
const MAX_CONSECUTIVE_ERRORS: usize = 10;

/// Maximum retry attempts for HTTP 429 responses (EC-AGUI-004).
const MAX_RETRIES: u32 = 3;

/// Initial retry backoff for 429 responses.
const INITIAL_RETRY_BACKOFF: Duration = Duration::from_secs(1);

// ============================================================================
// SSE Event Type Mapping
// ============================================================================

/// Maps AG-UI `SCREAMING_SNAKE_CASE` SSE event types to OATF `snake_case` types.
///
/// Unknown event types are returned as-is per EC-AGUI-012.
///
/// Implements: TJ-SPEC-016 F-001
#[must_use]
fn map_event_type(raw: &str) -> &str {
    match raw {
        // Lifecycle
        "RUN_STARTED" => "run_started",
        "RUN_FINISHED" => "run_finished",
        "RUN_ERROR" => "run_error",
        "STEP_STARTED" => "step_started",
        "STEP_FINISHED" => "step_finished",
        // Text
        "TEXT_MESSAGE_START" => "text_message_start",
        "TEXT_MESSAGE_CONTENT" => "text_message_content",
        "TEXT_MESSAGE_END" => "text_message_end",
        // Tool
        "TOOL_CALL_START" => "tool_call_start",
        "TOOL_CALL_ARGS" => "tool_call_args",
        "TOOL_CALL_END" => "tool_call_end",
        "TOOL_CALL_RESULT" => "tool_call_result",
        // State
        "STATE_SNAPSHOT" => "state_snapshot",
        "STATE_DELTA" => "state_delta",
        "MESSAGES_SNAPSHOT" => "messages_snapshot",
        // Activity
        "ACTIVITY_SNAPSHOT" => "activity_snapshot",
        "ACTIVITY_DELTA" => "activity_delta",
        // Reasoning
        "REASONING_START" => "reasoning_start",
        "REASONING_MESSAGE_START" => "reasoning_message_start",
        "REASONING_MESSAGE_CONTENT" => "reasoning_message_content",
        "REASONING_MESSAGE_END" => "reasoning_message_end",
        "REASONING_MESSAGE_CHUNK" => "reasoning_message_chunk",
        "REASONING_END" => "reasoning_end",
        "REASONING_ENCRYPTED_VALUE" => "reasoning_encrypted_value",
        // Special
        "RAW" => "raw",
        "CUSTOM" => "custom",
        // Unknown — pass through as-is
        _ => raw,
    }
}

// ============================================================================
// AgUiEvent
// ============================================================================

/// A parsed AG-UI SSE event with both raw and OATF-mapped type.
///
/// Implements: TJ-SPEC-016 F-001
#[derive(Debug, Clone)]
struct AgUiEvent {
    /// OATF `snake_case` event type (e.g., `"run_started"`).
    event_type: String,
    /// Parsed JSON data payload.
    data: Value,
    /// Raw SSE `SCREAMING_SNAKE_CASE` type (e.g., `"RUN_STARTED"`).
    /// Used in tests for verification.
    #[allow(dead_code)]
    raw_type: String,
}

// ============================================================================
// SseParser
// ============================================================================

/// Incremental SSE frame parser.
///
/// Reads bytes, accumulates lines, and yields complete `AgUiEvent` values.
/// Handles multi-line data fields, malformed JSON (skip with warning),
/// and the CUSTOM event subtype detection for `interrupt`.
///
/// Implements: TJ-SPEC-016 F-001
struct SseParser {
    /// Line accumulation buffer.
    buffer: String,
    /// Current SSE `event:` field value.
    current_event_type: Option<String>,
    /// Accumulated `data:` field content (may span multiple lines).
    current_data: String,
    /// Number of consecutive parse errors (resets on success).
    consecutive_errors: usize,
}

impl SseParser {
    /// Creates a new SSE parser.
    #[must_use]
    const fn new() -> Self {
        Self {
            buffer: String::new(),
            current_event_type: None,
            current_data: String::new(),
            consecutive_errors: 0,
        }
    }

    /// Feed raw bytes into the parser and extract any complete events.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<AgUiEvent, String>> {
        let text = String::from_utf8_lossy(bytes);
        self.buffer.push_str(&text);

        let mut events = Vec::new();

        // Process complete lines (terminated by \n)
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
            } else if let Some(value) = line.strip_prefix("event:") {
                self.current_event_type = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.current_data.is_empty() {
                    self.current_data.push('\n');
                }
                self.current_data.push_str(value.trim_start());
            } else if line.starts_with(':') {
                // SSE comment — ignore
            }
            // Other lines are ignored per SSE spec
        }

        events
    }

    /// Dispatch an accumulated event (called on blank line).
    fn dispatch_event(&mut self) -> Option<Result<AgUiEvent, String>> {
        if self.current_data.is_empty() && self.current_event_type.is_none() {
            return None;
        }

        let raw_type = self.current_event_type.take().unwrap_or_default();
        let data_str = std::mem::take(&mut self.current_data);

        if data_str.is_empty() {
            return None;
        }

        let data: Value = match serde_json::from_str(&data_str) {
            Ok(v) => v,
            Err(e) => {
                self.consecutive_errors += 1;
                return Some(Err(format!(
                    "malformed JSON in SSE data for event '{raw_type}': {e}"
                )));
            }
        };

        self.consecutive_errors = 0;

        // CUSTOM event subtype detection: if name == "interrupt" → event type "interrupt"
        let event_type = if raw_type == "CUSTOM" {
            if data
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n == "interrupt")
            {
                "interrupt".to_string()
            } else {
                map_event_type(&raw_type).to_string()
            }
        } else {
            map_event_type(&raw_type).to_string()
        };

        Some(Ok(AgUiEvent {
            event_type,
            data,
            raw_type,
        }))
    }

    /// Returns the current consecutive error count.
    #[must_use]
    const fn consecutive_errors(&self) -> usize {
        self.consecutive_errors
    }
}

// ============================================================================
// SseStream
// ============================================================================

/// Wraps a `reqwest::Response` byte stream with an `SseParser`.
///
/// Implements: TJ-SPEC-016 F-001
struct SseStream {
    parser: SseParser,
    stream: Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    /// Buffered events from the parser (multiple events can come from one chunk).
    pending: Vec<Result<AgUiEvent, String>>,
}

impl SseStream {
    /// Creates a new SSE stream from a reqwest response.
    fn new(response: reqwest::Response) -> Self {
        Self {
            parser: SseParser::new(),
            stream: Box::pin(response.bytes_stream()),
            pending: Vec::new(),
        }
    }

    /// Reads the next complete SSE event from the stream.
    ///
    /// Returns `None` when the stream is exhausted.
    ///
    /// # Errors
    ///
    /// Returns an error on HTTP stream errors or malformed SSE data.
    async fn next_event(&mut self) -> Result<Option<AgUiEvent>, EngineError> {
        loop {
            // Drain any buffered events first
            if let Some(result) = self.pending.pop() {
                return match result {
                    Ok(event) => Ok(Some(event)),
                    Err(msg) => Err(EngineError::Driver(msg)),
                };
            }

            // Read next chunk from the byte stream
            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    let mut events = self.parser.feed(&bytes);
                    // Reverse so we can pop from the end in order
                    events.reverse();
                    self.pending = events;
                    // Continue loop to drain pending
                }
                Some(Err(e)) => {
                    return Err(EngineError::Driver(format!("SSE stream error: {e}")));
                }
                None => {
                    // Stream exhausted
                    return Ok(None);
                }
            }
        }
    }
}

// ============================================================================
// MessageAccumulator
// ============================================================================

/// Accumulated text message from SSE deltas.
#[derive(Debug, Clone)]
struct AccumulatedMessage {
    message_id: String,
    role: String,
    content: String,
    tool_calls: Vec<String>,
    complete: bool,
}

/// Accumulated tool call from SSE deltas.
#[derive(Debug, Clone)]
struct AccumulatedToolCall {
    tool_call_id: String,
    tool_call_name: String,
    /// Tracked for potential future use in cross-referencing.
    #[allow(dead_code)]
    parent_message_id: Option<String>,
    arguments: String,
    result: Option<Value>,
    complete: bool,
}

/// Accumulated reasoning message from SSE deltas.
#[derive(Debug, Clone)]
struct AccumulatedReasoning {
    message_id: String,
    content: String,
    complete: bool,
}

/// Accumulates AG-UI streaming deltas into complete messages.
///
/// Tracks text messages, tool calls, and reasoning messages across the
/// SSE stream. Produces the `_accumulated_response` synthetic event
/// at stream completion.
///
/// Implements: TJ-SPEC-016 F-001
struct MessageAccumulator {
    messages: HashMap<String, AccumulatedMessage>,
    tool_calls: HashMap<String, AccumulatedToolCall>,
    reasoning: HashMap<String, AccumulatedReasoning>,
}

impl MessageAccumulator {
    /// Creates a new empty accumulator.
    #[must_use]
    fn new() -> Self {
        Self {
            messages: HashMap::new(),
            tool_calls: HashMap::new(),
            reasoning: HashMap::new(),
        }
    }

    /// Process an SSE event and update accumulated state.
    fn process_event(&mut self, event: &AgUiEvent) {
        match event.event_type.as_str() {
            "text_message_start" | "text_message_content" | "text_message_end" => {
                self.process_text_event(event);
            }
            "tool_call_start" | "tool_call_args" | "tool_call_end" | "tool_call_result" => {
                self.process_tool_event(event);
            }
            "reasoning_message_start"
            | "reasoning_message_content"
            | "reasoning_message_end"
            | "reasoning_message_chunk" => {
                self.process_reasoning_event(event);
            }
            _ => {}
        }
    }

    /// Process text message events (start/content/end).
    fn process_text_event(&mut self, event: &AgUiEvent) {
        let message_id = event
            .data
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match event.event_type.as_str() {
            "text_message_start" => {
                let role = event
                    .data
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("assistant")
                    .to_string();
                self.messages.insert(
                    message_id.to_string(),
                    AccumulatedMessage {
                        message_id: message_id.to_string(),
                        role,
                        content: String::new(),
                        tool_calls: Vec::new(),
                        complete: false,
                    },
                );
            }
            "text_message_content" => {
                if let Some(msg) = self.messages.get_mut(message_id)
                    && let Some(delta) = event.data.get("delta").and_then(Value::as_str)
                {
                    msg.content.push_str(delta);
                }
            }
            "text_message_end" => {
                if let Some(msg) = self.messages.get_mut(message_id) {
                    msg.complete = true;
                }
            }
            _ => {}
        }
    }

    /// Process tool call events (start/args/end/result).
    fn process_tool_event(&mut self, event: &AgUiEvent) {
        let tool_call_id = event
            .data
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match event.event_type.as_str() {
            "tool_call_start" => {
                let tool_call_name = event
                    .data
                    .get("toolCallName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let parent_message_id = event
                    .data
                    .get("parentMessageId")
                    .and_then(Value::as_str)
                    .map(String::from);

                // Link to parent message
                if let Some(ref parent_id) = parent_message_id
                    && let Some(msg) = self.messages.get_mut(parent_id.as_str())
                {
                    msg.tool_calls.push(tool_call_id.to_string());
                }

                self.tool_calls.insert(
                    tool_call_id.to_string(),
                    AccumulatedToolCall {
                        tool_call_id: tool_call_id.to_string(),
                        tool_call_name,
                        parent_message_id,
                        arguments: String::new(),
                        result: None,
                        complete: false,
                    },
                );
            }
            "tool_call_args" => {
                if let Some(tc) = self.tool_calls.get_mut(tool_call_id)
                    && let Some(delta) = event.data.get("delta").and_then(Value::as_str)
                {
                    tc.arguments.push_str(delta);
                }
            }
            "tool_call_end" => {
                if let Some(tc) = self.tool_calls.get_mut(tool_call_id) {
                    tc.complete = true;
                }
            }
            "tool_call_result" => {
                if let Some(tc) = self.tool_calls.get_mut(tool_call_id) {
                    tc.result = event.data.get("result").cloned();
                }
            }
            _ => {}
        }
    }

    /// Process reasoning events (start/content/end/chunk).
    fn process_reasoning_event(&mut self, event: &AgUiEvent) {
        let message_id = event
            .data
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match event.event_type.as_str() {
            "reasoning_message_start" => {
                self.reasoning.insert(
                    message_id.to_string(),
                    AccumulatedReasoning {
                        message_id: message_id.to_string(),
                        content: String::new(),
                        complete: false,
                    },
                );
            }
            "reasoning_message_content" => {
                if let Some(r) = self.reasoning.get_mut(message_id)
                    && let Some(delta) = event.data.get("delta").and_then(Value::as_str)
                {
                    r.content.push_str(delta);
                }
            }
            "reasoning_message_end" => {
                if let Some(r) = self.reasoning.get_mut(message_id) {
                    r.complete = true;
                }
            }
            "reasoning_message_chunk" => {
                // Non-streaming convenience: complete reasoning in one event
                let content = event
                    .data
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.reasoning.insert(
                    message_id.to_string(),
                    AccumulatedReasoning {
                        message_id: message_id.to_string(),
                        content,
                        complete: true,
                    },
                );
            }
            _ => {}
        }
    }

    /// Builds the `_accumulated_response` synthetic event content.
    fn accumulated_response(&self) -> Value {
        let messages: Vec<Value> = self
            .messages
            .values()
            .map(|msg| {
                let tool_calls: Vec<Value> = msg
                    .tool_calls
                    .iter()
                    .filter_map(|tc_id| self.tool_calls.get(tc_id))
                    .map(|tc| {
                        json!({
                            "id": tc.tool_call_id,
                            "name": tc.tool_call_name,
                            "arguments": tc.arguments,
                            "result": tc.result,
                        })
                    })
                    .collect();

                json!({
                    "id": msg.message_id,
                    "role": msg.role,
                    "content": msg.content,
                    "tool_calls": tool_calls,
                })
            })
            .collect();

        let reasoning: Vec<Value> = self
            .reasoning
            .values()
            .map(|r| {
                json!({
                    "id": r.message_id,
                    "content": r.content,
                })
            })
            .collect();

        json!({
            "messages": messages,
            "reasoning": reasoning,
        })
    }

    /// Resets the accumulator for a new run.
    fn reset(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.reasoning.clear();
    }
}

// ============================================================================
// RunAgentInput
// ============================================================================

/// AG-UI `RunAgentInput` request payload.
///
/// Implements: TJ-SPEC-016 F-001
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunAgentInput {
    thread_id: String,
    run_id: String,
    messages: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarded_props: Option<Value>,
}

/// Builds a `RunAgentInput` from the phase's effective state and extractors.
///
/// # Errors
///
/// Returns `EngineError::Driver` if `state["run_agent_input"]` is missing
/// or if a `synthesize` block is present (not yet supported).
///
/// Implements: TJ-SPEC-016 F-001
fn build_run_agent_input(
    state: &Value,
    extractors: &HashMap<String, String>,
    thread_id: &str,
) -> Result<RunAgentInput, EngineError> {
    let run_agent_input = state.get("run_agent_input").ok_or_else(|| {
        EngineError::Driver(
            "AG-UI phase state missing 'run_agent_input' key — \
             each AG-UI phase must define state.run_agent_input with at least 'messages'"
                .to_string(),
        )
    })?;

    // Interpolate the entire run_agent_input subtree
    let (interpolated, _) = interpolate_value(run_agent_input, extractors, None, None);

    // Check for synthesize block (not yet supported)
    if interpolated.get("synthesize").is_some() {
        return Err(EngineError::Driver(
            "synthesize not yet supported — GenerationProvider not available".to_string(),
        ));
    }

    // Extract messages (required)
    let messages = interpolated
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Auto-generate message IDs if missing
    let messages: Vec<Value> = messages
        .into_iter()
        .map(|mut msg| {
            if msg.get("id").is_none() || msg["id"].is_null() {
                msg["id"] = Value::String(uuid::Uuid::new_v4().to_string());
            }
            msg
        })
        .collect();

    // Use threadId from document or transport's persistent ID
    let doc_thread_id = interpolated
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or(thread_id);

    // Use runId from document or generate a new one
    let run_id = interpolated
        .get("runId")
        .and_then(Value::as_str)
        .map_or_else(|| uuid::Uuid::new_v4().to_string(), String::from);

    Ok(RunAgentInput {
        thread_id: doc_thread_id.to_string(),
        run_id,
        messages,
        tools: interpolated.get("tools").and_then(Value::as_array).cloned(),
        context: interpolated
            .get("context")
            .and_then(Value::as_array)
            .cloned(),
        state: interpolated.get("state").cloned(),
        forwarded_props: interpolated.get("forwardedProps").cloned(),
    })
}

// ============================================================================
// AgUiTransport
// ============================================================================

/// HTTP transport for AG-UI agent communication.
///
/// Manages the HTTP client, persistent `thread_id`, and custom headers.
/// Each `send_run()` call creates a new HTTP POST + SSE stream.
///
/// Implements: TJ-SPEC-016 F-001
struct AgUiTransport {
    agent_url: String,
    client: reqwest::Client,
    thread_id: String,
    headers: Vec<(String, String)>,
}

impl AgUiTransport {
    /// Creates a new AG-UI transport.
    ///
    /// Generates a persistent `thread_id` for conversation continuity
    /// across phases.
    fn new(endpoint: &str, headers: Vec<(String, String)>) -> Self {
        Self {
            agent_url: endpoint.to_string(),
            client: reqwest::Client::new(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            headers,
        }
    }

    /// Returns the persistent thread ID.
    fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Sends a `RunAgentInput` and returns an SSE stream.
    ///
    /// Retries on HTTP 429 with exponential backoff (up to 3 retries).
    /// Returns an error on 4xx/5xx responses.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on HTTP errors or connection failures.
    async fn send_run(&self, input: &RunAgentInput) -> Result<SseStream, EngineError> {
        let mut backoff = INITIAL_RETRY_BACKOFF;

        for attempt in 0..=MAX_RETRIES {
            let mut request = self
                .client
                .post(&self.agent_url)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream");

            for (key, value) in &self.headers {
                request = request.header(key.as_str(), value.as_str());
            }

            let response = request
                .json(input)
                .send()
                .await
                .map_err(|e| EngineError::Driver(format!("AG-UI HTTP request failed: {e}")))?;

            let status = response.status();

            if status.is_success() {
                return Ok(SseStream::new(response));
            }

            if status.as_u16() == 429 && attempt < MAX_RETRIES {
                tracing::warn!(
                    attempt = attempt + 1,
                    max_retries = MAX_RETRIES,
                    backoff_ms = backoff.as_millis(),
                    "AG-UI agent returned 429, retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(EngineError::Driver(format!(
                "AG-UI agent returned HTTP {status}: {body}"
            )));
        }

        unreachable!("retry loop should return before exceeding MAX_RETRIES")
    }
}

// ============================================================================
// AgUiDriver
// ============================================================================

/// AG-UI client-mode protocol driver.
///
/// Sends `RunAgentInput` HTTP POST requests to an AG-UI agent and
/// consumes the SSE response stream. Each SSE event is mapped to an
/// OATF event type and emitted as a `ProtocolEvent`.
///
/// Client-mode extractors are cloned once per `drive_phase()` call.
/// Multi-run phases are handled by the `PhaseLoop` re-calling
/// `drive_phase()`.
///
/// Implements: TJ-SPEC-016 F-001
pub struct AgUiDriver {
    transport: AgUiTransport,
    #[allow(dead_code)]
    raw_synthesize: bool,
    run_timeout: Duration,
    accumulator: MessageAccumulator,
}

impl AgUiDriver {
    /// Creates a new AG-UI client driver.
    ///
    /// # Arguments
    ///
    /// * `transport` — AG-UI HTTP transport.
    /// * `raw_synthesize` — If `true`, bypass synthesize output validation
    ///   (reserved for future `GenerationProvider` support).
    ///
    /// Implements: TJ-SPEC-016 F-001
    #[must_use]
    fn new(transport: AgUiTransport, raw_synthesize: bool) -> Self {
        Self {
            transport,
            raw_synthesize,
            run_timeout: DEFAULT_RUN_TIMEOUT,
            accumulator: MessageAccumulator::new(),
        }
    }
}

#[async_trait]
impl PhaseDriver for AgUiDriver {
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

        // Build RunAgentInput from state + extractors
        let input = build_run_agent_input(state, &current_extractors, self.transport.thread_id())?;

        // Emit outgoing request event
        let input_value = serde_json::to_value(&input)
            .map_err(|e| EngineError::Driver(format!("failed to serialize RunAgentInput: {e}")))?;
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "run_agent_input".to_string(),
            content: input_value,
        });

        // Send request, get SSE stream
        let mut stream = self.transport.send_run(&input).await?;

        // Reset accumulator for this run
        self.accumulator.reset();

        // Parse SSE events and emit them
        let run_timeout = self.run_timeout;
        loop {
            tokio::select! {
                result = tokio::time::timeout(run_timeout, stream.next_event()) => {
                    match result {
                        Ok(Ok(Some(event))) => {
                            // Update accumulator
                            self.accumulator.process_event(&event);

                            // Emit incoming event
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: event.event_type,
                                content: event.data,
                            });
                        }
                        Ok(Ok(None)) => {
                            // Stream closed — emit accumulated response and complete
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: "_accumulated_response".to_string(),
                                content: self.accumulator.accumulated_response(),
                            });
                            return Ok(DriveResult::Complete);
                        }
                        Ok(Err(e)) => {
                            // Parse error — warn and continue (up to MAX_CONSECUTIVE_ERRORS)
                            tracing::warn!("SSE parse error: {e}");
                            if stream.parser.consecutive_errors() >= MAX_CONSECUTIVE_ERRORS {
                                tracing::warn!(
                                    "closing AG-UI connection after {} consecutive parse errors",
                                    MAX_CONSECUTIVE_ERRORS
                                );
                                let _ = event_tx.send(ProtocolEvent {
                                    direction: Direction::Incoming,
                                    method: "_accumulated_response".to_string(),
                                    content: self.accumulator.accumulated_response(),
                                });
                                return Ok(DriveResult::Complete);
                            }
                        }
                        Err(_) => {
                            // Timeout
                            tracing::warn!(?run_timeout, "AG-UI run timed out");
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: "_accumulated_response".to_string(),
                                content: self.accumulator.accumulated_response(),
                            });
                            return Ok(DriveResult::Complete);
                        }
                    }
                }
                () = cancel.cancelled() => {
                    return Ok(DriveResult::Complete);
                }
            }
        }
    }
}

// ============================================================================
// Public constructor for runner integration
// ============================================================================

/// Creates an `AgUiDriver` for the given endpoint and configuration.
///
/// Called by the orchestration runner when an actor's mode is
/// `"ag_ui_client"`.
///
/// Implements: TJ-SPEC-016 F-001
#[must_use]
pub fn create_agui_driver(
    endpoint: &str,
    headers: Vec<(String, String)>,
    raw_synthesize: bool,
) -> AgUiDriver {
    let transport = AgUiTransport::new(endpoint, headers);
    AgUiDriver::new(transport, raw_synthesize)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SSE Event Type Mapping Tests ----

    #[test]
    #[allow(clippy::cognitive_complexity)]
    fn map_all_26_event_types() {
        // Lifecycle
        assert_eq!(map_event_type("RUN_STARTED"), "run_started");
        assert_eq!(map_event_type("RUN_FINISHED"), "run_finished");
        assert_eq!(map_event_type("RUN_ERROR"), "run_error");
        assert_eq!(map_event_type("STEP_STARTED"), "step_started");
        assert_eq!(map_event_type("STEP_FINISHED"), "step_finished");
        // Text
        assert_eq!(map_event_type("TEXT_MESSAGE_START"), "text_message_start");
        assert_eq!(
            map_event_type("TEXT_MESSAGE_CONTENT"),
            "text_message_content"
        );
        assert_eq!(map_event_type("TEXT_MESSAGE_END"), "text_message_end");
        // Tool
        assert_eq!(map_event_type("TOOL_CALL_START"), "tool_call_start");
        assert_eq!(map_event_type("TOOL_CALL_ARGS"), "tool_call_args");
        assert_eq!(map_event_type("TOOL_CALL_END"), "tool_call_end");
        assert_eq!(map_event_type("TOOL_CALL_RESULT"), "tool_call_result");
        // State
        assert_eq!(map_event_type("STATE_SNAPSHOT"), "state_snapshot");
        assert_eq!(map_event_type("STATE_DELTA"), "state_delta");
        assert_eq!(map_event_type("MESSAGES_SNAPSHOT"), "messages_snapshot");
        // Activity
        assert_eq!(map_event_type("ACTIVITY_SNAPSHOT"), "activity_snapshot");
        assert_eq!(map_event_type("ACTIVITY_DELTA"), "activity_delta");
        // Reasoning
        assert_eq!(map_event_type("REASONING_START"), "reasoning_start");
        assert_eq!(
            map_event_type("REASONING_MESSAGE_START"),
            "reasoning_message_start"
        );
        assert_eq!(
            map_event_type("REASONING_MESSAGE_CONTENT"),
            "reasoning_message_content"
        );
        assert_eq!(
            map_event_type("REASONING_MESSAGE_END"),
            "reasoning_message_end"
        );
        assert_eq!(
            map_event_type("REASONING_MESSAGE_CHUNK"),
            "reasoning_message_chunk"
        );
        assert_eq!(map_event_type("REASONING_END"), "reasoning_end");
        assert_eq!(
            map_event_type("REASONING_ENCRYPTED_VALUE"),
            "reasoning_encrypted_value"
        );
        // Special
        assert_eq!(map_event_type("RAW"), "raw");
        assert_eq!(map_event_type("CUSTOM"), "custom");
    }

    #[test]
    fn unknown_event_type_passes_through() {
        assert_eq!(map_event_type("FUTURE_EVENT"), "FUTURE_EVENT");
        assert_eq!(map_event_type("something_else"), "something_else");
    }

    // ---- SSE Parser Tests ----

    #[test]
    fn parse_basic_sse_event() {
        let mut parser = SseParser::new();
        let input = b"event: RUN_STARTED\ndata: {\"threadId\":\"abc\"}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.event_type, "run_started");
        assert_eq!(event.raw_type, "RUN_STARTED");
        assert_eq!(event.data["threadId"], "abc");
    }

    #[test]
    fn parse_multiline_data() {
        let mut parser = SseParser::new();
        let input = b"event: TEXT_MESSAGE_CONTENT\ndata: {\"messageId\":\"m1\",\ndata: \"delta\":\"hello\"}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.event_type, "text_message_content");
        assert_eq!(event.data["messageId"], "m1");
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let mut parser = SseParser::new();
        let input = b"event: RUN_STARTED\ndata: not-json\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
        assert_eq!(parser.consecutive_errors(), 1);
    }

    #[test]
    fn parse_consecutive_errors_counted() {
        let mut parser = SseParser::new();
        for _ in 0..10 {
            let input = b"event: X\ndata: bad\n\n";
            parser.feed(input);
        }
        assert_eq!(parser.consecutive_errors(), 10);
    }

    #[test]
    fn parse_success_resets_consecutive_errors() {
        let mut parser = SseParser::new();
        parser.feed(b"event: X\ndata: bad\n\n");
        assert_eq!(parser.consecutive_errors(), 1);
        parser.feed(b"event: RUN_STARTED\ndata: {}\n\n");
        assert_eq!(parser.consecutive_errors(), 0);
    }

    #[test]
    fn parse_custom_interrupt() {
        let mut parser = SseParser::new();
        let input = b"event: CUSTOM\ndata: {\"name\":\"interrupt\",\"message\":\"stop\"}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.event_type, "interrupt");
        assert_eq!(event.raw_type, "CUSTOM");
    }

    #[test]
    fn parse_custom_non_interrupt() {
        let mut parser = SseParser::new();
        let input = b"event: CUSTOM\ndata: {\"name\":\"my_event\",\"value\":42}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.event_type, "custom");
    }

    #[test]
    fn parse_multiple_events_in_one_chunk() {
        let mut parser = SseParser::new();
        let input =
            b"event: RUN_STARTED\ndata: {\"a\":1}\n\nevent: RUN_FINISHED\ndata: {\"b\":2}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 2);
        let e1 = events[0].as_ref().unwrap();
        let e2 = events[1].as_ref().unwrap();
        assert_eq!(e1.event_type, "run_started");
        assert_eq!(e2.event_type, "run_finished");
    }

    #[test]
    fn parse_sse_comment_ignored() {
        let mut parser = SseParser::new();
        let input = b": this is a comment\nevent: RUN_STARTED\ndata: {}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap().event_type, "run_started");
    }

    #[test]
    fn parse_incremental_chunks() {
        let mut parser = SseParser::new();

        // Partial chunk 1
        let events1 = parser.feed(b"event: RUN_S");
        assert!(events1.is_empty());

        // Partial chunk 2
        let events2 = parser.feed(b"TARTED\ndata: ");
        assert!(events2.is_empty());

        // Completing chunk
        let events3 = parser.feed(b"{\"ok\":true}\n\n");
        assert_eq!(events3.len(), 1);
        assert_eq!(events3[0].as_ref().unwrap().event_type, "run_started");
    }

    // ---- MessageAccumulator Tests ----

    #[test]
    fn accumulate_text_message() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "text_message_start".to_string(),
            data: json!({"messageId": "m1", "role": "assistant"}),
            raw_type: "TEXT_MESSAGE_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "text_message_content".to_string(),
            data: json!({"messageId": "m1", "delta": "Hello "}),
            raw_type: "TEXT_MESSAGE_CONTENT".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "text_message_content".to_string(),
            data: json!({"messageId": "m1", "delta": "world!"}),
            raw_type: "TEXT_MESSAGE_CONTENT".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "text_message_end".to_string(),
            data: json!({"messageId": "m1"}),
            raw_type: "TEXT_MESSAGE_END".to_string(),
        });

        let result = acc.accumulated_response();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Hello world!");
        assert_eq!(messages[0]["role"], "assistant");
    }

    #[test]
    fn accumulate_tool_call() {
        let mut acc = MessageAccumulator::new();

        // Parent message
        acc.process_event(&AgUiEvent {
            event_type: "text_message_start".to_string(),
            data: json!({"messageId": "m1", "role": "assistant"}),
            raw_type: "TEXT_MESSAGE_START".to_string(),
        });

        // Tool call
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_start".to_string(),
            data: json!({
                "toolCallId": "tc1",
                "toolCallName": "calculator",
                "parentMessageId": "m1"
            }),
            raw_type: "TOOL_CALL_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_args".to_string(),
            data: json!({"toolCallId": "tc1", "delta": "{\"expr\":"}),
            raw_type: "TOOL_CALL_ARGS".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_args".to_string(),
            data: json!({"toolCallId": "tc1", "delta": "\"2+2\"}"}),
            raw_type: "TOOL_CALL_ARGS".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_end".to_string(),
            data: json!({"toolCallId": "tc1"}),
            raw_type: "TOOL_CALL_END".to_string(),
        });

        let result = acc.accumulated_response();
        let messages = result["messages"].as_array().unwrap();
        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["name"], "calculator");
        assert_eq!(tool_calls[0]["arguments"], "{\"expr\":\"2+2\"}");
    }

    #[test]
    fn accumulate_tool_call_result() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "tool_call_start".to_string(),
            data: json!({"toolCallId": "tc1", "toolCallName": "calc"}),
            raw_type: "TOOL_CALL_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_end".to_string(),
            data: json!({"toolCallId": "tc1"}),
            raw_type: "TOOL_CALL_END".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_result".to_string(),
            data: json!({"toolCallId": "tc1", "result": "42"}),
            raw_type: "TOOL_CALL_RESULT".to_string(),
        });

        assert!(acc.tool_calls.get("tc1").unwrap().result.is_some());
    }

    #[test]
    fn accumulate_reasoning() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "reasoning_message_start".to_string(),
            data: json!({"messageId": "r1"}),
            raw_type: "REASONING_MESSAGE_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "reasoning_message_content".to_string(),
            data: json!({"messageId": "r1", "delta": "thinking..."}),
            raw_type: "REASONING_MESSAGE_CONTENT".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "reasoning_message_end".to_string(),
            data: json!({"messageId": "r1"}),
            raw_type: "REASONING_MESSAGE_END".to_string(),
        });

        let result = acc.accumulated_response();
        let reasoning = result["reasoning"].as_array().unwrap();
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0]["content"], "thinking...");
    }

    #[test]
    fn accumulate_reasoning_chunk() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "reasoning_message_chunk".to_string(),
            data: json!({"messageId": "r1", "content": "complete thought"}),
            raw_type: "REASONING_MESSAGE_CHUNK".to_string(),
        });

        let r = acc.reasoning.get("r1").unwrap();
        assert_eq!(r.content, "complete thought");
        assert!(r.complete);
    }

    #[test]
    fn tool_call_start_without_end() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "tool_call_start".to_string(),
            data: json!({"toolCallId": "tc1", "toolCallName": "calc"}),
            raw_type: "TOOL_CALL_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_args".to_string(),
            data: json!({"toolCallId": "tc1", "delta": "partial"}),
            raw_type: "TOOL_CALL_ARGS".to_string(),
        });

        // Incomplete tool call should still be in accumulator
        let tc = acc.tool_calls.get("tc1").unwrap();
        assert_eq!(tc.arguments, "partial");
        assert!(!tc.complete);
    }

    #[test]
    fn tool_call_end_resolves_name_from_start() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "tool_call_start".to_string(),
            data: json!({"toolCallId": "tc1", "toolCallName": "calculator"}),
            raw_type: "TOOL_CALL_START".to_string(),
        });
        acc.process_event(&AgUiEvent {
            event_type: "tool_call_end".to_string(),
            data: json!({"toolCallId": "tc1"}),
            raw_type: "TOOL_CALL_END".to_string(),
        });

        let tc = acc.tool_calls.get("tc1").unwrap();
        assert_eq!(tc.tool_call_name, "calculator");
        assert!(tc.complete);
    }

    #[test]
    fn accumulator_reset_clears_all() {
        let mut acc = MessageAccumulator::new();

        acc.process_event(&AgUiEvent {
            event_type: "text_message_start".to_string(),
            data: json!({"messageId": "m1", "role": "assistant"}),
            raw_type: "TEXT_MESSAGE_START".to_string(),
        });

        acc.reset();
        assert!(acc.messages.is_empty());
        assert!(acc.tool_calls.is_empty());
        assert!(acc.reasoning.is_empty());
    }

    // ---- RunAgentInput Construction Tests ----

    #[test]
    fn build_from_full_state() {
        let state = json!({
            "run_agent_input": {
                "messages": [
                    {"role": "user", "content": "Hello"}
                ],
                "tools": [
                    {"type": "function", "function": {"name": "calc"}}
                ],
                "state": {"key": "value"},
                "forwardedProps": {"theme": "dark"}
            }
        });

        let result = build_run_agent_input(&state, &HashMap::new(), "thread-1").unwrap();
        assert_eq!(result.thread_id, "thread-1");
        assert!(!result.run_id.is_empty());
        assert_eq!(result.messages.len(), 1);
        assert!(result.tools.is_some());
        assert!(result.state.is_some());
        assert!(result.forwarded_props.is_some());
    }

    #[test]
    fn build_with_template_interpolation() {
        let state = json!({
            "run_agent_input": {
                "messages": [
                    {"role": "user", "content": "Use {{tool_name}} to calculate"}
                ]
            }
        });

        let mut extractors = HashMap::new();
        extractors.insert("tool_name".to_string(), "calculator".to_string());

        let result = build_run_agent_input(&state, &extractors, "t1").unwrap();
        let content = result.messages[0]["content"].as_str().unwrap();
        assert_eq!(content, "Use calculator to calculate");
    }

    #[test]
    fn missing_run_agent_input_errors() {
        let state = json!({"other_key": "value"});
        let result = build_run_agent_input(&state, &HashMap::new(), "t1");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("run_agent_input"), "got: {err}");
    }

    #[test]
    fn empty_messages_array_valid() {
        let state = json!({
            "run_agent_input": {
                "messages": []
            }
        });
        let result = build_run_agent_input(&state, &HashMap::new(), "t1").unwrap();
        assert!(result.messages.is_empty());
    }

    #[test]
    fn auto_generate_ids() {
        let state = json!({
            "run_agent_input": {
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            }
        });
        let result = build_run_agent_input(&state, &HashMap::new(), "t1").unwrap();
        assert!(result.messages[0]["id"].is_string());
        assert!(!result.messages[0]["id"].as_str().unwrap().is_empty());
    }

    #[test]
    fn thread_id_from_document() {
        let state = json!({
            "run_agent_input": {
                "threadId": "custom-thread",
                "messages": [{"role": "user", "content": "hi"}]
            }
        });
        let result = build_run_agent_input(&state, &HashMap::new(), "fallback").unwrap();
        assert_eq!(result.thread_id, "custom-thread");
    }

    #[test]
    fn thread_id_persistence() {
        let transport = AgUiTransport::new("http://localhost:8000", vec![]);
        let tid = transport.thread_id().to_string();
        assert!(!tid.is_empty());
        // Thread ID should be consistent across accesses
        assert_eq!(transport.thread_id(), tid);
    }

    #[test]
    fn synthesize_not_yet_supported() {
        let state = json!({
            "run_agent_input": {
                "synthesize": {
                    "prompt": "Generate messages"
                }
            }
        });
        let result = build_run_agent_input(&state, &HashMap::new(), "t1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("synthesize"));
    }

    #[test]
    fn run_agent_input_serialization() {
        let input = RunAgentInput {
            thread_id: "t1".to_string(),
            run_id: "r1".to_string(),
            messages: vec![json!({"role": "user", "content": "hi"})],
            tools: None,
            context: None,
            state: None,
            forwarded_props: None,
        };

        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["threadId"], "t1");
        assert_eq!(json["runId"], "r1");
        assert!(json.get("tools").is_none());
        assert!(json.get("forwardedProps").is_none());
    }

    #[test]
    fn run_agent_input_camel_case() {
        let input = RunAgentInput {
            thread_id: "t1".to_string(),
            run_id: "r1".to_string(),
            messages: vec![],
            tools: None,
            context: None,
            state: Some(json!({"x": 1})),
            forwarded_props: Some(json!({"y": 2})),
        };

        let json = serde_json::to_value(&input).unwrap();
        // Check camelCase serialization
        assert!(json.get("threadId").is_some());
        assert!(json.get("runId").is_some());
        assert!(json.get("forwardedProps").is_some());
        // Ensure snake_case is NOT used
        assert!(json.get("thread_id").is_none());
        assert!(json.get("run_id").is_none());
        assert!(json.get("forwarded_props").is_none());
    }
}
