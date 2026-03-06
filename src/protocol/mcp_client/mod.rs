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

mod driver;
mod handler;
mod multiplexer;
mod transport;

#[cfg(test)]
mod tests;

use std::time::Duration;

use serde_json::Value;

pub use driver::{McpClientDriver, create_mcp_client_driver};

// ============================================================================
// Constants
// ============================================================================

/// Default per-request timeout.
///
/// Implements: TJ-SPEC-018 F-002
pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default post-action event loop timeout.
pub(super) const DEFAULT_PHASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle timeout used after explicit actions complete.
///
/// Action-driven phases should finish quickly once request/response actions are
/// done, while still allowing a short window for trailing notifications.
pub(super) const POST_ACTION_IDLE_TIMEOUT: Duration = Duration::from_millis(250);

/// Initialization handshake timeout.
pub(super) const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Server request handler channel capacity.
///
/// Implements: TJ-SPEC-018 F-002
pub(super) const SERVER_REQUEST_BUFFER_SIZE: usize = 64;

/// Capacity warning threshold (75% of buffer).
pub(super) const SERVER_REQUEST_BUFFER_WARNING: usize = SERVER_REQUEST_BUFFER_SIZE / 4;

/// Maximum queued transport messages awaiting multiplexer classification.
pub(super) const HTTP_MESSAGE_BUFFER_SIZE: usize = 256;

/// Maximum queued notifications awaiting driver forwarding.
pub(super) const NOTIFICATION_BUFFER_SIZE: usize = 128;

/// Maximum queued handler protocol events awaiting driver forwarding.
pub(super) const HANDLER_EVENT_BUFFER_SIZE: usize = 128;

// ============================================================================
// Core Types
// ============================================================================

/// Classified message from the MCP transport reader.
///
/// Implements: TJ-SPEC-018 F-001
#[derive(Debug)]
pub(super) enum McpClientMessage {
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
        /// Original request params (for qualifier resolution on response events).
        request_params: Option<Value>,
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
/// Stores both method and params so that qualifier resolution on
/// response events can access the original request context
/// (e.g., `tools/call:calculator` resolves from `params.name`).
///
/// Implements: TJ-SPEC-018 F-002, F-008
#[derive(Debug)]
pub(super) struct PendingRequest {
    /// Original request method.
    pub(super) method: String,
    /// Original request params (for qualifier resolution on response events).
    pub(super) params: Option<Value>,
}

/// Correlated response returned via oneshot channel.
///
/// Includes the original request params so that qualifier resolution
/// on response events can access the request context.
///
/// Implements: TJ-SPEC-018 F-002, F-008
#[derive(Debug)]
pub(super) struct CorrelatedResponse {
    /// Correlated request method.
    pub(super) method: String,
    /// Result value (from response.result or response.error).
    pub(super) result: Value,
    /// Whether this is an error response.
    pub(super) is_error: bool,
    /// Original request params (for qualifier resolution).
    pub(super) request_params: Option<Value>,
}

/// Notification routed by the multiplexer.
#[derive(Debug)]
pub(super) struct NotificationMessage {
    /// Notification method.
    pub(super) method: String,
    /// Notification params.
    pub(super) params: Option<Value>,
}

/// Server-initiated request routed by the multiplexer to the handler.
#[derive(Debug)]
pub(super) struct ServerRequestMessage {
    /// Request ID.
    pub(super) id: Value,
    /// Request method.
    pub(super) method: String,
    /// Request params.
    pub(super) params: Option<Value>,
}

/// Reason why the multiplexer closed.
///
/// Implements: TJ-SPEC-018 F-011
#[derive(Debug, Clone)]
pub(super) enum MultiplexerClosed {
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
pub(super) struct HandlerState {
    /// Current phase effective state.
    pub(super) state: Value,
}
