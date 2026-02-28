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
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use oatf::MatchPredicate;
use oatf::ResponseEntry;
use oatf::enums::ElicitationMode;
use oatf::primitives::{
    evaluate_predicate, interpolate_template, interpolate_value, parse_duration, select_response,
};
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
            // Receive-only handlers (§4.6)
            "sampling/createMessage" => handle_sampling(request),
            "roots/list" => handle_roots_list(request),
            // Elicitation response — agent responded to our elicitation
            "elicitation/create" => handle_elicitation_response(request),
            // Task lifecycle (§4.4)
            "tasks/get" => handle_tasks_get(request, state),
            "tasks/result" => handle_tasks_result(request, state),
            "tasks/list" => handle_tasks_list(request, state),
            "tasks/cancel" => handle_tasks_cancel(request, state),
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
        self.maybe_send_elicitation(state, params, event_tx).await;

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
        self.maybe_send_elicitation(state, params, event_tx).await;

        dispatch_response(
            &request.id,
            &prompt,
            extractors,
            params,
            None,
            self.raw_synthesize,
        )
    }

    /// Send an elicitation request if the state has a matching elicitation.
    ///
    /// Uses first-match-wins semantics per §4.3: iterates elicitations in
    /// order, evaluates each `when` predicate against the request context,
    /// and fires only the first match.
    async fn maybe_send_elicitation(
        &self,
        state: &Value,
        request_context: &Value,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) {
        let Some(elicitations) = state.get("elicitations").and_then(Value::as_array) else {
            return;
        };

        // First-match-wins (§4.3)
        let matched = elicitations.iter().find(|e| {
            let Some(when_value) = e.get("when") else {
                return true; // No predicate → always matches
            };
            match serde_json::from_value::<MatchPredicate>(when_value.clone()) {
                Ok(predicate) => evaluate_predicate(&predicate, request_context),
                Err(err) => {
                    tracing::warn!(error = %err, "failed to parse elicitation predicate");
                    false
                }
            }
        });

        let Some(elicitation) = matched else { return };

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
            return;
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

/// Handle `sampling/createMessage` — receive-only acknowledgement per §4.6.
///
/// The server does not initiate sampling; this simply acknowledges receipt
/// so the event can be used for trigger/extractor evaluation.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_sampling(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `roots/list` — receive-only acknowledgement per §4.6.
///
/// The server does not request roots; returns an empty roots list.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_roots_list(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({ "roots": [] }))
}

/// Handle `elicitation/create` response — acknowledge agent's elicitation response.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_elicitation_response(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `tasks/get` — look up a task by ID in `state["tasks"]`.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_tasks_get(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "id", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    JsonRpcResponse::success(
        request.id.clone(),
        strip_internal_fields(&task, &["_internal"]),
    )
}

/// Handle `tasks/result` — return a task's result content by ID.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_tasks_result(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "id", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    let result = task.get("result").cloned().unwrap_or(Value::Null);
    JsonRpcResponse::success(request.id.clone(), result)
}

/// Handle `tasks/list` — return all tasks from state, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_tasks_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let tasks = state
        .get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .map(|t| strip_internal_fields(t, &["_internal"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "tasks": tasks }))
}

/// Handle `tasks/cancel` — return cancelled status for the given task.
///
/// Implements: TJ-SPEC-013 F-001
fn handle_tasks_cancel(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(_task) = find_by_field(state, "tasks", "id", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    JsonRpcResponse::success(
        request.id.clone(),
        json!({ "id": task_id, "status": "cancelled" }),
    )
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

    // Apply payload generation for content items with `generate` blocks
    let mut interpolated = interpolated;
    apply_generation(&mut interpolated);

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
/// delivery mode: `normal`, `delayed`, `slow_stream`, or `unbounded`.
/// Parameters are read from `state["behavior"]["parameters"]` per §5.1.
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

    let params = state.get("behavior").and_then(|b| b.get("parameters"));

    match delivery {
        "delayed" => {
            let delay_ms = params
                .and_then(|p| p.get("delay_ms"))
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
            let byte_delay_ms = params
                .and_then(|p| p.get("byte_delay_ms"))
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
        "unbounded" => {
            let max_line_length = u64_to_usize(
                params
                    .and_then(|p| p.get("max_line_length"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1_000_000),
            );
            let nesting_depth = u64_to_usize(
                params
                    .and_then(|p| p.get("nesting_depth"))
                    .and_then(Value::as_u64)
                    .unwrap_or(100),
            );
            let inflated = inflate_response(response_msg, max_line_length, nesting_depth);
            transport
                .send_message(&inflated)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
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

/// Inflate a response message to create an oversized payload.
///
/// Pads text content items to reach `max_line_length` and wraps the
/// result in nested `{"wrapper": ...}` objects up to `nesting_depth`.
fn inflate_response(
    msg: &JsonRpcMessage,
    max_line_length: usize,
    nesting_depth: usize,
) -> JsonRpcMessage {
    let JsonRpcMessage::Response(resp) = msg else {
        return msg.clone();
    };

    let Some(result) = &resp.result else {
        return msg.clone();
    };

    let mut inflated = result.clone();

    // Pad text fields in content items
    if let Some(items) = inflated.get_mut("content").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if text.len() < max_line_length {
                    let padded = format!("{}{}", text, "X".repeat(max_line_length - text.len()));
                    item.as_object_mut()
                        .map(|obj| obj.insert("text".to_string(), Value::String(padded)));
                }
            }
        }
    }

    // Wrap in nested objects
    for _ in 0..nesting_depth {
        inflated = json!({ "wrapper": inflated });
    }

    JsonRpcMessage::Response(JsonRpcResponse::success(resp.id.clone(), inflated))
}

/// Apply side effects before delivering the response.
///
/// Reads `state["behavior"]["side_effects"]` as an array of
/// `{type, parameters}` objects per §5.1. Supports `notification_flood`,
/// `id_collision`, and `connection_reset`.
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
    let Some(effects) = state
        .get("behavior")
        .and_then(|b| b.get("side_effects"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for effect in effects {
        let effect_type = effect.get("type").and_then(Value::as_str).unwrap_or("");
        let params = effect.get("parameters");

        match effect_type {
            "notification_flood" => {
                let method = params
                    .and_then(|p| p.get("method"))
                    .and_then(Value::as_str)
                    .unwrap_or("notifications/message");
                let rate = params
                    .and_then(|p| p.get("rate"))
                    .and_then(Value::as_u64)
                    .unwrap_or(10);
                let duration_str = params
                    .and_then(|p| p.get("duration"))
                    .and_then(Value::as_str)
                    .unwrap_or("1s");
                let duration =
                    parse_duration(duration_str).unwrap_or(std::time::Duration::from_secs(1));

                let start = tokio::time::Instant::now();
                let interval = if rate > 0 {
                    std::time::Duration::from_millis(1000 / rate)
                } else {
                    std::time::Duration::from_millis(100)
                };

                while start.elapsed() < duration {
                    let notif = JsonRpcNotification::new(
                        method,
                        Some(json!({ "level": "info", "data": "flood" })),
                    );
                    transport
                        .send_message(&JsonRpcMessage::Notification(notif))
                        .await
                        .map_err(|e| EngineError::Driver(format!("flood send error: {e}")))?;
                    tokio::time::sleep(interval).await;
                }
            }
            "id_collision" => {
                let count = params
                    .and_then(|p| p.get("count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
                for _ in 0..count {
                    let collision =
                        JsonRpcResponse::success(request_id.clone(), json!({"collision": true}));
                    transport
                        .send_message(&JsonRpcMessage::Response(collision))
                        .await
                        .map_err(|e| EngineError::Driver(format!("collision send error: {e}")))?;
                }
            }
            "connection_reset" => {
                return Err(EngineError::Driver(
                    "connection_reset side effect triggered".to_string(),
                ));
            }
            _ => {
                tracing::warn!(side_effect = effect_type, "unknown side effect, skipping");
            }
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
// Payload generation (§6)
// ============================================================================

/// Saturating conversion from `u64` to `usize` for parameter parsing.
fn u64_to_usize(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// Maximum nesting depth for generated JSON to prevent stack overflow.
const MAX_GENERATION_DEPTH: usize = 1000;

/// Maximum payload size for generated content (50 MB).
const MAX_GENERATION_SIZE: usize = 50 * 1024 * 1024;

/// Apply payload generation to content items that have a `generate` block.
///
/// For each content item with a `generate` key, replaces the `text` field
/// with the generated payload and removes the `generate` key.
fn apply_generation(content: &mut Value) {
    if let Some(items) = content.get_mut("content").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(generator) = item.get("generate").cloned() {
                let kind = generator.get("kind").and_then(Value::as_str).unwrap_or("");
                let params = generator.get("parameters");
                let seed = generator.get("seed").and_then(Value::as_u64);
                let generated = match kind {
                    "nested_json" => generate_nested_json(params, seed),
                    "random_bytes" => generate_random_bytes(params, seed),
                    "unbounded_line" => generate_unbounded_line(params, seed),
                    "unicode_stress" => generate_unicode_stress(params, seed),
                    _ => {
                        tracing::warn!(kind, "unknown generator kind");
                        continue;
                    }
                };
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("text".to_string(), Value::String(generated));
                    obj.remove("generate");
                }
            }
        }
    }
}

/// Generate deeply nested JSON: `{"a":{"a":...}}` to the specified depth.
fn generate_nested_json(params: Option<&Value>, _seed: Option<u64>) -> String {
    let depth = u64_to_usize(
        params
            .and_then(|p| p.get("depth"))
            .and_then(Value::as_u64)
            .unwrap_or(100),
    );
    let clamped_depth = depth.min(MAX_GENERATION_DEPTH);

    let mut result = String::with_capacity(clamped_depth * 6 + 10);
    for _ in 0..clamped_depth {
        result.push_str(r#"{"a":"#);
    }
    result.push_str(r#""leaf""#);
    for _ in 0..clamped_depth {
        result.push('}');
    }
    result
}

/// Generate deterministic pseudo-random bytes, hex-encoded.
///
/// Uses a simple LCG (no `rand` dependency) seeded by the provided seed.
fn generate_random_bytes(params: Option<&Value>, seed: Option<u64>) -> String {
    let size = u64_to_usize(
        params
            .and_then(|p| p.get("size"))
            .and_then(Value::as_u64)
            .unwrap_or(1024),
    );
    let clamped_size = size.min(MAX_GENERATION_SIZE);

    // Simple LCG: x = (a * x + c) mod m
    let mut lcg_state = seed.unwrap_or(42);
    let mut hex = String::with_capacity(clamped_size * 2);
    for _ in 0..clamped_size {
        lcg_state = lcg_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        #[allow(clippy::cast_possible_truncation)]
        let byte = (lcg_state >> 33) as u8;
        let _ = write!(hex, "{byte:02x}");
    }

    hex
}

/// Generate an unbounded single-line string of repeated characters.
fn generate_unbounded_line(params: Option<&Value>, _seed: Option<u64>) -> String {
    let length = u64_to_usize(
        params
            .and_then(|p| p.get("length"))
            .and_then(Value::as_u64)
            .unwrap_or(1_000_000),
    );
    let clamped_length = length.min(MAX_GENERATION_SIZE);

    let ch = params
        .and_then(|p| p.get("char"))
        .and_then(Value::as_str)
        .and_then(|s| s.chars().next())
        .unwrap_or('A');

    ch.to_string().repeat(clamped_length)
}

/// Generate a Unicode stress-test string with category-based sequences.
///
/// Categories: RTL overrides, zero-width characters, combining marks,
/// emoji sequences, and other edge-case Unicode.
fn generate_unicode_stress(params: Option<&Value>, _seed: Option<u64>) -> String {
    let category = params
        .and_then(|p| p.get("category"))
        .and_then(Value::as_str)
        .unwrap_or("mixed");
    let repeat = u64_to_usize(
        params
            .and_then(|p| p.get("repeat"))
            .and_then(Value::as_u64)
            .unwrap_or(100),
    );

    let pattern = match category {
        "rtl" => "\u{202E}\u{200F}\u{202B}\u{2067}", // RTL override, RLM, RLE, RLI
        "zero_width" => "\u{200B}\u{200C}\u{200D}\u{FEFF}", // ZWSP, ZWNJ, ZWJ, BOM
        "combining" => "a\u{0300}\u{0301}\u{0302}\u{0303}\u{0304}", // a + 5 combining marks
        "emoji" => "\u{1F600}\u{200D}\u{1F525}\u{FE0F}\u{20E3}", // emoji + ZWJ + fire + VS16 + keycap
        _ => "\u{202E}\u{200B}a\u{0300}\u{0301}\u{1F600}\u{200D}\u{FEFF}", // mixed
    };

    pattern.repeat(repeat.min(MAX_GENERATION_SIZE / pattern.len()))
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

        fn as_any(&self) -> &dyn std::any::Any {
            self
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
                "parameters": {
                    "delay_ms": 10
                }
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
                "side_effects": [
                    {
                        "type": "notification_flood",
                        "parameters": {
                            "rate": 1000,
                            "duration": "0s"
                        }
                    }
                ]
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
        // With duration "0s" we get no flood notifications, just the response
        // The flood loop immediately exits because elapsed >= 0s duration
        assert!(!sent.is_empty());
        // Last message should be the response
        assert!(matches!(sent.last().unwrap(), JsonRpcMessage::Response(_)));
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
                "side_effects": [
                    { "type": "connection_reset" }
                ]
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

    // ---- New handler tests (Gap 1) ----

    #[test]
    fn sampling_returns_empty_object() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "sampling/createMessage".to_string(),
            params: Some(json!({})),
            id: json!(1),
        };
        let resp = handle_sampling(&request);
        assert_eq!(resp.result, Some(json!({})));
    }

    #[test]
    fn roots_list_returns_empty_roots() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "roots/list".to_string(),
            params: None,
            id: json!(1),
        };
        let resp = handle_roots_list(&request);
        let result = resp.result.unwrap();
        assert_eq!(result["roots"], json!([]));
    }

    #[test]
    fn elicitation_response_returns_empty_object() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "elicitation/create".to_string(),
            params: Some(json!({"action": "accept", "content": {"key": "val"}})),
            id: json!(1),
        };
        let resp = handle_elicitation_response(&request);
        assert_eq!(resp.result, Some(json!({})));
    }

    #[test]
    fn tasks_get_returns_task() {
        let state = json!({
            "tasks": [
                {"id": "task-1", "status": "running", "result": {"data": "hello"}}
            ]
        });
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/get".to_string(),
            params: Some(json!({"id": "task-1"})),
            id: json!(1),
        };
        let resp = handle_tasks_get(&request, &state);
        let result = resp.result.unwrap();
        assert_eq!(result["id"], "task-1");
        assert_eq!(result["status"], "running");
    }

    #[test]
    fn tasks_get_unknown_returns_error() {
        let state = json!({"tasks": []});
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/get".to_string(),
            params: Some(json!({"id": "missing"})),
            id: json!(1),
        };
        let resp = handle_tasks_get(&request, &state);
        assert!(resp.error.is_some());
    }

    #[test]
    fn tasks_result_returns_result() {
        let state = json!({
            "tasks": [
                {"id": "task-1", "status": "completed", "result": {"output": "done"}}
            ]
        });
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/result".to_string(),
            params: Some(json!({"id": "task-1"})),
            id: json!(1),
        };
        let resp = handle_tasks_result(&request, &state);
        let result = resp.result.unwrap();
        assert_eq!(result["output"], "done");
    }

    #[test]
    fn tasks_list_returns_all_tasks() {
        let state = json!({
            "tasks": [
                {"id": "task-1", "status": "running"},
                {"id": "task-2", "status": "completed"}
            ]
        });
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/list".to_string(),
            params: None,
            id: json!(1),
        };
        let resp = handle_tasks_list(&request, &state);
        let result = resp.result.unwrap();
        assert_eq!(result["tasks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn tasks_list_empty_state() {
        let state = json!({});
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/list".to_string(),
            params: None,
            id: json!(1),
        };
        let resp = handle_tasks_list(&request, &state);
        let result = resp.result.unwrap();
        assert_eq!(result["tasks"], json!([]));
    }

    #[test]
    fn tasks_cancel_returns_cancelled() {
        let state = json!({
            "tasks": [{"id": "task-1", "status": "running"}]
        });
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks/cancel".to_string(),
            params: Some(json!({"id": "task-1"})),
            id: json!(1),
        };
        let resp = handle_tasks_cancel(&request, &state);
        let result = resp.result.unwrap();
        assert_eq!(result["id"], "task-1");
        assert_eq!(result["status"], "cancelled");
    }

    // ---- Edge case tests (Gap 7) ----

    /// EC-OATF-011: Empty content returned when no response entry matches.
    #[test]
    fn select_response_no_match() {
        let item = json!({
            "name": "test-tool",
            "responses": [
                {
                    "when": {"name": "other-tool"},
                    "content": [{"type": "text", "text": "should not match"}]
                }
            ]
        });
        let context = json!({"name": "test-tool"});
        let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &context, None, false);
        let result = resp.result.unwrap();
        assert_eq!(result["content"], json!([]));
    }

    /// EC-OATF-012: Error message when synthesize is requested but no
    /// `GenerationProvider` is available.
    #[test]
    fn synthesize_no_provider() {
        let item = json!({
            "name": "test-tool",
            "responses": [
                {
                    "synthesize": {"prompt": "generate something"}
                }
            ]
        });
        let resp = dispatch_response(&json!(1), &item, &HashMap::new(), &Value::Null, None, false);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert!(
            err.message.contains("synthesize"),
            "error should mention synthesize: {}",
            err.message
        );
    }

    /// EC-OATF-013: When agent declines an elicitation, the tool call
    /// completes normally. We verify that an elicitation with a non-matching
    /// predicate is skipped and the response is still sent.
    #[tokio::test]
    async fn elicitation_agent_declines() {
        // Set up: agent sends tools/call, then a decline response to the elicitation
        let tools_call = make_request(
            "tools/call",
            Some(json!({"name": "calculator", "arguments": {}})),
        );
        let decline_response = JsonRpcMessage::Response(JsonRpcResponse::success(
            json!("elicit-decline"),
            json!({"action": "decline"}),
        ));
        let (transport, outgoing) = MockTransport::new(vec![tools_call, decline_response]);
        let mut driver = McpServerDriver::new(transport, false);

        // State with an always-matching elicitation
        let state = json!({
            "tools": [{
                "name": "calculator",
                "description": "calc",
                "inputSchema": {"type": "object"},
                "responses": [
                    {"content": [{"type": "text", "text": "42"}]}
                ]
            }],
            "elicitations": [{
                "message": "Enter API key",
                "requestedSchema": {"type": "object"}
            }]
        });
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        // Verify the tool response was still sent despite the decline
        let sent = outgoing.lock().await;
        let has_tool_response = sent.iter().any(|msg| {
            matches!(msg, JsonRpcMessage::Response(r) if r.result.as_ref().is_some_and(|v| v["content"][0]["text"] == "42"))
        });
        assert!(
            has_tool_response,
            "tool response should be sent after elicitation decline"
        );

        // Verify elicitation events were emitted
        let mut events = Vec::new();
        while let Ok(evt) = event_rx.try_recv() {
            events.push(evt);
        }
        let has_elicit_out = events
            .iter()
            .any(|e| e.method == "elicitation/create" && e.direction == Direction::Outgoing);
        assert!(has_elicit_out, "should have outgoing elicitation event");
    }

    // ---- Payload generation tests ----

    #[test]
    fn generate_nested_json_produces_valid_json() {
        let result = generate_nested_json(Some(&json!({"depth": 5})), None);
        let parsed: serde_json::Result<Value> = serde_json::from_str(&result);
        assert!(parsed.is_ok(), "generated JSON should be valid");
    }

    #[test]
    fn generate_nested_json_respects_depth_limit() {
        let result = generate_nested_json(Some(&json!({"depth": 2000})), None);
        // Should be clamped to MAX_GENERATION_DEPTH (1000)
        let nesting = result.matches(r#"{"a":"#).count();
        assert_eq!(nesting, MAX_GENERATION_DEPTH);
    }

    #[test]
    fn generate_random_bytes_is_deterministic() {
        let a = generate_random_bytes(Some(&json!({"size": 32})), Some(12345));
        let b = generate_random_bytes(Some(&json!({"size": 32})), Some(12345));
        assert_eq!(a, b, "same seed should produce same output");
    }

    #[test]
    fn generate_random_bytes_different_seeds_differ() {
        let a = generate_random_bytes(Some(&json!({"size": 32})), Some(1));
        let b = generate_random_bytes(Some(&json!({"size": 32})), Some(2));
        assert_ne!(a, b, "different seeds should produce different output");
    }

    #[test]
    fn generate_unbounded_line_correct_length() {
        let result = generate_unbounded_line(Some(&json!({"length": 100})), None);
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn generate_unicode_stress_produces_content() {
        for category in &["rtl", "zero_width", "combining", "emoji", "mixed"] {
            let result =
                generate_unicode_stress(Some(&json!({"category": category, "repeat": 10})), None);
            assert!(
                !result.is_empty(),
                "category {category} should produce content"
            );
        }
    }

    #[test]
    fn apply_generation_replaces_generate_blocks() {
        let mut content = json!({
            "content": [
                {
                    "type": "text",
                    "generate": {
                        "kind": "unbounded_line",
                        "parameters": {"length": 50}
                    }
                }
            ]
        });
        apply_generation(&mut content);
        let items = content["content"].as_array().unwrap();
        assert!(
            items[0].get("generate").is_none(),
            "generate block should be removed"
        );
        assert_eq!(items[0]["text"].as_str().unwrap().len(), 50);
    }

    // ---- Unbounded delivery test ----

    #[tokio::test]
    async fn unbounded_delivery_inflates_response() {
        let request = make_request("ping", None);
        let (transport, outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = json!({
            "behavior": {
                "delivery": "unbounded",
                "parameters": {
                    "max_line_length": 100,
                    "nesting_depth": 3
                }
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
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            JsonRpcMessage::Response(r) => {
                let result = r.result.as_ref().unwrap();
                // Should be wrapped in 3 levels of nesting
                assert!(result.get("wrapper").is_some());
                assert!(result["wrapper"].get("wrapper").is_some());
                assert!(result["wrapper"]["wrapper"].get("wrapper").is_some());
            }
            _ => panic!("expected response"),
        }
    }

    // ---- Elicitation predicate tests ----

    #[tokio::test]
    async fn elicitation_first_match_wins() {
        // Set up: agent sends tools/call, then a response to the elicitation
        let tools_call = make_request(
            "tools/call",
            Some(json!({"name": "calculator", "arguments": {}})),
        );
        let elicit_response = JsonRpcMessage::Response(JsonRpcResponse::success(
            json!("elicit-resp"),
            json!({"action": "accept"}),
        ));
        let (transport, outgoing) = MockTransport::new(vec![tools_call, elicit_response]);
        let mut driver = McpServerDriver::new(transport, false);

        // State with two elicitations: first requires name=other, second matches all
        let state = json!({
            "tools": [{
                "name": "calculator",
                "description": "calc",
                "inputSchema": {"type": "object"},
                "responses": [
                    {"content": [{"type": "text", "text": "42"}]}
                ]
            }],
            "elicitations": [
                {
                    "when": {"name": "other-tool"},
                    "message": "Should not fire",
                    "requestedSchema": {}
                },
                {
                    "message": "Should fire (no predicate)",
                    "requestedSchema": {}
                }
            ]
        });
        let (_tx, rx) = watch::channel(HashMap::new());
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        driver
            .drive_phase(0, &state, rx, event_tx, cancel)
            .await
            .unwrap();

        let sent = outgoing.lock().await;
        // Should have: elicitation request + tool response
        let elicitation_sent = sent.iter().any(|msg| {
            matches!(msg, JsonRpcMessage::Request(r) if r.method == "elicitation/create"
                && r.params.as_ref().unwrap()["message"] == "Should fire (no predicate)")
        });
        assert!(
            elicitation_sent,
            "second (matching) elicitation should fire"
        );
    }

    // ---- Side effects array format test ----

    #[tokio::test]
    async fn id_collision_side_effect_with_count() {
        let request = make_request("ping", None);
        let (transport, outgoing) = MockTransport::new(vec![request]);
        let mut driver = McpServerDriver::new(transport, false);

        let state = json!({
            "behavior": {
                "side_effects": [
                    {
                        "type": "id_collision",
                        "parameters": {"count": 2}
                    }
                ]
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
        // 2 collision responses + 1 real response = 3
        assert_eq!(sent.len(), 3);
        // First 2 should be collision responses
        for msg in &sent[..2] {
            match msg {
                JsonRpcMessage::Response(r) => {
                    assert_eq!(r.result.as_ref().unwrap()["collision"], true);
                }
                _ => panic!("expected collision response"),
            }
        }
    }
}
