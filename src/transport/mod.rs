//! Transport abstraction layer (TJ-SPEC-002).
//!
//! Provides the [`Transport`] trait for sending and receiving JSON-RPC messages
//! over different transport mechanisms (stdio, HTTP). The transport layer handles
//! message framing, serialization, and delivery behavior adaptation.

pub mod http;
pub mod jsonrpc;
pub mod sse;
pub mod stdio;

pub use http::{HttpTransport, ResponseHandle, ResponseHandleAdapter};
// RawResponseWriter is used internally by HttpTransport::capture_raw_writer()
// and by apply_delivery() in the MCP server driver.
pub use jsonrpc::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse,
};
pub use stdio::StdioTransport;

use crate::error::TransportError;

use bytes::Bytes;
use std::fmt;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Result type alias for transport operations.
pub type Result<T> = std::result::Result<T, TransportError>;

/// Default maximum message size in bytes (10 MB).
///
/// Implements: TJ-SPEC-002 F-008
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

/// Default buffer size for stdio transport (64 KB).
///
/// Implements: TJ-SPEC-002 F-002
pub const DEFAULT_STDIO_BUFFER_SIZE: usize = 64 * 1024;

/// Async transport trait for sending and receiving JSON-RPC messages.
///
/// Implementations handle message framing, serialization, and transport-specific
/// concerns. The trait uses `&self` with interior mutability (via `tokio::sync::Mutex`)
/// to allow shared ownership while supporting concurrent read/write.
///
/// Implements: TJ-SPEC-002 F-001
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Sends a complete JSON-RPC message with proper framing.
    ///
    /// For stdio: serializes to JSON and writes with newline terminator.
    /// For HTTP: sends as HTTP response body.
    async fn send_message(&self, message: &JsonRpcMessage) -> Result<()>;

    /// Sends raw bytes without JSON-RPC framing.
    ///
    /// Used by behavioral attacks that manipulate message framing
    /// (e.g., unbounded line, slow loris byte dripping).
    async fn send_raw(&self, bytes: &[u8]) -> Result<()>;

    /// Receives the next JSON-RPC message.
    ///
    /// Returns `Ok(None)` on EOF (clean shutdown).
    /// Returns `Err` on I/O or parse errors.
    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>>;

    /// Returns the type of this transport for logging and metrics.
    fn transport_type(&self) -> TransportType;

    /// Signals that the current response delivery is complete.
    ///
    /// For HTTP: drops the response body sender, ending the chunked stream and
    /// completing the HTTP response. For stdio: no-op.
    ///
    /// Implements: TJ-SPEC-002 F-001
    async fn finalize_response(&self) -> Result<()>;

    /// Returns the connection context for the current request.
    ///
    /// For HTTP: returns context set by the last `receive_message()`.
    /// For stdio: returns the fixed stdio context.
    ///
    /// Implements: TJ-SPEC-002 F-016
    fn connection_context(&self) -> ConnectionContext;

    /// Captures a per-request raw byte writer for background delivery tasks.
    ///
    /// For HTTP: clones the current response channel sender so a spawned
    /// task can write without being affected by new requests overwriting
    /// the shared `current_response` slot. Prevents the slow\_stream race
    /// where interleaved `receive_message()` calls redirect bytes to the
    /// wrong client.
    ///
    /// For stdio: returns `None` (single-writer, no per-request isolation needed).
    ///
    /// Implements: TJ-SPEC-002 F-003
    async fn capture_raw_writer(&self) -> Result<Option<RawResponseWriter>> {
        Ok(None)
    }

    /// Returns `self` as `&dyn Any` for downcasting.
    ///
    /// Used by the server to downcast to concrete transport types
    /// (e.g., `HttpTransport`) for transport-specific functionality.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A captured per-request writer for sending raw bytes to an HTTP response.
///
/// Cloned from the `HttpTransport` response channel at capture time, isolating
/// the writer from subsequent `receive_message()` calls that would overwrite
/// the shared slot.
///
/// Implements: TJ-SPEC-002 F-003
pub struct RawResponseWriter {
    tx: mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
}

impl RawResponseWriter {
    /// Creates a new writer wrapping the given channel sender.
    #[must_use]
    pub const fn new(tx: mpsc::Sender<std::result::Result<Bytes, std::io::Error>>) -> Self {
        Self { tx }
    }

    /// Sends raw bytes into the HTTP response body.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::ConnectionClosed`] if the client disconnected.
    pub async fn send_raw(&self, bytes: &[u8]) -> Result<()> {
        self.tx
            .send(Ok(Bytes::copy_from_slice(bytes)))
            .await
            .map_err(|_| TransportError::ConnectionClosed("response channel closed".into()))
    }
}

/// Transport type identifier.
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportType {
    /// NDJSON over stdin/stdout.
    Stdio,
    /// HTTP POST + Server-Sent Events.
    Http,
}

impl fmt::Display for TransportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => write!(f, "stdio"),
            Self::Http => write!(f, "http"),
        }
    }
}

/// Context for the current connection.
///
/// Provides metadata about the connection for use in side effects,
/// logging, and connection-scoped state management.
///
/// Implements: TJ-SPEC-002 F-016
#[derive(Debug, Clone)]
pub struct ConnectionContext {
    /// Unique connection identifier.
    ///
    /// Always 0 for stdio (single connection). Incrementing for HTTP.
    pub connection_id: u64,

    /// Remote address of the client (HTTP only).
    pub remote_addr: Option<SocketAddr>,

    /// Whether this is the only possible connection.
    ///
    /// Always `true` for stdio. `false` for HTTP (multiple connections possible).
    pub is_exclusive: bool,

    /// When this connection was established.
    pub connected_at: Instant,
}

impl ConnectionContext {
    /// Creates a connection context for the stdio transport.
    ///
    /// Implements: TJ-SPEC-002 F-002
    #[must_use]
    pub fn stdio() -> Self {
        Self {
            connection_id: 0,
            remote_addr: None,
            is_exclusive: true,
            connected_at: Instant::now(),
        }
    }
}

// ============================================================================
// NullTransport (test-only)
// ============================================================================

/// A no-op transport for tests that pends forever on reads.
///
/// `receive_message()` returns `Pending` indefinitely, allowing the
/// driver's `select!` to pick up cancellation tokens cleanly —
/// unlike `StdioTransport` whose `spawn_blocking` stdin reads are
/// uncancellable and cause test hangs.
///
/// Implements: TJ-SPEC-002 F-001
#[cfg(test)]
pub struct NullTransport;

#[cfg(test)]
#[async_trait::async_trait]
impl Transport for NullTransport {
    async fn send_message(&self, _message: &JsonRpcMessage) -> Result<()> {
        Ok(())
    }

    async fn send_raw(&self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>> {
        // Pend forever — the driver's `select!` will pick up the cancel token.
        // Returning Ok(None) (EOF) would cause a tight spin loop in PhaseLoop.
        std::future::pending().await
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Stdio
    }

    async fn finalize_response(&self) -> Result<()> {
        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext::stdio()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_type_display() {
        assert_eq!(TransportType::Stdio.to_string(), "stdio");
        assert_eq!(TransportType::Http.to_string(), "http");
    }

    #[test]
    fn test_connection_context_stdio() {
        let ctx = ConnectionContext::stdio();
        assert_eq!(ctx.connection_id, 0);
        assert!(ctx.remote_addr.is_none());
        assert!(ctx.is_exclusive);
    }

    #[test]
    fn test_constants() {
        assert_eq!(DEFAULT_MAX_MESSAGE_SIZE, 10 * 1024 * 1024);
        assert_eq!(DEFAULT_STDIO_BUFFER_SIZE, 64 * 1024);
    }
}
