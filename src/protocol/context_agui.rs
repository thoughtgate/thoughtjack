//! Context-mode AG-UI `PhaseDriver`.
//!
//! `ContextAgUiDriver` bridges AG-UI phase state to the `Transport` trait,
//! allowing it to work with `AgUiHandle` (channel-based) instead of
//! `AgUiTransport` (HTTP/SSE). Reuses `build_run_agent_input()` from the
//! traffic-mode AG-UI module.
//!
//! See TJ-SPEC-022 §2.7 for the driver specification.

use std::collections::HashMap;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;
use crate::protocol::agui::build_run_agent_input;
use crate::transport::{JsonRpcMessage, JsonRpcRequest, Transport, JSONRPC_VERSION};

/// Context-mode AG-UI `PhaseDriver`.
///
/// Uses the generic `Transport` trait (implemented by `AgUiHandle`) instead
/// of the HTTP-specific `AgUiTransport`. Sends `RunAgentInput` via
/// `transport.send_message()` and receives AG-UI events via
/// `transport.receive_message()`.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ContextAgUiDriver {
    transport: Box<dyn Transport>,
    thread_id: String,
}

impl ContextAgUiDriver {
    /// Creates a new context-mode AG-UI driver.
    #[must_use]
    pub fn new(transport: Box<dyn Transport>, thread_id: String) -> Self {
        Self {
            transport,
            thread_id,
        }
    }
}

#[async_trait::async_trait]
impl PhaseDriver for ContextAgUiDriver {
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        // Client-mode: clone extractors once at start
        let current_extractors = extractors.borrow().clone();

        // Build RunAgentInput from state (reuses AG-UI module function)
        let input = build_run_agent_input(state, &current_extractors, &self.thread_id)?;
        let input_value = serde_json::to_value(&input)
            .map_err(|e| EngineError::Driver(format!("serialize RunAgentInput: {e}")))?;

        // Emit outgoing request event
        let _ = event_tx
            .send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: "run_agent_input".to_string(),
                content: input_value.clone(),
            })
            .await;

        // Send via Transport (AgUiHandle forwards to agui_response_tx)
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "run_agent_input".to_string(),
            params: Some(input_value),
            id: json!(uuid::Uuid::new_v4().to_string()),
        });
        self.transport
            .send_message(&msg)
            .await
            .map_err(|e| EngineError::Driver(format!("send RunAgentInput: {e}")))?;

        // Receive events from drive loop via transport.receive_message()
        loop {
            tokio::select! {
                result = self.transport.receive_message() => {
                    match result {
                        Ok(Some(msg)) => {
                            let (method, content) = extract_event_from_message(&msg);
                            let _ = event_tx
                                .send(ProtocolEvent {
                                    direction: Direction::Incoming,
                                    method,
                                    content,
                                })
                                .await;
                        }
                        Ok(None) => return Ok(DriveResult::TransportClosed),
                        Err(e) => {
                            tracing::warn!(error = %e, "receive error");
                            return Ok(DriveResult::TransportClosed);
                        }
                    }
                }
                () = cancel.cancelled() => return Ok(DriveResult::Complete),
            }
        }
    }
}

/// Extracts event method and content from a `JsonRpcMessage`.
///
/// Maps `JsonRpcNotification` method names to `ProtocolEvent` fields.
/// Passes method names through unchanged (drive loop uses canonical names).
fn extract_event_from_message(msg: &JsonRpcMessage) -> (String, Value) {
    match msg {
        JsonRpcMessage::Notification(notif) => {
            let content = notif.params.clone().unwrap_or(json!({}));
            (notif.method.clone(), content)
        }
        JsonRpcMessage::Response(resp) => {
            let content = resp
                .result
                .clone()
                .or_else(|| resp.error.as_ref().map(|e| json!({"error": e.message})))
                .unwrap_or(json!({}));
            ("response".to_string(), content)
        }
        JsonRpcMessage::Request(req) => {
            let content = req.params.clone().unwrap_or(json!({}));
            (req.method.clone(), content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::JsonRpcNotification;

    #[test]
    fn test_extract_event_from_notification() {
        let msg = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "text_message_content",
            Some(json!({"delta": "hello"})),
        ));
        let (method, content) = extract_event_from_message(&msg);
        assert_eq!(method, "text_message_content");
        assert_eq!(content["delta"], "hello");
    }

    #[test]
    fn test_extract_event_from_notification_no_params() {
        let msg = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "run_finished",
            None,
        ));
        let (method, content) = extract_event_from_message(&msg);
        assert_eq!(method, "run_finished");
        assert_eq!(content, json!({}));
    }
}
