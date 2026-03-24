use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::TransportError;
use crate::transport::{ConnectionContext, JsonRpcMessage, Transport, TransportType};

/// Entry for a server actor in the `ContextTransport` routing table.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ServerActorEntry {
    /// Channel sender for dispatching tool calls to this actor.
    pub tx: mpsc::Sender<JsonRpcMessage>,
    /// Actor mode (`"mcp_server"` or `"a2a_server"`).
    pub mode: String,
    /// For A2A actors: watch receiver for the current default skill ID.
    ///
    /// Updated by the `PhaseLoop` on phase advance so that tool-call
    /// dispatch always uses the first skill from the current phase
    /// (not the stale Phase 0 value).
    pub a2a_skill_rx: Option<tokio::sync::watch::Receiver<Option<String>>>,
}

/// A server-initiated request (elicitation/sampling) routed to the drive loop.
///
/// Tagged with the actor name so the response can be routed back.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ServerRequest {
    /// Name of the originating actor.
    pub actor_name: String,
    /// The JSON-RPC request message.
    pub request: JsonRpcMessage,
}

/// Channel-based transport handle for the AG-UI actor in context-mode.
///
/// Receives AG-UI events (text, tool calls, `run_finished`) from the drive
/// loop via `rx` and sends follow-up user messages back via `response_tx`.
///
/// Implements: TJ-SPEC-022 F-001
pub struct AgUiHandle {
    rx: tokio::sync::Mutex<mpsc::Receiver<JsonRpcMessage>>,
    response_tx: mpsc::Sender<JsonRpcMessage>,
    created_at: Instant,
}

impl AgUiHandle {
    /// Creates a new AG-UI handle.
    #[must_use]
    pub fn new(
        rx: mpsc::Receiver<JsonRpcMessage>,
        response_tx: mpsc::Sender<JsonRpcMessage>,
    ) -> Self {
        Self {
            rx: tokio::sync::Mutex::new(rx),
            response_tx,
            created_at: Instant::now(),
        }
    }
}

#[async_trait::async_trait]
impl Transport for AgUiHandle {
    async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
        self.response_tx
            .send(message.clone())
            .await
            .map_err(|_| TransportError::ConnectionClosed("drive loop closed".into()))?;
        Ok(())
    }

    async fn send_raw(&self, _bytes: &[u8]) -> crate::transport::Result<()> {
        Err(TransportError::ConnectionClosed(
            "send_raw not supported in context-mode".into(),
        ))
    }

    async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Context
    }

    async fn finalize_response(&self) -> crate::transport::Result<()> {
        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext {
            connection_id: 0,
            remote_addr: None,
            is_exclusive: true,
            connected_at: self.created_at,
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ============================================================================
// ServerHandle
// ============================================================================

/// Channel-based transport handle for server actors in context-mode.
///
/// Receives tool call requests from the drive loop via `rx`. Sends tool
/// results via `result_tx` and server-initiated requests (elicitation,
/// sampling) via `server_request_tx`.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ServerHandle {
    rx: tokio::sync::Mutex<mpsc::Receiver<JsonRpcMessage>>,
    result_tx: mpsc::Sender<JsonRpcMessage>,
    server_request_tx: mpsc::Sender<ServerRequest>,
    actor_name: String,
    created_at: Instant,
}

impl ServerHandle {
    /// Creates a new server handle.
    #[must_use]
    pub fn new(
        rx: mpsc::Receiver<JsonRpcMessage>,
        result_tx: mpsc::Sender<JsonRpcMessage>,
        server_request_tx: mpsc::Sender<ServerRequest>,
        actor_name: String,
    ) -> Self {
        Self {
            rx: tokio::sync::Mutex::new(rx),
            result_tx,
            server_request_tx,
            actor_name,
            created_at: Instant::now(),
        }
    }
}

#[async_trait::async_trait]
impl Transport for ServerHandle {
    async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
        match message {
            JsonRpcMessage::Request(_) => {
                // Server-initiated request (elicitation/sampling) —
                // route to drive loop for LLM roundtrip
                self.server_request_tx
                    .send(ServerRequest {
                        actor_name: self.actor_name.clone(),
                        request: message.clone(),
                    })
                    .await
                    .map_err(|_| TransportError::ConnectionClosed("drive loop closed".into()))?;
            }
            _ => {
                // Tool result or notification — route to result channel
                self.result_tx.send(message.clone()).await.map_err(|_| {
                    TransportError::ConnectionClosed("context transport closed".into())
                })?;
            }
        }
        Ok(())
    }

    async fn send_raw(&self, _bytes: &[u8]) -> crate::transport::Result<()> {
        Err(TransportError::ConnectionClosed(
            "send_raw not supported in context-mode".into(),
        ))
    }

    async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Context
    }

    async fn finalize_response(&self) -> crate::transport::Result<()> {
        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext {
            connection_id: 0,
            remote_addr: None,
            is_exclusive: true,
            connected_at: self.created_at,
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
