//! MCP client-mode `PhaseDriver` implementation.
//!
//! `McpClientDriver` connects to an MCP server (via stdio or HTTP),
//! sends JSON-RPC requests (tool calls, resource reads, prompt gets),
//! and handles server-initiated requests (sampling, elicitation, roots)
//! via a background handler task.
//!
//! A multiplexer task continuously reads from the transport, routing
//! responses to oneshot channels for request correlation, server
//! requests to a bounded handler channel, and notifications to an
//! unbounded channel. This prevents deadlock when the server sends
//! sampling/elicitation requests while the driver awaits a response.
//!
//! See TJ-SPEC-018 for the full MCP client mode specification.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oatf::ResponseEntry;
use oatf::primitives::{interpolate_value, select_response};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    error_codes,
};

// ============================================================================
// Constants
// ============================================================================

/// Default per-request timeout.
///
/// Implements: TJ-SPEC-018 F-002
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default post-action event loop timeout.
const DEFAULT_PHASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Initialization handshake timeout.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Server request handler channel capacity.
///
/// Implements: TJ-SPEC-018 F-002
const SERVER_REQUEST_BUFFER_SIZE: usize = 64;

/// Capacity warning threshold (75% of buffer).
const SERVER_REQUEST_BUFFER_WARNING: usize = SERVER_REQUEST_BUFFER_SIZE / 4;

// ============================================================================
// Core Types
// ============================================================================

/// Classified message from the MCP transport reader.
///
/// Implements: TJ-SPEC-018 F-001
#[derive(Debug)]
enum McpClientMessage {
    /// JSON-RPC response correlated with its originating request.
    Response {
        /// Response ID.
        id: Value,
        /// Correlated request method.
        method: String,
        /// Result or error value.
        result: Value,
        /// Whether this is an error response.
        is_error: bool,
    },
    /// Server-to-client notification.
    Notification {
        /// Notification method.
        method: String,
        /// Notification params.
        params: Option<Value>,
    },
    /// Server-initiated request (sampling, elicitation, roots, ping).
    ServerRequest {
        /// Request ID (must respond).
        id: Value,
        /// Request method.
        method: String,
        /// Request params.
        params: Option<Value>,
    },
}

/// Tracks a pending outgoing request for response correlation.
///
/// Implements: TJ-SPEC-018 F-002
#[derive(Debug)]
struct PendingRequest {
    /// Original request method.
    method: String,
}

/// Correlated response returned via oneshot channel.
///
/// Implements: TJ-SPEC-018 F-002
#[derive(Debug)]
struct CorrelatedResponse {
    /// Correlated request method.
    method: String,
    /// Result value (from response.result or response.error).
    result: Value,
    /// Whether this is an error response.
    is_error: bool,
}

/// Notification routed by the multiplexer.
#[derive(Debug)]
struct NotificationMessage {
    /// Notification method.
    method: String,
    /// Notification params.
    params: Option<Value>,
}

/// Server-initiated request routed by the multiplexer to the handler.
#[derive(Debug)]
struct ServerRequestMessage {
    /// Request ID.
    id: Value,
    /// Request method.
    method: String,
    /// Request params.
    params: Option<Value>,
}

/// Reason why the multiplexer closed.
///
/// Implements: TJ-SPEC-018 F-011
#[derive(Debug, Clone)]
enum MultiplexerClosed {
    /// Server closed the connection normally.
    TransportEof,
    /// Transport-level failure.
    TransportError(String),
    /// Actor was cancelled.
    Cancelled,
}

impl std::fmt::Display for MultiplexerClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransportEof => write!(f, "transport EOF"),
            Self::TransportError(e) => write!(f, "transport error: {e}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Shared state published by the driver for the server request handler.
///
/// The handler reads phase state from here and fresh extractors from
/// its own `watch::Receiver`.
///
/// Implements: TJ-SPEC-018 F-003
#[derive(Debug)]
struct HandlerState {
    /// Current phase effective state.
    state: Value,
}

// ============================================================================
// Transport Traits
// ============================================================================

/// Writer half of the split MCP client transport.
///
/// Shared via `Arc<tokio::sync::Mutex>` between the driver and handler.
///
/// Implements: TJ-SPEC-018 F-001
#[async_trait]
trait McpClientTransportWriter: Send {
    /// Send a JSON-RPC request with a caller-provided ID.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on write failure.
    async fn send_request_with_id(
        &mut self,
        method: &str,
        params: Option<Value>,
        id: &Value,
    ) -> Result<(), EngineError>;

    /// Send a JSON-RPC response (to server-initiated requests).
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on write failure.
    async fn send_response(&mut self, id: &Value, result: Value) -> Result<(), EngineError>;

    /// Send a JSON-RPC error response.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on write failure.
    async fn send_error_response(
        &mut self,
        id: &Value,
        code: i64,
        message: &str,
    ) -> Result<(), EngineError>;

    /// Send a JSON-RPC notification (no id, no response expected).
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on write failure.
    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), EngineError>;

    /// Close the transport.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on close failure.
    #[allow(dead_code)]
    async fn close(&mut self) -> Result<(), EngineError>;
}

/// Reader half of the split MCP client transport.
///
/// Owned exclusively by the multiplexer — no lock contention.
///
/// Implements: TJ-SPEC-018 F-001
#[async_trait]
trait McpClientTransportReader: Send {
    /// Read the next incoming message, classified by type.
    ///
    /// Returns `None` on EOF.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on read/parse failure.
    async fn recv(
        &mut self,
        pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError>;
}

// ============================================================================
// Stdio Transport
// ============================================================================

/// Stdio writer: owns `ChildStdin`, serializes JSON-RPC messages.
///
/// Implements: TJ-SPEC-018 F-001
struct StdioWriter {
    /// Stdin of the spawned server process.
    stdin: ChildStdin,
}

#[async_trait]
impl McpClientTransportWriter for StdioWriter {
    async fn send_request_with_id(
        &mut self,
        method: &str,
        params: Option<Value>,
        id: &Value,
    ) -> Result<(), EngineError> {
        let request = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
            id: id.clone(),
        };
        let msg = JsonRpcMessage::Request(request);
        self.write_message(&msg).await
    }

    async fn send_response(&mut self, id: &Value, result: Value) -> Result<(), EngineError> {
        let response = JsonRpcResponse::success(id.clone(), result);
        let msg = JsonRpcMessage::Response(response);
        self.write_message(&msg).await
    }

    async fn send_error_response(
        &mut self,
        id: &Value,
        code: i64,
        message: &str,
    ) -> Result<(), EngineError> {
        let response = JsonRpcResponse::error(id.clone(), code, message);
        let msg = JsonRpcMessage::Response(response);
        self.write_message(&msg).await
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), EngineError> {
        let notification = JsonRpcNotification::new(method, params);
        let msg = JsonRpcMessage::Notification(notification);
        self.write_message(&msg).await
    }

    async fn close(&mut self) -> Result<(), EngineError> {
        self.stdin
            .shutdown()
            .await
            .map_err(|e| EngineError::Driver(format!("stdin close failed: {e}")))
    }
}

impl StdioWriter {
    /// Serialize and write a newline-delimited JSON-RPC message.
    async fn write_message(&mut self, msg: &JsonRpcMessage) -> Result<(), EngineError> {
        let mut bytes = serde_json::to_vec(msg)
            .map_err(|e| EngineError::Driver(format!("JSON serialization failed: {e}")))?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|e| EngineError::Driver(format!("stdin write failed: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| EngineError::Driver(format!("stdin flush failed: {e}")))
    }
}

/// Stdio reader: owns `BufReader<ChildStdout>`, classifies incoming messages.
///
/// Implements: TJ-SPEC-018 F-001
struct StdioReader {
    /// Buffered reader over the server process's stdout.
    stdout: BufReader<ChildStdout>,
}

#[async_trait]
impl McpClientTransportReader for StdioReader {
    async fn recv(
        &mut self,
        pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .await
            .map_err(|e| EngineError::Driver(format!("stdout read failed: {e}")))?;

        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Empty line — skip (EC-MCPC-010: lenient parsing)
            return self.recv(pending).await;
        }

        let msg: JsonRpcMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(e) => {
                // EC-MCPC-010: non-JSON lines are skipped with a warning
                tracing::warn!(error = %e, line = %trimmed.chars().take(200).collect::<String>(), "skipping non-JSON line from server");
                return self.recv(pending).await;
            }
        };

        Ok(Some(classify_message(msg, pending)))
    }
}

/// Classify a parsed `JsonRpcMessage` into `McpClientMessage`.
///
/// Uses the pending request map for response correlation.
fn classify_message(
    msg: JsonRpcMessage,
    pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
) -> McpClientMessage {
    match msg {
        JsonRpcMessage::Response(resp) => {
            let id_key = resp.id.to_string();
            let method = pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id_key)
                .map_or_else(|| "unknown".to_string(), |p| p.method);

            let is_error = resp.error.is_some();
            let result = if let Some(err) = resp.error {
                serde_json::to_value(&err).unwrap_or(Value::Null)
            } else {
                resp.result.unwrap_or(Value::Null)
            };

            McpClientMessage::Response {
                id: resp.id,
                method,
                result,
                is_error,
            }
        }
        JsonRpcMessage::Request(req) => {
            // Server-initiated request
            McpClientMessage::ServerRequest {
                id: req.id,
                method: req.method,
                params: req.params,
            }
        }
        JsonRpcMessage::Notification(notif) => McpClientMessage::Notification {
            method: notif.method,
            params: notif.params,
        },
    }
}

// ============================================================================
// Stdio Process Spawning
// ============================================================================

/// Spawn a server process and return the split transport halves + child.
///
/// # Errors
///
/// Returns `EngineError::Driver` if process spawn fails.
///
/// Implements: TJ-SPEC-018 F-001
fn spawn_stdio_transport(
    command: &str,
    args: &[String],
) -> Result<(StdioReader, StdioWriter, Child), EngineError> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            EngineError::Driver(format!(
                "failed to spawn MCP server process '{command}': {e}"
            ))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngineError::Driver("failed to capture server stdin".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::Driver("failed to capture server stdout".to_string()))?;

    let reader = StdioReader {
        stdout: BufReader::new(stdout),
    };
    let writer = StdioWriter { stdin };

    Ok((reader, writer, child))
}

// ============================================================================
// Message Multiplexer
// ============================================================================

/// Background task that reads from the transport and routes messages.
///
/// - Responses → oneshot channels (by ID)
/// - Server requests → bounded handler channel
/// - Notifications → unbounded channel
///
/// Implements: TJ-SPEC-018 F-002
struct MessageMultiplexer {
    /// Pending response senders: `id.to_string()` → oneshot sender.
    response_senders: Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<CorrelatedResponse>>>>,
    /// Why the multiplexer closed (set on loop exit).
    close_reason: Arc<std::sync::Mutex<Option<MultiplexerClosed>>>,
    /// Join handle for the background task.
    #[allow(dead_code)]
    handle: JoinHandle<()>,
}

impl MessageMultiplexer {
    /// Spawn the multiplexer background task.
    ///
    /// Takes exclusive ownership of the transport reader.
    ///
    /// Implements: TJ-SPEC-018 F-002
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        mut reader: Box<dyn McpClientTransportReader>,
        writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>>,
        pending: Arc<std::sync::Mutex<HashMap<String, PendingRequest>>>,
        server_request_tx: mpsc::Sender<ServerRequestMessage>,
        notification_tx: mpsc::UnboundedSender<NotificationMessage>,
        response_senders: Arc<
            std::sync::Mutex<HashMap<String, oneshot::Sender<CorrelatedResponse>>>,
        >,
        close_reason: Arc<std::sync::Mutex<Option<MultiplexerClosed>>>,
        cancel: CancellationToken,
    ) -> Self {
        let senders = Arc::clone(&response_senders);
        let reason = Arc::clone(&close_reason);

        let handle = tokio::spawn(async move {
            let exit_reason = loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        break MultiplexerClosed::Cancelled;
                    }
                    msg = reader.recv(&pending) => {
                        match msg {
                            Ok(Some(McpClientMessage::Response { id, method, result, is_error })) => {
                                let id_key = id.to_string();
                                let sender = senders
                                    .lock()
                                    .expect("response_senders lock poisoned")
                                    .remove(&id_key);

                                if let Some(tx) = sender {
                                    let _ = tx.send(CorrelatedResponse { method, result, is_error });
                                } else {
                                    // EC-MCPC-001: unmatched response ID
                                    tracing::warn!(id = %id, "received response for unknown request id");
                                }
                            }
                            Ok(Some(McpClientMessage::Notification { method, params })) => {
                                let _ = notification_tx.send(NotificationMessage { method, params });
                            }
                            Ok(Some(McpClientMessage::ServerRequest { id, method, params })) => {
                                // Backpressure check (§3.7)
                                if server_request_tx.capacity() < SERVER_REQUEST_BUFFER_WARNING {
                                    tracing::warn!(
                                        capacity = server_request_tx.capacity(),
                                        max = SERVER_REQUEST_BUFFER_SIZE,
                                        "server request buffer nearly full"
                                    );
                                }

                                let req = ServerRequestMessage {
                                    id: id.clone(),
                                    method: method.clone(),
                                    params,
                                };
                                if server_request_tx.try_send(req).is_err() {
                                    tracing::warn!(
                                        method = %method,
                                        id = %id,
                                        "server request buffer full, dropping request"
                                    );
                                    // Return error to server so it doesn't hang
                                    let _ = writer.lock().await
                                        .send_error_response(&id, -32000, "Client overwhelmed: server request buffer full")
                                        .await;
                                }
                            }
                            Ok(None) => {
                                break MultiplexerClosed::TransportEof;
                            }
                            Err(e) => {
                                break MultiplexerClosed::TransportError(e.to_string());
                            }
                        }
                    }
                }
            };

            // Store close reason BEFORE dropping senders
            *reason.lock().expect("close_reason lock poisoned") = Some(exit_reason);
            // Drop response_senders — all waiting receivers get RecvError
        });

        Self {
            response_senders,
            close_reason,
            handle,
        }
    }

    /// Register a oneshot channel for a response, keyed by request ID.
    ///
    /// Must be called BEFORE sending the request to prevent races.
    fn register_response(&self, id: &Value) -> oneshot::Receiver<CorrelatedResponse> {
        let (tx, rx) = oneshot::channel();
        self.response_senders
            .lock()
            .expect("response_senders lock poisoned")
            .insert(id.to_string(), tx);
        rx
    }

    /// Returns the reason the multiplexer closed, if it has.
    fn close_reason(&self) -> MultiplexerClosed {
        self.close_reason
            .lock()
            .expect("close_reason lock poisoned")
            .clone()
            .unwrap_or(MultiplexerClosed::TransportEof)
    }
}

// ============================================================================
// Server Request Handler
// ============================================================================

/// Background task that processes server-initiated requests.
///
/// Reads from the server request channel, builds responses from
/// the current `HandlerState` and fresh extractors from the watch
/// channel, and sends responses back via the shared writer.
///
/// Implements: TJ-SPEC-018 F-003
#[allow(clippy::too_many_arguments)]
async fn server_request_handler(
    mut server_request_rx: mpsc::Receiver<ServerRequestMessage>,
    writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>>,
    handler_state: Arc<tokio::sync::RwLock<HandlerState>>,
    extractors_rx: watch::Receiver<HashMap<String, String>>,
    handler_event_tx: mpsc::UnboundedSender<ProtocolEvent>,
    raw_synthesize: bool,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            msg = server_request_rx.recv() => {
                let Some(req) = msg else { break };

                let content = req.params.clone().unwrap_or(Value::Null);

                // Emit incoming event — PhaseLoop handles trace, extractors, triggers
                let _ = handler_event_tx.send(ProtocolEvent {
                    direction: Direction::Incoming,
                    method: req.method.clone(),
                    content: content.clone(),
                });

                // Build response from current phase state + fresh extractors
                let hs = handler_state.read().await;
                let current_extractors = extractors_rx.borrow().clone();
                let result = match req.method.as_str() {
                    "sampling/createMessage" => {
                        handle_sampling(&hs.state, &current_extractors, &content, raw_synthesize)
                    }
                    "elicitation/create" => {
                        Ok(handle_elicitation(&hs.state, &current_extractors, &content))
                    }
                    "roots/list" => Ok(handle_roots_list(&hs.state)),
                    "ping" => Ok(json!({})),
                    other => {
                        tracing::debug!(method = %other, "unknown server-initiated request, returning empty result");
                        Ok(json!({}))
                    }
                };
                drop(hs);

                match result {
                    Ok(result_value) => {
                        // Emit outgoing response event
                        let _ = handler_event_tx.send(ProtocolEvent {
                            direction: Direction::Outgoing,
                            method: req.method.clone(),
                            content: result_value.clone(),
                        });
                        let _ = writer.lock().await.send_response(&req.id, result_value).await;
                    }
                    Err(e) => {
                        tracing::warn!(method = %req.method, error = %e, "handler error, sending error response");
                        let _ = writer.lock().await
                            .send_error_response(&req.id, error_codes::INTERNAL_ERROR, &e.to_string())
                            .await;
                    }
                }
            }
        }
    }
}

/// Handle `sampling/createMessage` — match against `state.sampling_responses`.
///
/// Implements: TJ-SPEC-018 F-003
fn handle_sampling(
    state: &Value,
    extractors: &HashMap<String, String>,
    params: &Value,
    _raw_synthesize: bool,
) -> Result<Value, EngineError> {
    let Some(responses_value) = state.get("sampling_responses") else {
        // No sampling_responses defined — return minimal valid response
        return Ok(default_sampling_response());
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(error = %err, "failed to deserialize sampling_responses");
            return Ok(default_sampling_response());
        }
    };

    let Some(entry) = select_response(&entries, params) else {
        return Ok(default_sampling_response());
    };

    // Check for synthesize block
    if entry.synthesize.is_some() && entry.extra.is_empty() {
        // GenerationProvider not available yet — same stub as all other drivers
        tracing::info!(
            "sampling synthesize block encountered but GenerationProvider not available"
        );
        return Err(EngineError::Driver(
            "synthesize not yet supported — GenerationProvider not available".to_string(),
        ));
    }

    // Build response from extra fields with interpolation
    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, diagnostics) =
        interpolate_value(&extra_value, extractors, Some(params), None);

    for diag in &diagnostics {
        tracing::debug!(diagnostic = ?diag, "sampling interpolation diagnostic");
    }

    Ok(interpolated)
}

/// Handle `elicitation/create` — match against `state.elicitation_responses`.
///
/// Implements: TJ-SPEC-018 F-003
fn handle_elicitation(
    state: &Value,
    extractors: &HashMap<String, String>,
    params: &Value,
) -> Value {
    let Some(responses_value) = state.get("elicitation_responses") else {
        return json!({"action": "cancel"});
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(error = %err, "failed to deserialize elicitation_responses");
            return json!({"action": "cancel"});
        }
    };

    let Some(entry) = select_response(&entries, params) else {
        return json!({"action": "cancel"});
    };

    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, diagnostics) =
        interpolate_value(&extra_value, extractors, Some(params), None);

    for diag in &diagnostics {
        tracing::debug!(diagnostic = ?diag, "elicitation interpolation diagnostic");
    }

    interpolated
}

/// Handle `roots/list` — return configured roots.
///
/// Implements: TJ-SPEC-018 F-003
fn handle_roots_list(state: &Value) -> Value {
    state
        .get("roots")
        .map_or_else(|| json!({"roots": []}), |roots| json!({"roots": roots}))
}

/// Default sampling response when no matching entry is found.
fn default_sampling_response() -> Value {
    json!({
        "role": "assistant",
        "content": {"type": "text", "text": ""},
        "model": "default",
        "stopReason": "endTurn"
    })
}

// ============================================================================
// Action Normalization
// ============================================================================

/// Normalize heterogeneous OATF action YAML to uniform `{"type": ..., ...params}`.
///
/// OATF deserializes actions as:
/// - Bare string: `"list_tools"` → `Value::String`
/// - Single-key object: `{"call_tool": {"name": "foo", ...}}` → `Value::Object`
///
/// This normalizes both to `{"type": "...", ...params}`.
///
/// Implements: TJ-SPEC-018 F-006
fn normalize_action(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            // Bare string → {"type": "<string>"}
            json!({"type": s})
        }
        Value::Object(map) if map.len() == 1 && !map.contains_key("type") => {
            // Single-key object → {"type": "<key>", ...value_fields}
            let (key, val) = map.iter().next().expect("single-key object");
            let mut normalized = json!({"type": key});
            if let Value::Object(inner) = val {
                for (k, v) in inner {
                    normalized[k] = v.clone();
                }
            }
            normalized
        }
        other => {
            // Already has "type" or unexpected structure — pass through
            other.clone()
        }
    }
}

// ============================================================================
// McpClientDriver
// ============================================================================

/// MCP client-mode protocol driver.
///
/// Connects to an MCP server, sends JSON-RPC requests, handles
/// server-initiated requests via a background handler task, and
/// emits protocol events for the `PhaseLoop`.
///
/// Implements: TJ-SPEC-018 F-004
pub struct McpClientDriver {
    /// Shared writer (driver + handler both write).
    writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>>,
    /// Pending request map for response correlation.
    pending: Arc<std::sync::Mutex<HashMap<String, PendingRequest>>>,
    /// Multiplexer (spawned on first `drive_phase()`).
    mux: Option<MessageMultiplexer>,
    /// Notification receiver from multiplexer.
    notification_rx: Option<mpsc::UnboundedReceiver<NotificationMessage>>,
    /// Handler event receiver (handler emits events here, driver forwards to `PhaseLoop`).
    handler_event_rx: Option<mpsc::UnboundedReceiver<ProtocolEvent>>,
    /// Shared handler state.
    handler_state: Arc<tokio::sync::RwLock<HandlerState>>,
    /// Handler task join handle.
    handler_handle: Option<JoinHandle<()>>,
    /// Server capabilities (captured during init).
    server_capabilities: Option<Value>,
    /// Per-request timeout.
    request_timeout: Duration,
    /// Post-action event loop timeout.
    phase_timeout: Duration,
    /// Whether initialization has completed.
    initialized: bool,
    /// Next request ID counter.
    next_request_id: u64,
    /// Bypass synthesize output validation.
    raw_synthesize: bool,
    /// Transport reader (consumed on first `drive_phase()`).
    reader: Option<Box<dyn McpClientTransportReader>>,
    /// Cancellation token for background tasks.
    transport_cancel: CancellationToken,
    /// Spawned child process (for stdio transport).
    #[allow(dead_code)]
    child: Option<Child>,
}

impl McpClientDriver {
    /// Generate the next monotonically increasing request ID.
    const fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Send a JSON-RPC request and await its correlated response.
    ///
    /// Registers the oneshot channel BEFORE sending to prevent races.
    /// Server-initiated requests are handled concurrently by the
    /// multiplexer + handler while this method awaits.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on timeout or multiplexer close.
    async fn send_and_await(
        &mut self,
        method: &str,
        params: Option<Value>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<CorrelatedResponse, EngineError> {
        let id = json!(self.next_id());
        let id_key = id.to_string();

        // Register pending request for correlation
        self.pending.lock().expect("pending lock poisoned").insert(
            id_key,
            PendingRequest {
                method: method.to_string(),
            },
        );

        // Register response channel BEFORE sending (prevents race)
        let mux = self
            .mux
            .as_ref()
            .ok_or_else(|| EngineError::Driver("multiplexer not started".to_string()))?;
        let response_rx = mux.register_response(&id);

        // Send request
        self.writer
            .lock()
            .await
            .send_request_with_id(method, params.clone(), &id)
            .await?;

        // Emit outgoing event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: method.to_string(),
            content: params.unwrap_or(Value::Null),
        });

        // Await response via oneshot — multiplexer handles concurrent server requests
        let response = tokio::time::timeout(self.request_timeout, response_rx)
            .await
            .map_err(|_| {
                EngineError::Driver(format!(
                    "request timeout for '{method}' after {:?}",
                    self.request_timeout
                ))
            })?
            .map_err(|_| {
                let reason = mux.close_reason();
                EngineError::Driver(format!(
                    "multiplexer closed while awaiting '{method}': {reason}"
                ))
            })?;

        // Emit incoming event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: response.method.clone(),
            content: response.result.clone(),
        });

        Ok(response)
    }

    /// Forward any buffered events from handler and notifications to the `PhaseLoop`.
    ///
    /// Called between actions to minimize event forwarding latency.
    fn forward_pending_events(&mut self, event_tx: &mpsc::UnboundedSender<ProtocolEvent>) {
        if let Some(ref mut rx) = self.handler_event_rx {
            while let Ok(evt) = rx.try_recv() {
                let _ = event_tx.send(evt);
            }
        }
        if let Some(ref mut rx) = self.notification_rx {
            while let Ok(notif) = rx.try_recv() {
                let _ = event_tx.send(ProtocolEvent {
                    direction: Direction::Incoming,
                    method: notif.method,
                    content: notif.params.unwrap_or(Value::Null),
                });
            }
        }
    }

    /// Perform the MCP initialization handshake.
    ///
    /// Sends `initialize` request, captures server capabilities,
    /// sends `notifications/initialized` notification.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` if initialization fails.
    ///
    /// Implements: TJ-SPEC-018 F-005
    async fn initialize(
        &mut self,
        state: &Value,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<(), EngineError> {
        let init_params = json!({
            "protocolVersion": "2025-11-25",
            "capabilities": build_client_capabilities(state),
            "clientInfo": {
                "name": "ThoughtJack",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let id = json!(self.next_id());
        let id_key = id.to_string();

        // Register pending request
        self.pending.lock().expect("pending lock poisoned").insert(
            id_key,
            PendingRequest {
                method: "initialize".to_string(),
            },
        );

        // Register response channel BEFORE sending
        let mux = self
            .mux
            .as_ref()
            .ok_or_else(|| EngineError::Driver("multiplexer not started".to_string()))?;
        let response_rx = mux.register_response(&id);

        // Send initialize request
        self.writer
            .lock()
            .await
            .send_request_with_id("initialize", Some(init_params.clone()), &id)
            .await?;

        // Emit outgoing event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "initialize".to_string(),
            content: init_params,
        });

        // Await response
        let response = tokio::time::timeout(INIT_TIMEOUT, response_rx)
            .await
            .map_err(|_| EngineError::Driver("initialization timeout".to_string()))?
            .map_err(|_| {
                let reason = mux.close_reason();
                EngineError::Driver(format!(
                    "multiplexer closed during initialization: {reason}"
                ))
            })?;

        // Check for error response (EC-MCPC-005)
        if response.is_error {
            return Err(EngineError::Driver(format!(
                "server rejected initialization: {}",
                response.result
            )));
        }

        // Capture server capabilities
        self.server_capabilities = Some(response.result.clone());

        // Emit incoming event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: "initialize".to_string(),
            content: response.result,
        });

        // Send initialized notification
        self.writer
            .lock()
            .await
            .send_notification("notifications/initialized", None)
            .await?;

        self.initialized = true;
        tracing::info!("MCP client initialization complete");

        Ok(())
    }

    /// Execute a single normalized action.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on request/response failure.
    ///
    /// Implements: TJ-SPEC-018 F-006
    async fn execute_action(
        &mut self,
        action: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<(), EngineError> {
        let action_type = action["type"].as_str().unwrap_or("");

        match action_type {
            "list_tools" => {
                self.send_and_await("tools/list", None, event_tx).await?;
            }
            "call_tool" => {
                let name = action["name"].as_str().unwrap_or_default();
                let arguments = action.get("arguments").cloned().unwrap_or(json!({}));
                let (interpolated_args, _) = interpolate_value(&arguments, extractors, None, None);
                let params = json!({"name": name, "arguments": interpolated_args});
                self.send_and_await("tools/call", Some(params), event_tx)
                    .await?;
            }
            "list_resources" => {
                self.send_and_await("resources/list", None, event_tx)
                    .await?;
            }
            "read_resource" => {
                let uri = action["uri"].as_str().unwrap_or_default();
                let params = json!({"uri": uri});
                self.send_and_await("resources/read", Some(params), event_tx)
                    .await?;
            }
            "list_prompts" => {
                self.send_and_await("prompts/list", None, event_tx).await?;
            }
            "get_prompt" => {
                let name = action["name"].as_str().unwrap_or_default();
                let arguments = action.get("arguments").cloned().unwrap_or(json!({}));
                let (interpolated_args, _) = interpolate_value(&arguments, extractors, None, None);
                let params = json!({"name": name, "arguments": interpolated_args});
                self.send_and_await("prompts/get", Some(params), event_tx)
                    .await?;
            }
            "subscribe_resource" => {
                let uri = action["uri"].as_str().unwrap_or_default();
                let params = json!({"uri": uri});
                self.send_and_await("resources/subscribe", Some(params), event_tx)
                    .await?;
            }
            unknown => {
                tracing::warn!(action_type = %unknown, "unknown MCP client action type, skipping");
            }
        }

        Ok(())
    }

    /// Bootstrap the multiplexer and handler on first `drive_phase()` call.
    fn bootstrap(&mut self, extractors: watch::Receiver<HashMap<String, String>>) {
        let reader = self
            .reader
            .take()
            .expect("reader should be available on first drive_phase");

        // Create channels
        let (server_request_tx, server_request_rx) = mpsc::channel(SERVER_REQUEST_BUFFER_SIZE);
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let (handler_event_tx, handler_event_rx) = mpsc::unbounded_channel();

        // Create multiplexer shared state
        let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let close_reason = Arc::new(std::sync::Mutex::new(None));

        // Spawn multiplexer
        let mux = MessageMultiplexer::spawn(
            reader,
            Arc::clone(&self.writer),
            Arc::clone(&self.pending),
            server_request_tx,
            notification_tx,
            response_senders,
            close_reason,
            self.transport_cancel.clone(),
        );

        // Spawn handler
        let handler_handle = tokio::spawn(server_request_handler(
            server_request_rx,
            Arc::clone(&self.writer),
            Arc::clone(&self.handler_state),
            extractors, // Ownership transfer — handler holds this for the driver's lifetime
            handler_event_tx,
            self.raw_synthesize,
            self.transport_cancel.clone(),
        ));

        self.mux = Some(mux);
        self.notification_rx = Some(notification_rx);
        self.handler_event_rx = Some(handler_event_rx);
        self.handler_handle = Some(handler_handle);
    }
}

// ============================================================================
// PhaseDriver Implementation
// ============================================================================

#[async_trait]
impl PhaseDriver for McpClientDriver {
    /// Execute the MCP client protocol work for a single phase.
    ///
    /// On the first call, bootstraps the multiplexer and handler.
    /// Performs initialization handshake if not yet done.
    /// Executes phase actions in order, forwarding handler events between each.
    /// After actions, enters an event loop forwarding handler/notification events
    /// until cancel or phase timeout.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on protocol-level failures.
    ///
    /// Implements: TJ-SPEC-018 F-004
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        // Bootstrap on first call: spawn multiplexer and handler
        if self.mux.is_none() {
            self.bootstrap(extractors.clone());
        }

        // Initialize on first call
        if !self.initialized {
            self.initialize(state, &event_tx).await?;
        }

        // Update handler state for this phase
        {
            let mut hs = self.handler_state.write().await;
            hs.state = state.clone();
        }

        // Clone extractors for action interpolation
        let current_extractors = extractors.borrow().clone();

        // Execute actions defined in the phase state
        if let Some(actions) = state.get("actions").and_then(Value::as_array) {
            for action_value in actions {
                // Forward any buffered handler events before each action
                self.forward_pending_events(&event_tx);

                // Normalize and execute action
                let normalized = normalize_action(action_value);
                self.execute_action(&normalized, &current_extractors, &event_tx)
                    .await?;
            }
        }

        // Post-action event loop: forward handler and notification events
        // until cancel fires or phase_timeout expires. PhaseLoop checks triggers
        // on each forwarded event and will cancel if a trigger fires.
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    break;
                }
                evt = async {
                    if let Some(ref mut rx) = self.handler_event_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(evt) = evt {
                        let _ = event_tx.send(evt);
                    } else {
                        break;
                    }
                }
                notif = async {
                    if let Some(ref mut rx) = self.notification_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(n) = notif {
                        let _ = event_tx.send(ProtocolEvent {
                            direction: Direction::Incoming,
                            method: n.method,
                            content: n.params.unwrap_or(Value::Null),
                        });
                    } else {
                        break;
                    }
                }
                () = tokio::time::sleep(self.phase_timeout) => {
                    break;
                }
            }
        }

        Ok(DriveResult::Complete)
    }

    async fn on_phase_advanced(&mut self, _from: usize, _to: usize) -> Result<(), EngineError> {
        // No-op: handler state updated at start of next drive_phase,
        // extractors come from watch channel (always fresh).
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Build client capabilities from the phase state.
///
/// Advertises sampling, elicitation, and roots support based on
/// whether the state defines the corresponding response fields.
///
/// Implements: TJ-SPEC-018 F-005
fn build_client_capabilities(state: &Value) -> Value {
    let mut caps = json!({});

    if state.get("sampling_responses").is_some() {
        caps["sampling"] = json!({});
    }
    if state.get("roots").is_some() {
        caps["roots"] = json!({"listChanged": false});
    }
    if state.get("elicitation_responses").is_some() {
        caps["elicitation"] = json!({});
    }

    caps
}

// ============================================================================
// Factory Function
// ============================================================================

/// Creates an `McpClientDriver` for stdio transport.
///
/// Spawns the server process and sets up split transport.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the server process cannot be spawned.
///
/// Implements: TJ-SPEC-018 F-001
pub fn create_mcp_client_driver(
    command: &str,
    args: &[String],
    _endpoint: Option<&str>,
    raw_synthesize: bool,
) -> Result<McpClientDriver, EngineError> {
    // TODO: HTTP transport support (Streamable HTTP) — use endpoint when provided
    let (reader, writer, child) = spawn_stdio_transport(command, args)?;

    let transport_cancel = CancellationToken::new();

    Ok(McpClientDriver {
        writer: Arc::new(tokio::sync::Mutex::new(Box::new(writer))),
        pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
        mux: None,
        notification_rx: None,
        handler_event_rx: None,
        handler_state: Arc::new(tokio::sync::RwLock::new(HandlerState {
            state: Value::Null,
        })),
        handler_handle: None,
        server_capabilities: None,
        request_timeout: DEFAULT_REQUEST_TIMEOUT,
        phase_timeout: DEFAULT_PHASE_TIMEOUT,
        initialized: false,
        next_request_id: 1,
        raw_synthesize,
        reader: Some(Box::new(reader)),
        transport_cancel,
        child: Some(child),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
            },
        );

        let msg =
            JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!({"tools": []})));
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
            },
        );

        let msg =
            JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!({"tools": []})));
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

    // ---- Multiplexer Tests ----

    #[tokio::test]
    async fn multiplexer_routes_response_to_oneshot() {
        let (mock_writer, _sent) = MockWriter::new();
        let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
            Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
        let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (server_request_tx, _server_request_rx) = mpsc::channel(64);
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
        let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let close_reason = Arc::new(std::sync::Mutex::new(None));
        let cancel = CancellationToken::new();

        // Pre-register a pending request so the mock reader produces a Response
        let reader = MockReader::new(vec![McpClientMessage::Response {
            id: json!(1),
            method: "tools/list".to_string(),
            result: json!({"tools": ["calc"]}),
            is_error: false,
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

        // Give multiplexer time to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = rx.await.unwrap();
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
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
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
        let (notification_tx, mut notification_rx) = mpsc::unbounded_channel();
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
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
        let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let close_reason = Arc::new(std::sync::Mutex::new(None));
        let cancel = CancellationToken::new();

        // Response with id=999 but no oneshot registered for it
        let reader = MockReader::new(vec![McpClientMessage::Response {
            id: json!(999),
            method: "unknown".to_string(),
            result: json!({}),
            is_error: false,
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

        // Allow time for processing — should not panic
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    }

    #[tokio::test]
    async fn multiplexer_eof_sets_close_reason() {
        let (mock_writer, _sent) = MockWriter::new();
        let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
            Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
        let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (server_request_tx, _server_request_rx) = mpsc::channel(64);
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
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

        // Wait for multiplexer to process EOF
        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (notification_tx, _notification_rx) = mpsc::unbounded_channel();
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

        // Cancel and wait for task to process
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, mut handler_event_rx) = mpsc::unbounded_channel();
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

        // Wait for handler to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check events emitted (incoming + outgoing)
        let evt1 = handler_event_rx.try_recv().unwrap();
        assert_eq!(evt1.method, "sampling/createMessage");
        assert!(matches!(evt1.direction, Direction::Incoming));

        let evt2 = handler_event_rx.try_recv().unwrap();
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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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

        tokio::time::sleep(Duration::from_millis(50)).await;

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
        let (handler_event_tx, _handler_event_rx) = mpsc::unbounded_channel();
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
    async fn driver_forward_pending_events_drains_channels() {
        let (mock_writer, _sent) = MockWriter::new();
        let writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>> =
            Arc::new(tokio::sync::Mutex::new(Box::new(mock_writer)));
        let reader = Box::new(MockReader::new(vec![]));
        let mut driver = create_test_driver(writer, reader);

        // Set up channels manually
        let (handler_tx, handler_rx) = mpsc::unbounded_channel();
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        driver.handler_event_rx = Some(handler_rx);
        driver.notification_rx = Some(notif_rx);

        // Push events
        handler_tx
            .send(ProtocolEvent {
                direction: Direction::Incoming,
                method: "sampling/createMessage".to_string(),
                content: json!({}),
            })
            .unwrap();
        notif_tx
            .send(NotificationMessage {
                method: "notifications/progress".to_string(),
                params: Some(json!({"progress": 50})),
            })
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
}
