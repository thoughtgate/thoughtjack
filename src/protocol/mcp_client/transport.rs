use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use crate::error::EngineError;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};

use super::{HTTP_MESSAGE_BUFFER_SIZE, McpClientMessage, PendingRequest};

// ============================================================================
// Transport Traits
// ============================================================================

/// Writer half of the split MCP client transport.
///
/// Shared via `Arc<tokio::sync::Mutex>` between the driver and handler.
///
/// Implements: TJ-SPEC-018 F-001
#[async_trait]
pub(super) trait McpClientTransportWriter: Send {
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
    // Future: graceful close handshake
    #[allow(dead_code)]
    async fn close(&mut self) -> Result<(), EngineError>;
}

/// Reader half of the split MCP client transport.
///
/// Owned exclusively by the multiplexer — no lock contention.
///
/// Implements: TJ-SPEC-018 F-001
#[async_trait]
pub(super) trait McpClientTransportReader: Send {
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
pub(super) struct StdioWriter {
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
/// Uses bounded line reading (via `fill_buf`/`consume`) to prevent OOM from
/// a single line without `\n`. Lines exceeding the size limit are skipped.
///
/// Implements: TJ-SPEC-018 F-001
pub(super) struct StdioReader {
    /// Buffered reader over the server process's stdout.
    stdout: BufReader<ChildStdout>,
}

/// Maximum NDJSON line size for MCP client stdio (10 MB, matching transport default).
const MAX_LINE_SIZE: usize = crate::transport::DEFAULT_MAX_MESSAGE_SIZE;

#[async_trait]
impl McpClientTransportReader for StdioReader {
    async fn recv(
        &mut self,
        pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        loop {
            let line = match read_bounded_line(&mut self.stdout).await {
                Ok(Some(line)) => line,
                Ok(None) => return Ok(None), // EOF
                Err(e) => return Err(EngineError::Driver(format!("stdout read failed: {e}"))),
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                // Empty line — skip (EC-MCPC-010: lenient parsing)
                continue;
            }

            let msg: JsonRpcMessage = match serde_json::from_str(trimmed) {
                Ok(m) => m,
                Err(e) => {
                    // EC-MCPC-010: non-JSON lines are skipped with a warning
                    tracing::warn!(error = %e, line = %trimmed.chars().take(200).collect::<String>(), "skipping non-JSON line from server");
                    continue;
                }
            };

            return Ok(Some(classify_message(msg, pending)));
        }
    }
}

/// Reads a single NDJSON line with bounded memory.
///
/// Uses `fill_buf()`/`consume()` to cap memory usage at `MAX_LINE_SIZE + 1`
/// bytes. Lines exceeding the limit are drained and skipped (returns the
/// next valid line). Returns `Ok(None)` on EOF.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    let read_limit = MAX_LINE_SIZE + 1;
    let mut buf: Vec<u8> = Vec::with_capacity(read_limit.min(64 * 1024));

    loop {
        buf.clear();
        let mut overflowed = false;

        // Bounded line read using fill_buf + consume
        loop {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                // EOF
                if buf.is_empty() {
                    return Ok(None);
                }
                // Last line without trailing '\n'
                break;
            }

            if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                if !overflowed {
                    let remaining_cap = read_limit.saturating_sub(buf.len());
                    let copy_len = pos.min(remaining_cap);
                    buf.extend_from_slice(&available[..copy_len]);
                    if pos > remaining_cap {
                        overflowed = true;
                    }
                }
                reader.consume(pos + 1);
                break;
            }

            // No newline in this chunk — append if within limit
            if !overflowed {
                let remaining_cap = read_limit.saturating_sub(buf.len());
                if remaining_cap == 0 {
                    overflowed = true;
                } else {
                    let copy_len = available.len().min(remaining_cap);
                    buf.extend_from_slice(&available[..copy_len]);
                    if available.len() > remaining_cap {
                        overflowed = true;
                    }
                }
            }
            let consumed = available.len();
            reader.consume(consumed);
        }

        if overflowed {
            tracing::warn!(
                limit = MAX_LINE_SIZE,
                "MCP client: message exceeds size limit, skipping"
            );
            continue;
        }

        match std::str::from_utf8(&buf) {
            Ok(s) => return Ok(Some(s.to_string())),
            Err(e) => {
                tracing::warn!("MCP client: invalid UTF-8 in message, skipping: {e}");
            }
        }
    }
}

/// Classify a parsed `JsonRpcMessage` into `McpClientMessage`.
///
/// Uses the pending request map for response correlation. Extracts
/// the original request params so that qualifier resolution on
/// response events can access the request context.
pub(super) fn classify_message(
    msg: JsonRpcMessage,
    pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
) -> McpClientMessage {
    match msg {
        JsonRpcMessage::Response(resp) => {
            let id_key = resp.id.to_string();
            let pending_req = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id_key);
            let (method, request_params) =
                pending_req.map_or_else(|| ("unknown".to_string(), None), |p| (p.method, p.params));

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
                request_params,
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
pub(super) fn spawn_stdio_transport(
    command: &str,
    args: &[String],
) -> Result<(StdioReader, StdioWriter, Child), EngineError> {
    let mut cmd = Command::new(command);
    // Ensure child processes do not outlive the driver if dropped unexpectedly.
    cmd.kill_on_drop(true);
    let mut child = cmd
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
// HTTP (Streamable HTTP) Transport
// ============================================================================

/// HTTP writer for MCP Streamable HTTP transport.
///
/// POSTs JSON-RPC messages to the server endpoint. Responses (JSON or SSE)
/// are parsed and pushed to an internal channel consumed by `HttpReader`.
///
/// Implements: TJ-SPEC-018 F-001
pub(super) struct HttpWriter {
    /// HTTP client.
    client: reqwest::Client,
    /// Server endpoint URL.
    endpoint: String,
    /// Session ID captured from `Mcp-Session-Id` response header.
    session_id: Arc<Mutex<Option<String>>>,
    /// Extra HTTP headers from CLI `--header` flags.
    headers: Vec<(String, String)>,
    /// Channel sender — parsed response messages pushed here for the reader.
    message_tx: mpsc::Sender<JsonRpcMessage>,
    /// Background response collectors spawned for in-flight HTTP responses.
    response_tasks: Arc<StdMutex<Vec<JoinHandle<()>>>>,
}

/// HTTP reader for MCP Streamable HTTP transport.
///
/// Pops `JsonRpcMessage` values from the channel fed by `HttpWriter`.
///
/// Implements: TJ-SPEC-018 F-001
pub(super) struct HttpReader {
    /// Channel receiver — messages arrive from `HttpWriter` after each POST.
    message_rx: mpsc::Receiver<JsonRpcMessage>,
}

#[async_trait]
impl McpClientTransportWriter for HttpWriter {
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
        self.post_and_collect(&msg).await
    }

    async fn send_response(&mut self, id: &Value, result: Value) -> Result<(), EngineError> {
        let response = JsonRpcResponse::success(id.clone(), result);
        let msg = JsonRpcMessage::Response(response);
        self.post_and_collect(&msg).await
    }

    async fn send_error_response(
        &mut self,
        id: &Value,
        code: i64,
        message: &str,
    ) -> Result<(), EngineError> {
        let response = JsonRpcResponse::error(id.clone(), code, message);
        let msg = JsonRpcMessage::Response(response);
        self.post_and_collect(&msg).await
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), EngineError> {
        let notification = JsonRpcNotification::new(method, params);
        let msg = JsonRpcMessage::Notification(notification);
        // Notifications don't expect a response body, but send anyway
        self.post_and_collect(&msg).await
    }

    async fn close(&mut self) -> Result<(), EngineError> {
        // HTTP is stateless — no-op
        Ok(())
    }
}

/// Maximum number of retries for HTTP connection errors.
const HTTP_MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (doubled each retry).
const HTTP_RETRY_BASE_MS: u64 = 250;

impl HttpWriter {
    /// POST a JSON-RPC message to the endpoint and collect any response messages.
    ///
    /// Retries connection-level errors (refused, timeout) with exponential
    /// backoff per TJ-SPEC-018 §10.1. HTTP error status codes and body
    /// parsing errors are NOT retried.
    ///
    /// Parses the HTTP response body based on Content-Type:
    /// - `application/json` → single JSON-RPC message
    /// - `text/event-stream` → SSE stream, each `data:` line is a JSON-RPC message
    ///
    /// Captures `Mcp-Session-Id` from response headers.
    async fn post_and_collect(&self, msg: &JsonRpcMessage) -> Result<(), EngineError> {
        let body = serde_json::to_vec(msg)
            .map_err(|e| EngineError::Driver(format!("JSON serialization failed: {e}")))?;

        let response = self.send_with_retry(&body).await?;

        // Capture session ID from response
        if let Some(sid) = response.headers().get("mcp-session-id")
            && let Ok(sid_str) = sid.to_str()
        {
            *self.session_id.lock().await = Some(sid_str.to_string());
        }

        let status = response.status();
        if !status.is_success() {
            // Read error body for diagnostics
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(EngineError::Driver(format!(
                "HTTP {status} from {}: {error_body}",
                self.endpoint
            )));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        self.spawn_response_collector(response, content_type);

        Ok(())
    }

    fn spawn_response_collector(&self, response: reqwest::Response, content_type: String) {
        let message_tx = self.message_tx.clone();
        let handle = tokio::spawn(async move {
            let result = if content_type.contains("text/event-stream") {
                Self::collect_sse_response(message_tx, response).await
            } else {
                Self::collect_json_response(message_tx, response).await
            };

            if let Err(err) = result {
                tracing::warn!("failed to collect MCP HTTP response body: {err}");
            }
        });

        self.response_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(handle);
    }

    /// Send an HTTP POST with retry on connection-level errors.
    ///
    /// Retries up to `HTTP_MAX_RETRIES` times with exponential backoff
    /// for connection refused and timeout errors. Other errors (e.g.,
    /// TLS, invalid URL) fail immediately.
    async fn send_with_retry(&self, body: &[u8]) -> Result<reqwest::Response, EngineError> {
        let mut last_err = None;
        for attempt in 0..=HTTP_MAX_RETRIES {
            let mut request = self
                .client
                .post(&self.endpoint)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream");

            if let Some(ref sid) = *self.session_id.lock().await {
                request = request.header("Mcp-Session-Id", sid.as_str());
            }

            for (key, value) in &self.headers {
                request = request.header(key.as_str(), value.as_str());
            }

            match request.body(body.to_vec()).send().await {
                Ok(resp) => return Ok(resp),
                Err(e) if e.is_connect() || e.is_timeout() => {
                    if attempt < HTTP_MAX_RETRIES {
                        let delay_ms = HTTP_RETRY_BASE_MS * 2u64.pow(attempt);
                        tracing::warn!(
                            attempt = attempt + 1,
                            max_retries = HTTP_MAX_RETRIES,
                            delay_ms,
                            error = %e,
                            "HTTP connection failed, retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    last_err = Some(e);
                }
                Err(e) => {
                    return Err(EngineError::Driver(format!(
                        "HTTP POST to {} failed: {e}",
                        self.endpoint
                    )));
                }
            }
        }

        Err(EngineError::Driver(format!(
            "HTTP POST to {} failed after {} attempts: {}",
            self.endpoint,
            HTTP_MAX_RETRIES + 1,
            last_err.expect("last_err set on retry exhaustion")
        )))
    }

    /// Parse a single JSON response body as a `JsonRpcMessage` and push to channel.
    async fn collect_json_response(
        message_tx: mpsc::Sender<JsonRpcMessage>,
        response: reqwest::Response,
    ) -> Result<(), EngineError> {
        let text = response
            .text()
            .await
            .map_err(|e| EngineError::Driver(format!("failed to read HTTP response body: {e}")))?;

        let trimmed = text.trim();
        if trimmed.is_empty() {
            // No response body (e.g. for notifications) — not an error
            return Ok(());
        }

        let msg: JsonRpcMessage = serde_json::from_str(trimmed)
            .map_err(|e| EngineError::Driver(format!("failed to parse JSON-RPC response: {e}")))?;

        // Ignore send failure — reader may have been dropped (shutdown)
        let _ = message_tx.send(msg).await;

        Ok(())
    }

    /// Parse an SSE response body, extracting `data:` lines as `JsonRpcMessage` values.
    async fn collect_sse_response(
        message_tx: mpsc::Sender<JsonRpcMessage>,
        response: reqwest::Response,
    ) -> Result<(), EngineError> {
        use futures_util::StreamExt;

        let mut stream = response.bytes_stream();
        let mut parser = McpSseParser::new();

        while let Some(chunk) = stream.next().await {
            let bytes =
                chunk.map_err(|e| EngineError::Driver(format!("SSE stream read error: {e}")))?;

            for result in parser.feed(&bytes) {
                match result {
                    Ok(msg) => {
                        let _ = message_tx.send(msg).await;
                    }
                    Err(e) => {
                        tracing::warn!("skipping malformed SSE data in MCP response: {e}");
                    }
                }
            }
        }

        for result in parser.finish() {
            match result {
                Ok(msg) => {
                    let _ = message_tx.send(msg).await;
                }
                Err(e) => {
                    tracing::warn!("skipping malformed SSE data in MCP response: {e}");
                }
            }
        }

        Ok(())
    }
}

impl Drop for HttpWriter {
    fn drop(&mut self) {
        let mut tasks = self
            .response_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for handle in tasks.drain(..) {
            handle.abort();
        }
    }
}

#[async_trait]
impl McpClientTransportReader for HttpReader {
    async fn recv(
        &mut self,
        pending: &std::sync::Mutex<HashMap<String, PendingRequest>>,
    ) -> Result<Option<McpClientMessage>, EngineError> {
        self.message_rx
            .recv()
            .await
            .map_or_else(|| Ok(None), |msg| Ok(Some(classify_message(msg, pending))))
    }
}

/// Creates an HTTP transport pair for MCP Streamable HTTP.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the HTTP client cannot be built.
///
/// Implements: TJ-SPEC-018 F-001
pub(super) fn create_http_transport(
    endpoint: &str,
    headers: &[(String, String)],
) -> Result<(HttpReader, HttpWriter), EngineError> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| EngineError::Driver(format!("failed to build HTTP client: {e}")))?;

    let (message_tx, message_rx) = mpsc::channel(HTTP_MESSAGE_BUFFER_SIZE);
    let session_id = Arc::new(Mutex::new(None));
    let response_tasks = Arc::new(StdMutex::new(Vec::new()));

    let writer = HttpWriter {
        client,
        endpoint: endpoint.to_string(),
        session_id,
        headers: headers.to_vec(),
        message_tx,
        response_tasks,
    };

    let reader = HttpReader { message_rx };

    Ok((reader, writer))
}

// ============================================================================
// MCP SSE Parser
// ============================================================================

/// MCP SSE parser wrapping the shared `transport::SseParser`.
///
/// Parses `data:` lines as complete JSON-RPC messages. Inherits buffer
/// overflow protection from the shared parser.
///
/// Implements: TJ-SPEC-018 F-001
struct McpSseParser {
    /// Shared SSE parser with buffer limits.
    inner: crate::transport::sse::SseParser,
}

impl McpSseParser {
    /// Creates a new SSE parser.
    const fn new() -> Self {
        Self {
            inner: crate::transport::sse::SseParser::new(),
        }
    }

    /// Feed raw bytes and extract any complete JSON-RPC messages.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<JsonRpcMessage, String>> {
        let raw_events = self.inner.feed(bytes);
        self.map_raw_events(raw_events)
    }

    fn finish(&mut self) -> Vec<Result<JsonRpcMessage, String>> {
        let raw_events = self.inner.finish();
        self.map_raw_events(raw_events)
    }

    #[allow(clippy::unused_self)]
    fn map_raw_events(
        &self,
        raw_events: Vec<
            Result<crate::transport::sse::RawSseEvent, crate::transport::sse::SseParseError>,
        >,
    ) -> Vec<Result<JsonRpcMessage, String>> {
        let mut messages = Vec::new();

        for raw in raw_events {
            match raw {
                Err(e) => {
                    messages.push(Err(format!("MCP SSE parse error: {e}")));
                }
                Ok(raw_event) => match serde_json::from_str::<JsonRpcMessage>(&raw_event.data) {
                    Ok(msg) => messages.push(Ok(msg)),
                    Err(e) => {
                        messages.push(Err(format!("malformed JSON in MCP SSE data: {e}")));
                    }
                },
            }
        }

        messages
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- SSE Parser Tests ----

    #[test]
    fn sse_parser_basic_response() {
        let mut parser = McpSseParser::new();
        let input = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let msg = events[0].as_ref().unwrap();
        assert!(matches!(msg, JsonRpcMessage::Response(_)));
    }

    #[test]
    fn sse_parser_notification() {
        let mut parser = McpSseParser::new();
        let input = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/updated\",\"params\":{\"uri\":\"file:///a\"}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let msg = events[0].as_ref().unwrap();
        assert!(matches!(msg, JsonRpcMessage::Notification(_)));
    }

    #[test]
    fn sse_parser_malformed_json_returns_error() {
        let mut parser = McpSseParser::new();
        let input = b"data: not json\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
    }

    #[test]
    fn sse_parser_multiple_events() {
        let mut parser = McpSseParser::new();
        let input = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 2);
        assert!(events[0].is_ok());
        assert!(events[1].is_ok());
    }

    #[test]
    fn sse_parser_incremental_chunks() {
        let mut parser = McpSseParser::new();

        let events1 = parser.feed(b"data: {\"jsonrpc\":\"2");
        assert!(events1.is_empty());

        let events2 = parser.feed(b".0\",\"id\":1,\"result\":{}}\n\n");
        assert_eq!(events2.len(), 1);
        assert!(events2[0].is_ok());
    }

    #[test]
    fn sse_parser_comments_ignored() {
        let mut parser = McpSseParser::new();
        let input = b": keepalive\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
    }

    #[test]
    fn sse_parser_empty_lines_no_event() {
        let mut parser = McpSseParser::new();
        let input = b"\n\n";
        let events = parser.feed(input);
        assert!(events.is_empty());
    }

    // ---- HTTP Transport Creation Tests ----

    #[test]
    fn create_http_transport_succeeds() {
        let result = create_http_transport("http://localhost:8080/mcp", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn create_http_transport_with_headers() {
        let headers = vec![
            ("Authorization".to_string(), "Bearer tok".to_string()),
            ("X-Custom".to_string(), "value".to_string()),
        ];
        let result = create_http_transport("http://localhost:8080/mcp", &headers);
        assert!(result.is_ok());
    }

    // ---- HttpReader EOF Behavior ----

    #[tokio::test]
    async fn http_reader_returns_none_on_sender_drop() {
        let (tx, rx) = mpsc::channel(8);
        let mut reader = HttpReader { message_rx: rx };
        let pending = std::sync::Mutex::new(HashMap::new());

        // Drop the sender
        drop(tx);

        let result = reader.recv(&pending).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn http_reader_receives_response() {
        let (tx, rx) = mpsc::channel(8);
        let mut reader = HttpReader { message_rx: rx };
        let pending = std::sync::Mutex::new(HashMap::new());

        // Register a pending request for correlation
        pending.lock().unwrap().insert(
            "1".to_string(),
            PendingRequest {
                method: "tools/list".to_string(),
                params: None,
            },
        );

        // Send a response message
        let resp = JsonRpcResponse::success(json!(1), json!({"tools": []}));
        tx.send(JsonRpcMessage::Response(resp)).await.unwrap();

        let result = reader.recv(&pending).await.unwrap();
        assert!(result.is_some());
        match result.unwrap() {
            McpClientMessage::Response {
                method, is_error, ..
            } => {
                assert_eq!(method, "tools/list");
                assert!(!is_error);
            }
            other => panic!("expected Response, got: {other:?}"),
        }
    }

    // ---- SSE Parser Property Tests ----

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Generates a well-formed SSE frame with a JSON-RPC response as data.
        fn arb_sse_frame() -> impl Strategy<Value = Vec<u8>> {
            (1..=100_i64).prop_map(|id| {
                format!("data: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}\n\n")
                    .into_bytes()
            })
        }

        /// Generates a valid SSE stream with split points.
        fn arb_sse_stream_with_splits() -> impl Strategy<Value = (Vec<u8>, Vec<usize>)> {
            prop::collection::vec(arb_sse_frame(), 1..6).prop_flat_map(|frames| {
                let stream: Vec<u8> = frames.into_iter().flatten().collect();
                let len = stream.len();
                let splits = prop::collection::vec(0..len, 1..8).prop_map(|mut pts| {
                    pts.sort_unstable();
                    pts.dedup();
                    pts
                });
                (Just(stream), splits)
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn prop_mcp_sse_chunk_independence(
                (stream, splits) in arb_sse_stream_with_splits()
            ) {
                // Parse all-at-once
                let mut one_shot = McpSseParser::new();
                let one_shot_ok: Vec<_> = one_shot
                    .feed(&stream)
                    .into_iter()
                    .filter_map(Result::ok)
                    .collect();

                // Parse in chunks at split points
                let mut chunked = McpSseParser::new();
                let mut chunked_ok: Vec<_> = Vec::new();
                let mut prev = 0;
                for &split in &splits {
                    if split > prev {
                        chunked_ok.extend(
                            chunked.feed(&stream[prev..split]).into_iter().filter_map(Result::ok),
                        );
                        prev = split;
                    }
                }
                chunked_ok.extend(
                    chunked.feed(&stream[prev..]).into_iter().filter_map(Result::ok),
                );

                prop_assert_eq!(one_shot_ok.len(), chunked_ok.len(),
                    "chunk independence: one-shot={}, chunked={}",
                    one_shot_ok.len(), chunked_ok.len());
            }

            #[test]
            fn prop_mcp_sse_no_panic(data in prop::collection::vec(any::<u8>(), 0..512)) {
                let mut parser = McpSseParser::new();
                let _ = parser.feed(&data);
            }
        }
    }

    // ---- Factory Function Tests ----

    #[test]
    fn create_driver_neither_command_nor_endpoint_errors() {
        let result =
            crate::protocol::mcp_client::create_mcp_client_driver(None, &[], None, &[], false);
        match result {
            Err(e) => {
                let err = e.to_string();
                assert!(
                    err.contains("mcp_client mode requires"),
                    "unexpected error: {err}"
                );
            }
            Ok(_) => panic!("expected error when neither command nor endpoint provided"),
        }
    }

    #[test]
    fn create_driver_with_endpoint_succeeds() {
        let result = crate::protocol::mcp_client::create_mcp_client_driver(
            None,
            &[],
            Some("http://localhost:8080/mcp"),
            &[],
            false,
        );
        assert!(result.is_ok());
        let driver = result.unwrap();
        // HTTP transport → no child process
        assert!(driver.child.is_none());
    }
}
