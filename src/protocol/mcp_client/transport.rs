use std::collections::HashMap;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::error::EngineError;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};

use super::{McpClientMessage, PendingRequest};

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
/// Implements: TJ-SPEC-018 F-001
pub(super) struct StdioReader {
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
pub(super) fn classify_message(
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
pub(super) fn spawn_stdio_transport(
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
