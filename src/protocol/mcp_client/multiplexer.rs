use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::transport::{McpClientTransportReader, McpClientTransportWriter};
use super::{
    CorrelatedResponse, McpClientMessage, MultiplexerClosed, NotificationMessage, PendingRequest,
    SERVER_REQUEST_BUFFER_SIZE, SERVER_REQUEST_BUFFER_WARNING, ServerRequestMessage,
};

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
pub(super) struct MessageMultiplexer {
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
    pub(super) fn spawn(
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
    pub(super) fn register_response(&self, id: &Value) -> oneshot::Receiver<CorrelatedResponse> {
        let (tx, rx) = oneshot::channel();
        self.response_senders
            .lock()
            .expect("response_senders lock poisoned")
            .insert(id.to_string(), tx);
        rx
    }

    /// Returns the reason the multiplexer closed, if it has.
    pub(super) fn close_reason(&self) -> MultiplexerClosed {
        self.close_reason
            .lock()
            .expect("close_reason lock poisoned")
            .clone()
            .unwrap_or(MultiplexerClosed::TransportEof)
    }
}
