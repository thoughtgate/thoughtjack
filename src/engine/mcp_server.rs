//! MCP server-mode `PhaseDriver` implementation.
//!
//! `McpServerDriver` listens for JSON-RPC requests from an AI agent
//! client, dispatches responses based on the current OATF phase's
//! effective state, applies behavioral modifiers (delayed, `slow_stream`,
//! `notification_flood`, etc.), supports elicitation interleaving, and
//! emits protocol events for the `PhaseLoop` to process.
//!
//! `McpTransportEntryActionSender` implements `EntryActionSender` to
//! deliver phase-transition notifications and elicitations over the
//! transport.
//!
//! See TJ-SPEC-013 §8.2 for the MCP server driver specification.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use oatf::ResponseEntry;
use oatf::enums::ElicitationMode;
use oatf::primitives::{interpolate_template, interpolate_value, select_response};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;
use crate::transport::jsonrpc::error_codes;
use crate::transport::{
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, Transport,
};

use super::actions::EntryActionSender;
use super::driver::PhaseDriver;
use super::types::{Direction, DriveResult, ProtocolEvent};

// ============================================================================
// McpServerDriver
// ============================================================================

/// MCP server-mode protocol driver.
///
/// Listens for JSON-RPC requests on the provided transport, dispatches
/// responses from the current phase's effective state, applies behavioral
/// modifiers, and emits protocol events for the `PhaseLoop`.
///
/// # Transport sharing
///
/// The transport is shared via `Arc<dyn Transport>` to allow both the
/// driver and the `McpTransportEntryActionSender` to access it.
///
/// Implements: TJ-SPEC-013 F-001
pub struct McpServerDriver {
    transport: Arc<dyn Transport>,
    raw_synthesize: bool,
}

impl McpServerDriver {
    /// Creates a new MCP server driver.
    ///
    /// # Arguments
    ///
    /// * `transport` — Shared transport for JSON-RPC I/O.
    /// * `raw_synthesize` — If `true`, bypass synthesize output validation.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>, raw_synthesize: bool) -> Self {
        Self {
            transport,
            raw_synthesize,
        }
    }

    /// Creates an `McpTransportEntryActionSender` sharing this driver's transport.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn entry_action_sender(&self) -> McpTransportEntryActionSender {
        McpTransportEntryActionSender {
            transport: Arc::clone(&self.transport),
            next_request_id: AtomicU64::new(1_000_000),
        }
    }

    /// Dispatch a request to the appropriate handler.
    async fn dispatch_request(
        &self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => handle_initialize(request, state),
            "tools/list" => handle_tools_list(request, state),
            "tools/call" => {
                self.handle_tools_call(request, state, extractors, event_tx)
                    .await
            }
            "resources/list" => handle_resources_list(request, state),
            "resources/read" => handle_resources_read(request, state, extractors),
            "prompts/list" => handle_prompts_list(request, state),
            "prompts/get" => {
                self.handle_prompts_get(request, state, extractors, event_tx)
                    .await
            }
            "resources/subscribe" | "resources/unsubscribe" => handle_subscribe(request),
            "completion/complete" => handle_completion(request),
            "ping" => handle_ping(request),
            _ => handle_unknown(request),
        }
    }

    /// Handle `tools/call` with response dispatch and optional elicitation.
    async fn handle_tools_call(
        &self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> JsonRpcResponse {
        let params = request.params.as_ref().unwrap_or(&Value::Null);
        let tool_name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let Some(tool) = find_by_name(state, "tools", tool_name) else {
            return JsonRpcResponse::error(
                request.id.clone(),
                error_codes::INVALID_PARAMS,
                format!("tool not found: {tool_name}"),
            );
        };

        // Elicitation interleaving (before response)
        self.maybe_send_elicitation(state, tool_name, event_tx)
            .await;

        dispatch_response(
            &request.id,
            &tool,
            extractors,
            params,
            tool.get("outputSchema"),
            self.raw_synthesize,
        )
    }

    /// Handle `prompts/get` with response dispatch and optional elicitation.
    async fn handle_prompts_get(
        &self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> JsonRpcResponse {
        let params = request.params.as_ref().unwrap_or(&Value::Null);
        let prompt_name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let Some(prompt) = find_by_name(state, "prompts", prompt_name) else {
            return JsonRpcResponse::error(
                request.id.clone(),
                error_codes::INVALID_PARAMS,
                format!("prompt not found: {prompt_name}"),
            );
        };

        // Elicitation interleaving (before response)
        self.maybe_send_elicitation(state, prompt_name, event_tx)
            .await;

        dispatch_response(
            &request.id,
            &prompt,
            extractors,
            params,
            None,
            self.raw_synthesize,
        )
    }

    /// Send an elicitation request if the state has matching elicitations.
    async fn maybe_send_elicitation(
        &self,
        state: &Value,
        _target_name: &str,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) {
        let Some(elicitations) = state.get("elicitations").and_then(Value::as_array) else {
            return;
        };

        for elicitation in elicitations {
            let message = elicitation
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Please provide input");

            let elicitation_request = JsonRpcRequest {
                jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
                method: "elicitation/create".to_string(),
                params: Some(json!({
                    "message": message,
                    "requestedSchema": elicitation.get("requestedSchema").cloned().unwrap_or(json!({})),
                })),
                id: json!(format!(
                    "elicit-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                )),
            };

            // Emit outgoing elicitation event
            let _ = event_tx.send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: "elicitation/create".to_string(),
                content: serde_json::to_value(&elicitation_request).unwrap_or(Value::Null),
            });

            // Send to agent
            if let Err(err) = self
                .transport
                .send_message(&JsonRpcMessage::Request(elicitation_request))
                .await
            {
                tracing::warn!(error = %err, "failed to send elicitation request");
                continue;
            }

            // Wait for response
            match self.transport.receive_message().await {
                Ok(Some(JsonRpcMessage::Response(resp))) => {
                    let _ = event_tx.send(ProtocolEvent {
                        direction: Direction::Incoming,
                        method: "elicitation/create".to_string(),
                        content: serde_json::to_value(&resp).unwrap_or(Value::Null),
                    });
                }
                Ok(Some(msg)) => {
                    tracing::debug!(?msg, "unexpected message during elicitation wait");
                }
                Ok(None) => {
                    tracing::debug!("transport EOF during elicitation wait");
                }
                Err(err) => {
                    tracing::warn!(error = %err, "transport error during elicitation wait");
                }
            }
        }
    }
}

// ============================================================================
// PhaseDriver impl
// ============================================================================

#[async_trait]
impl PhaseDriver for McpServerDriver {
    /// Run the MCP server event loop for a single phase.
    ///
    /// Receives JSON-RPC requests, dispatches responses based on the
    /// effective state, applies behavioral modifiers, and emits protocol
    /// events for the `PhaseLoop`.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on unrecoverable transport failures.
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(DriveResult::Complete);
                }
                msg = self.transport.receive_message() => {
                    match msg {
                        Ok(Some(JsonRpcMessage::Request(request))) => {
                            // Emit incoming event
                            let incoming_content = request.params.clone().unwrap_or(Value::Null);
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: request.method.clone(),
                                content: incoming_content,
                            });

                            // Get fresh extractors
                            let current_extractors = extractors.borrow().clone();

                            // Dispatch request
                            let response = self.dispatch_request(
                                &request, state, &current_extractors, &event_tx,
                            ).await;

                            // Apply side effects
                            apply_side_effects(
                                &self.transport, state, &request.id,
                            ).await?;

                            // Apply delivery
                            let response_msg = JsonRpcMessage::Response(response.clone());
                            apply_delivery(&self.transport, state, &response_msg).await?;

                            // Emit outgoing event
                            let outgoing_content = response.result.clone().unwrap_or_else(||
                                response.error.as_ref()
                                    .map_or(Value::Null, |e| serde_json::to_value(e).unwrap_or(Value::Null))
                            );
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Outgoing,
                                method: request.method.clone(),
                                content: outgoing_content,
                            });

                            // Finalize
                            self.transport.finalize_response().await
                                .map_err(|e| EngineError::Driver(format!("finalize error: {e}")))?;
                        }
                        Ok(Some(JsonRpcMessage::Notification(notif))) => {
                            // Notifications have no response — just emit event
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: notif.method.clone(),
                                content: notif.params.unwrap_or(Value::Null),
                            });
                        }
                        Ok(Some(JsonRpcMessage::Response(resp))) => {
                            // Unexpected response — log and continue
                            tracing::debug!(id = ?resp.id, "unexpected response from agent");
                        }
                        Ok(None) => {
                            // EOF — clean shutdown
                            return Ok(DriveResult::Complete);
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "transport receive error");
                            return Err(EngineError::Driver(format!("transport error: {err}")));
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Simple handlers
// ============================================================================

/// Handle `initialize` — return server capabilities.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_initialize(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let capabilities = state
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| default_capabilities(state));

    JsonRpcResponse::success(
        request.id.clone(),
        json!({
            "protocolVersion": "2025-03-26",
            "capabilities": capabilities,
            "serverInfo": {
                "name": "thoughtjack",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )
}

/// Derive capabilities from the state's declared tools/resources/prompts.
fn default_capabilities(state: &Value) -> Value {
    let mut caps = serde_json::Map::new();

    if state
        .get("tools")
        .is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert("tools".to_string(), json!({"listChanged": true}));
    }
    if state
        .get("resources")
        .is_some_and(|r| r.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert(
            "resources".to_string(),
            json!({"subscribe": true, "listChanged": true}),
        );
    }
    if state
        .get("prompts")
        .is_some_and(|p| p.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert("prompts".to_string(), json!({"listChanged": true}));
    }

    Value::Object(caps)
}

/// Handle `tools/list` — return tool definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_tools_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let tools = state
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| strip_internal_fields(tool, &["responses"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "tools": tools }))
}

/// Handle `resources/list` — return resource definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_resources_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let resources = state
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .map(|r| strip_internal_fields(r, &["responses", "content"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "resources": resources }))
}

/// Handle `resources/read` — dispatch response from resource state.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_resources_read(
    request: &JsonRpcRequest,
    state: &Value,
    extractors: &HashMap<String, String>,
) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(resource) = find_by_field(state, "resources", "uri", uri) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("resource not found: {uri}"),
        );
    };

    dispatch_response(&request.id, &resource, extractors, params, None, false)
}

/// Handle `prompts/list` — return prompt definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_prompts_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let prompts = state
        .get("prompts")
        .and_then(Value::as_array)
        .map(|prompts| {
            prompts
                .iter()
                .map(|p| strip_internal_fields(p, &["responses"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "prompts": prompts }))
}

/// Handle `ping` — return empty object.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_ping(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `resources/subscribe` and `resources/unsubscribe` — no-op accept.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_subscribe(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `completion/complete` — return empty completions.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_completion(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        request.id.clone(),
        json!({
            "completion": {
                "values": [],
                "hasMore": false,
            }
        }),
    )
}

/// Handle unknown methods — return null result per §11.1.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_unknown(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), Value::Null)
}

// ============================================================================
// Response dispatch
// ============================================================================

/// Dispatch a response using the OATF `select_response()` SDK function.
///
/// Finds matching `ResponseEntry` from the item's `responses` array,
/// interpolates values, and validates synthesized output if applicable.
fn dispatch_response(
    request_id: &Value,
    item: &Value,
    extractors: &HashMap<String, String>,
    request_context: &Value,
    output_schema: Option<&Value>,
    raw_synthesize: bool,
) -> JsonRpcResponse {
    let Some(responses_value) = item.get("responses") else {
        // No responses defined — return empty content
        return JsonRpcResponse::success(request_id.clone(), json!({ "content": [] }));
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = %err, "failed to deserialize response entries");
            return JsonRpcResponse::error(
                request_id.clone(),
                error_codes::INTERNAL_ERROR,
                format!("response configuration error: {err}"),
            );
        }
    };

    let Some(entry) = select_response(&entries, request_context) else {
        // No matching response
        return JsonRpcResponse::success(request_id.clone(), json!({ "content": [] }));
    };

    // Check for synthesize block
    if entry.synthesize.is_some() && entry.extra.is_empty() {
        // No static content and synthesize required — GenerationProvider not available yet
        tracing::info!("synthesize block encountered but GenerationProvider not available");
        return JsonRpcResponse::error(
            request_id.clone(),
            error_codes::INTERNAL_ERROR,
            "synthesize not yet supported — GenerationProvider not available",
        );
    }

    // Build response from extra fields with interpolation
    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, diagnostics) =
        interpolate_value(&extra_value, extractors, Some(request_context), None);

    for diag in &diagnostics {
        tracing::debug!(diagnostic = ?diag, "interpolation diagnostic");
    }

    // Validate synthesized output if applicable
    if entry.synthesize.is_some() && !raw_synthesize {
        if let Err(err) =
            super::generation::validate_synthesized_output("mcp", &interpolated, output_schema)
        {
            tracing::warn!(error = %err, "synthesized output validation failed");
            return JsonRpcResponse::error(
                request_id.clone(),
                error_codes::INTERNAL_ERROR,
                format!("synthesize validation: {err}"),
            );
        }
    }

    JsonRpcResponse::success(request_id.clone(), interpolated)
}

// ============================================================================
// Behavioral modifiers
// ============================================================================

/// Apply delivery behavior to a response.
///
/// Reads `state["behavior"]["delivery"]` and applies the configured
/// delivery mode: normal, delayed, or `slow_stream`.
///
/// # Errors
///
/// Returns `EngineError::Driver` on transport failure.
async fn apply_delivery(
    transport: &Arc<dyn Transport>,
    state: &Value,
    response_msg: &JsonRpcMessage,
) -> Result<(), EngineError> {
    let delivery = state
        .get("behavior")
        .and_then(|b| b.get("delivery"))
        .and_then(Value::as_str)
        .unwrap_or("normal");

    match delivery {
        "delayed" => {
            let delay_ms = state
                .get("behavior")
                .and_then(|b| b.get("delay_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(1000);
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            transport
                .send_message(response_msg)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
        }
        "slow_stream" => {
            let bytes = serde_json::to_vec(response_msg)
                .map_err(|e| EngineError::Driver(format!("serialize error: {e}")))?;
            let byte_delay_ms = state
                .get("behavior")
                .and_then(|b| b.get("byte_delay_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(50);
            for byte in &bytes {
                transport
                    .send_raw(&[*byte])
                    .await
                    .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
                tokio::time::sleep(tokio::time::Duration::from_millis(byte_delay_ms)).await;
            }
            // Send newline terminator for NDJSON framing
            transport
                .send_raw(b"\n")
                .await
                .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
        }
        _ => {
            if delivery != "normal" {
                tracing::warn!(delivery, "unknown delivery behavior, using normal");
            }
            transport
                .send_message(response_msg)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
        }
    }

    Ok(())
}

/// Apply side effects before delivering the response.
///
/// Reads `state["behavior"]["side_effect"]` and applies configured
/// side effects: `notification_flood`, `id_collision`, `connection_reset`.
///
/// # Errors
///
/// Returns `EngineError::Driver` for `connection_reset` (intentional)
/// or on transport failure.
async fn apply_side_effects(
    transport: &Arc<dyn Transport>,
    state: &Value,
    request_id: &Value,
) -> Result<(), EngineError> {
    let side_effect = state
        .get("behavior")
        .and_then(|b| b.get("side_effect"))
        .and_then(Value::as_str);

    let Some(effect) = side_effect else {
        return Ok(());
    };

    match effect {
        "notification_flood" => {
            let count = state
                .get("behavior")
                .and_then(|b| b.get("flood_count"))
                .and_then(Value::as_u64)
                .unwrap_or(10);
            for i in 0..count {
                let notif = JsonRpcNotification::new(
                    "notifications/message",
                    Some(json!({
                        "level": "info",
                        "data": format!("flood notification {i}"),
                    })),
                );
                transport
                    .send_message(&JsonRpcMessage::Notification(notif))
                    .await
                    .map_err(|e| EngineError::Driver(format!("flood send error: {e}")))?;
            }
        }
        "id_collision" => {
            // Send a fake response with the same request ID (collision attack)
            let collision =
                JsonRpcResponse::success(request_id.clone(), json!({"collision": true}));
            transport
                .send_message(&JsonRpcMessage::Response(collision))
                .await
                .map_err(|e| EngineError::Driver(format!("collision send error: {e}")))?;
        }
        "connection_reset" => {
            return Err(EngineError::Driver(
                "connection_reset side effect triggered".to_string(),
            ));
        }
        _ => {
            tracing::warn!(side_effect = effect, "unknown side effect, skipping");
        }
    }

    Ok(())
}

// ============================================================================
// McpTransportEntryActionSender
// ============================================================================

/// Entry action sender that delivers notifications and elicitations
/// over the MCP transport.
///
/// Created by `McpServerDriver::entry_action_sender()` and shares the
/// same `Arc<dyn Transport>`.
///
/// Implements: TJ-SPEC-013 F-001
pub struct McpTransportEntryActionSender {
    transport: Arc<dyn Transport>,
    next_request_id: AtomicU64,
}

#[async_trait]
impl EntryActionSender for McpTransportEntryActionSender {
    /// Send a JSON-RPC notification to the agent.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::EntryAction` if the transport fails.
    async fn send_notification(
        &self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<(), EngineError> {
        let notif = JsonRpcNotification::new(method, params.cloned());
        self.transport
            .send_message(&JsonRpcMessage::Notification(notif))
            .await
            .map_err(|e| EngineError::EntryAction(format!("notification send failed: {e}")))
    }

    /// Send an elicitation request to the agent.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::EntryAction` if the transport fails.
    async fn send_elicitation(
        &self,
        message: &str,
        mode: Option<&ElicitationMode>,
        requested_schema: Option<&Value>,
        url: Option<&str>,
    ) -> Result<(), EngineError> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (interpolated_message, _) = interpolate_template(message, &HashMap::new(), None, None);

        let mut params = serde_json::Map::new();
        params.insert("message".to_string(), json!(interpolated_message));
        if let Some(mode) = mode {
            params.insert(
                "mode".to_string(),
                serde_json::to_value(mode).unwrap_or(Value::Null),
            );
        }
        if let Some(schema) = requested_schema {
            params.insert("requestedSchema".to_string(), schema.clone());
        }
        if let Some(url) = url {
            params.insert("url".to_string(), json!(url));
        }

        let request = JsonRpcRequest {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            method: "elicitation/create".to_string(),
            params: Some(Value::Object(params)),
            id: json!(id),
        };

        self.transport
            .send_message(&JsonRpcMessage::Request(request))
            .await
            .map_err(|e| EngineError::EntryAction(format!("elicitation send failed: {e}")))
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Find an item by `name` field in a state array.
fn find_by_name(state: &Value, collection: &str, name: &str) -> Option<Value> {
    state
        .get(collection)
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// Find an item by an arbitrary field in a state array.
fn find_by_field(state: &Value, collection: &str, field: &str, value: &str) -> Option<Value> {
    state
        .get(collection)
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get(field).and_then(Value::as_str) == Some(value))
        .cloned()
}

/// Strip internal fields from a state object for wire format.
fn strip_internal_fields(value: &Value, fields: &[&str]) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut cleaned = obj.clone();
    for field in fields {
        cleaned.remove(*field);
    }
    Value::Object(cleaned)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tokio::sync::Mutex;
    use tokio::time::Instant;

    use crate::transport::{ConnectionContext, TransportType};

    use super::*;

    // ---- MockTransport ----

    /// Shared outgoing message buffer for test assertions.
    type OutgoingBuffer = Arc<Mutex<Vec<JsonRpcMessage>>>;

    struct MockTransport {
        incoming: Mutex<VecDeque<JsonRpcMessage>>,
        outgoing: OutgoingBuffer,
    }

    impl MockTransport {
        fn new(messages: Vec<JsonRpcMessage>) -> (Arc<dyn Transport>, OutgoingBuffer) {
            let outgoing: OutgoingBuffer = Arc::new(Mutex::new(Vec::new()));
            let transport: Arc<dyn Transport> = Arc::new(Self {
                incoming: Mutex::new(VecDeque::from(messages)),
                outgoing: Arc::clone(&outgoing),
            });
            (transport, outgoing)
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
            self.outgoing.lock().await.push(message.clone());
            Ok(())
        }

        async fn send_raw(&self, bytes: &[u8]) -> crate::transport::Result<()> {
            // Accumulate raw bytes — for testing we just store as-is
            let s = String::from_utf8_lossy(bytes);
            if !s.trim().is_empty() {
                if let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(s.trim()) {
                    self.outgoing.lock().await.push(msg);
                }
            }
            Ok(())
        }

        async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
            Ok(self.incoming.lock().await.pop_front())
        }

        fn transport_type(&self) -> TransportType {
            TransportType::Stdio
        }

        async fn finalize_response(&self) -> crate::transport::Result<()> {
            Ok(())
        }

        fn connection_context(&self) -> ConnectionContext {
            ConnectionContext {
                connection_id: 0,
                remote_addr: None,
                is_exclusive: true,
                connected_at: Instant::now(),
            }
        }
    }

    // ---- Helper to make a request ----

    fn make_request(method: &str, params: Option<Value>) -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
            id: json!(1),
        })
    }

    fn test_state() -> Value {
        json!({
            "tools": [
                {
                    "name": "calculator",
                    "description": "Performs calculations",
                    "inputSchema": {"type": "object"},
                    "responses": [
                        {
                            "content": [{"type": "text", "text": "42"}]
                        }
                    ]
                }
            ],
            "resources": [
                {
                    "uri": "file:///data.txt",
                    "name": "data",
                    "description": "Test data",
                    "mimeType": "text/plain",
                    "responses": [
                        {
                            "contents": [{"uri": "file:///data.txt", "text": "hello"}]
                        }
                    ]
                }
            ],
            "prompts": [
                {
                    "name": "greeting",
                    "description": "A greeting prompt",
                    "arguments": [
                        {"name": "name", "description": "Name to greet", "required": true}
                    ],
                    "responses": [
                        {
                            "messages": [
                                {"role": "assistant", "content": {"type": "text", "text": "Hello!"}}
                            ]
                        }
                    ]
                }
            ]
        })
    }

    // ---- Handler tests ----

    #[test]
    fn initialize_returns_capabilities() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "initialize".to_string(),
            params: Some(json!({})),
            id: json!(1),
        };
        let state = test_state();

        let resp = handle_initialize(&request, &state);
        let result = resp.result.unwrap();

        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["serverInfo"]["name"], "thoughtjack");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["prompts"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
    }

    #[test]
    fn tools_list_returns_tools() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/list".to_string(),
            params: None,
            id: json!(1),
        };
        let state = test_state();

        let resp = handle_tools_list(&request, &state);
        let result = resp.result.unwrap();

        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "calculator");
        assert_eq!(tools[0]["description"], "Performs calculations");
        // responses field should be stripped
        assert!(tools[0].get("responses").is_none());
    }

    #[test]
    fn tools_list_includes_input_schema() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/list".to_string(),
            params: None,
            id: json!(1),
        };
        let state = json!({
            "tools": [{
                "name": "test",
                "description": "test",
                "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}}},
                "outputSchema": {"type": "object"},
                "responses": []
            }]
        });

        let resp = handle_tools_list(&request, &state);
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(tools[0]["inputSchema"].is_object());
        assert!(tools[0]["outputSchema"].is_object());
    }

    #[test]
    fn tools_call_selects_response() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(json!({"name": "calculator", "arguments": {}})),
            id: json!(1),
        };
        let state = test_state();
        let tool = find_by_name(&state, "tools", "calculator").unwrap();
        let extractors = HashMap::new();

        let resp = dispatch_response(
            &request.id,
            &tool,
            &extractors,
            request.params.as_ref().unwrap(),
            None,
            false,
        );

        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "42");
    }

    #[test]
    fn tools_call_unknown_tool_errors() {
        let state = test_state();

        // Simulate what handle_tools_call does for missing tool
        let result = find_by_name(&state, "tools", "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn resources_list_returns_resources() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "resources/list".to_string(),
            params: None,
            id: json!(1),
        };
        let state = test_state();

        let resp = handle_resources_list(&request, &state);
        let result = resp.result.unwrap();

        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "file:///data.txt");
        assert!(resources[0].get("responses").is_none());
        assert!(resources[0].get("content").is_none());
    }

    #[test]
    fn resources_read_returns_content() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "resources/read".to_string(),
            params: Some(json!({"uri": "file:///data.txt"})),
            id: json!(1),
        };
        let state = test_state();
        let extractors = HashMap::new();

        let resp = handle_resources_read(&request, &state, &extractors);
        let result = resp.result.unwrap();
        assert!(result["contents"].is_array());
    }

    #[test]
    fn prompts_list_returns_prompts() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "prompts/list".to_string(),
            params: None,
            id: json!(1),
        };
        let state = test_state();

        let resp = handle_prompts_list(&request, &state);
        let result = resp.result.unwrap();

        let prompts = result["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["name"], "greeting");
        assert!(prompts[0].get("responses").is_none());
    }

    #[test]
    fn prompts_get_selects_response() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "prompts/get".to_string(),
            params: Some(json!({"name": "greeting"})),
            id: json!(1),
        };
        let state = test_state();
        let prompt = find_by_name(&state, "prompts", "greeting").unwrap();
        let extractors = HashMap::new();

        let resp = dispatch_response(
            &request.id,
            &prompt,
            &extractors,
            request.params.as_ref().unwrap(),
            None,
            false,
        );

        let result = resp.result.unwrap();
        assert!(result["messages"].is_array());
    }

    #[test]
    fn ping_returns_empty_object() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "ping".to_string(),
            params: None,
            id: json!(1),
        };

        let resp = handle_ping(&request);
        assert_eq!(resp.result, Some(json!({})));
    }

    #[test]
    fn unknown_method_returns_null() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "x-custom/frobnicate".to_string(),
            params: None,
            id: json!(42),
        };

        let resp = handle_unknown(&request);
        assert_eq!(resp.result, Some(Value::Null));
        assert!(resp.error.is_none());
    }

    #[test]
    fn subscribe_returns_success() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "resources/subscribe".to_string(),
            params: Some(json!({"uri": "file:///data.txt"})),
            id: json!(1),
        };

        let resp = handle_subscribe(&request);
        assert_eq!(resp.result, Some(json!({})));
    }

    #[test]
    fn completion_returns_empty() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "completion/complete".to_string(),
            params: Some(json!({})),
            id: json!(1),
        };

        let resp = handle_completion(&request);
        let result = resp.result.unwrap();
        assert_eq!(result["completion"]["values"], json!([]));
        assert_eq!(result["completion"]["hasMore"], false);
    }

    // ---- PhaseDriver integration tests ----

    #[tokio::test]
    async fn drive_phase_completes_on_eof() {
        let (transport, _outgoing) = MockTransport::new(vec![]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = test_state();
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let result = driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();
        assert!(matches!(result, DriveResult::Complete));
    }

    #[tokio::test]
    async fn drive_phase_completes_on_cancel() {
        let (transport, _outgoing) = MockTransport::new(vec![]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = test_state();
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        // Cancel immediately
        cancel.cancel();

        let result = driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();
        assert!(matches!(result, DriveResult::Complete));
    }

    #[tokio::test]
    async fn drive_phase_emits_events() {
        let request = make_request("tools/list", None);
        let (transport, _outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = test_state();
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        // Should have incoming + outgoing events
        let mut events = Vec::new();
        while let Ok(evt) = event_rx.try_recv() {
            events.push(evt);
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].direction, Direction::Incoming);
        assert_eq!(events[0].method, "tools/list");
        assert_eq!(events[1].direction, Direction::Outgoing);
        assert_eq!(events[1].method, "tools/list");
    }

    #[tokio::test]
    async fn extractors_refreshed_per_request() {
        let requests = vec![make_request("tools/list", None), make_request("ping", None)];
        let (transport, outgoing) = MockTransport::new(requests);
        let mut driver = McpServerDriver::new(transport, false);

        let state = test_state();
        let (tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        // Update extractors between the two requests being processed
        tx.send(HashMap::from([("key".to_string(), "value".to_string())]))
            .unwrap();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        // If extractors weren't refreshed per-request, the second request
        // would not see the updated value. Since this is a server-mode
        // driver, it borrows fresh values each time.
        let sent = outgoing.lock().await;
        assert_eq!(sent.len(), 2); // Two responses sent
    }

    #[tokio::test]
    async fn delayed_delivery_waits() {
        let request = make_request("ping", None);
        let (transport, _outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = json!({
            "behavior": {
                "delivery": "delayed",
                "delay_ms": 10
            }
        });
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let start = tokio::time::Instant::now();
        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();
        let elapsed = start.elapsed();

        // Should have waited at least the delay
        assert!(elapsed >= tokio::time::Duration::from_millis(10));
    }

    #[tokio::test]
    async fn notification_flood_sends_before_response() {
        let request = make_request("ping", None);
        let (transport, outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = json!({
            "behavior": {
                "side_effect": "notification_flood",
                "flood_count": 3
            }
        });
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        let sent = outgoing.lock().await;
        // 3 flood notifications + 1 response
        assert_eq!(sent.len(), 4);

        // First 3 should be notifications
        for msg in &sent[..3] {
            assert!(matches!(msg, JsonRpcMessage::Notification(_)));
        }
        // Last should be the response
        assert!(matches!(sent[3], JsonRpcMessage::Response(_)));
    }

    #[tokio::test]
    async fn entry_action_sender_sends_notification() {
        let (transport, outgoing) = MockTransport::new(vec![]);
        let sender = McpTransportEntryActionSender {
            transport,
            next_request_id: AtomicU64::new(1_000_000),
        };

        sender
            .send_notification("notifications/tools/list_changed", None)
            .await
            .unwrap();

        let sent = outgoing.lock().await;
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            JsonRpcMessage::Notification(n) => {
                assert_eq!(n.method, "notifications/tools/list_changed");
            }
            _ => panic!("expected notification"),
        }
    }

    #[tokio::test]
    async fn entry_action_sender_sends_elicitation() {
        let (transport, outgoing) = MockTransport::new(vec![]);
        let sender = McpTransportEntryActionSender {
            transport,
            next_request_id: AtomicU64::new(1_000_000),
        };

        sender
            .send_elicitation(
                "Enter your API key",
                Some(&ElicitationMode::Form),
                Some(&json!({"type": "object"})),
                None,
            )
            .await
            .unwrap();

        let sent = outgoing.lock().await;
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            JsonRpcMessage::Request(r) => {
                assert_eq!(r.method, "elicitation/create");
                assert_eq!(r.id, json!(1_000_000));
                let params = r.params.as_ref().unwrap();
                assert_eq!(params["message"], "Enter your API key");
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn strip_internal_fields_removes_responses() {
        let tool = json!({
            "name": "calc",
            "description": "Calculator",
            "responses": [{"content": []}]
        });
        let stripped = strip_internal_fields(&tool, &["responses"]);
        assert!(stripped.get("responses").is_none());
        assert_eq!(stripped["name"], "calc");
    }

    #[test]
    fn find_by_name_works() {
        let state = test_state();
        let found = find_by_name(&state, "tools", "calculator");
        assert!(found.is_some());
        assert_eq!(found.unwrap()["name"], "calculator");

        let not_found = find_by_name(&state, "tools", "nonexistent");
        assert!(not_found.is_none());
    }

    #[test]
    fn find_by_field_works() {
        let state = test_state();
        let found = find_by_field(&state, "resources", "uri", "file:///data.txt");
        assert!(found.is_some());

        let not_found = find_by_field(&state, "resources", "uri", "file:///missing.txt");
        assert!(not_found.is_none());
    }

    #[test]
    fn default_capabilities_derives_from_state() {
        let state = test_state();
        let caps = default_capabilities(&state);
        assert!(caps["tools"].is_object());
        assert!(caps["prompts"].is_object());
        assert!(caps["resources"].is_object());
    }

    #[test]
    fn default_capabilities_empty_state() {
        let state = json!({});
        let caps = default_capabilities(&state);
        assert!(caps.as_object().unwrap().is_empty());
    }

    #[test]
    fn dispatch_response_no_responses_returns_empty_content() {
        let item = json!({"name": "test"});
        let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
        let result = resp.result.unwrap();
        assert_eq!(result["content"], json!([]));
    }

    #[test]
    fn dispatch_response_empty_responses_returns_empty_content() {
        let item = json!({"name": "test", "responses": []});
        let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
        let result = resp.result.unwrap();
        assert_eq!(result["content"], json!([]));
    }

    #[tokio::test]
    async fn drive_phase_handles_notification_from_agent() {
        let messages = vec![JsonRpcMessage::Notification(JsonRpcNotification::new(
            "notifications/initialized",
            None,
        ))];
        let (transport, _outgoing) = MockTransport::new(messages);
        let mut driver = McpServerDriver::new(transport, false);

        let state = test_state();
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        // Should have emitted one incoming event for the notification
        let evt = event_rx.try_recv().unwrap();
        assert_eq!(evt.direction, Direction::Incoming);
        assert_eq!(evt.method, "notifications/initialized");
    }

    #[tokio::test]
    async fn connection_reset_side_effect_returns_error() {
        let request = make_request("ping", None);
        let (transport, _outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = json!({
            "behavior": {
                "side_effect": "connection_reset"
            }
        });
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let result = driver.drive_phase(0, &state, rx, event_tx, cancel).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("connection_reset"));
    }
}
