use std::collections::HashMap;
use std::sync::Arc;

use oatf::ResponseEntry;
use oatf::primitives::{interpolate_value, select_response};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::types::{Direction, ProtocolEvent};
use crate::error::EngineError;
use crate::transport::jsonrpc::error_codes;

use super::transport::McpClientTransportWriter;
use super::{HandlerState, ServerRequestMessage};

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
#[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
pub(super) async fn server_request_handler(
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
pub(super) fn handle_sampling(
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
pub(super) fn handle_elicitation(
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
pub(super) fn handle_roots_list(state: &Value) -> Value {
    state
        .get("roots")
        .map_or_else(|| json!({"roots": []}), |roots| json!({"roots": roots}))
}

/// Default sampling response when no matching entry is found.
pub(super) fn default_sampling_response() -> Value {
    json!({
        "role": "assistant",
        "content": {"type": "text", "text": ""},
        "model": "default",
        "stopReason": "endTurn"
    })
}

/// Normalize heterogeneous OATF action YAML to uniform `{"type": ..., ...params}`.
///
/// OATF deserializes actions as:
/// - Bare string: `"list_tools"` → `Value::String`
/// - Single-key object: `{"call_tool": {"name": "foo", ...}}` → `Value::Object`
///
/// This normalizes both to `{"type": "...", ...params}`.
///
/// Implements: TJ-SPEC-018 F-006
pub(super) fn normalize_action(value: &Value) -> Value {
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
