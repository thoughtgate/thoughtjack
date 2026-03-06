use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use oatf::MatchPredicate;
use oatf::enums::ElicitationMode;
use oatf::primitives::{evaluate_predicate, interpolate_template};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;
use crate::transport::HttpTransport;
use crate::transport::{
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, Transport, TransportType,
};

use super::behavior::{apply_delivery, apply_side_effects};
use super::handlers::{
    handle_completion, handle_elicitation_response, handle_initialize, handle_logging_set_level,
    handle_ping, handle_prompts_list, handle_resources_list, handle_resources_read,
    handle_resources_templates_list, handle_roots_list, handle_sampling, handle_subscribe,
    handle_tasks_cancel, handle_tasks_get, handle_tasks_list, handle_tasks_result,
    handle_tools_list, handle_unknown,
};
use super::helpers::find_by_name;
use super::response::dispatch_response;

use crate::engine::actions::EntryActionSender;
use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};

/// Maximum number of deferred client requests buffered during slow-stream delivery.
///
/// A malicious client can intentionally keep a slow-stream window open and pile
/// up requests. This cap prevents unbounded memory growth; excess requests are
/// dropped with a warning.
const MAX_DEFERRED_REQUESTS: usize = 1_000;

/// Maximum number of buffered responses during server-initiated request waits.
///
/// Bounds memory used when a server-initiated request (sampling, elicitation)
/// receives interleaved responses with non-matching IDs.
const MAX_BUFFERED_RESPONSES: usize = 100;

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
    /// Client capabilities captured from the `initialize` request.
    /// Used to gate elicitation and sampling requests per MCP spec.
    client_capabilities: Option<Value>,
    /// Client requests received while a slow-stream response is still in flight.
    deferred_requests: VecDeque<JsonRpcRequest>,
    /// Responses received while waiting for a specific server-initiated request.
    buffered_server_responses: HashMap<String, JsonRpcResponse>,
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
            client_capabilities: None,
            deferred_requests: VecDeque::new(),
            buffered_server_responses: HashMap::new(),
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

    /// Observe interleaved messages while a spawned delivery task runs.
    ///
    /// Re-emits incoming notifications and requests as protocol events so
    /// that the `PhaseLoop` can evaluate triggers and capture extractors
    /// even while a `slow_stream` delivery is dripping bytes.
    ///
    /// Returns `Some(DriveResult::Complete)` when the driver should exit
    /// early (cancellation or EOF), or `None` when delivery finished
    /// normally and the caller should continue.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on transport or delivery task failure.
    async fn observe_during_delivery(
        &mut self,
        mut handle: JoinHandle<Result<(), EngineError>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> Result<Option<DriveResult>, EngineError> {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    handle.abort();
                    return Ok(Some(DriveResult::Complete));
                }
                result = &mut handle => {
                    return match result {
                        Ok(Ok(())) => Ok(None),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(EngineError::Driver(
                            format!("delivery task panicked: {e}")
                        )),
                    };
                }
                msg = self.transport.receive_message() => {
                    match msg {
                        Ok(Some(JsonRpcMessage::Notification(notif))) => {
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: notif.method.clone(),
                                content: notif.params.unwrap_or(Value::Null),
                            }).await;
                        }
                        Ok(Some(JsonRpcMessage::Request(req))) => {
                            if self.deferred_requests.len() < MAX_DEFERRED_REQUESTS {
                                self.deferred_requests.push_back(req);
                            } else {
                                tracing::warn!(method = %req.method, "deferred request queue full, dropping");
                            }
                        }
                        Ok(Some(JsonRpcMessage::Response(resp))) => {
                            tracing::debug!(id = ?resp.id, "unexpected response from agent during delivery");
                        }
                        Ok(None) => {
                            handle.abort();
                            return Ok(Some(DriveResult::TransportClosed));
                        }
                        Err(err) => {
                            handle.abort();
                            return Err(EngineError::Driver(
                                format!("transport error during delivery: {err}")
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Dispatch a request to the appropriate handler.
    ///
    /// For `initialize`, captures client capabilities from the request
    /// params so that subsequent elicitation and sampling requests can
    /// be gated on client support (§4.3).
    async fn dispatch_request(
        &mut self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => {
                // Capture client capabilities for capability gating (§4.3)
                if let Some(params) = &request.params {
                    self.client_capabilities = params.get("capabilities").cloned();
                }
                handle_initialize(request, state)
            }
            "tools/list" => handle_tools_list(request, state),
            "tools/call" => {
                self.handle_tools_call(request, state, extractors, event_tx, cancel)
                    .await
            }
            "resources/list" => handle_resources_list(request, state),
            "resources/read" => {
                handle_resources_read(request, state, extractors, self.raw_synthesize)
            }
            "resources/templates/list" => handle_resources_templates_list(request, state),
            "prompts/list" => handle_prompts_list(request, state),
            "prompts/get" => {
                self.handle_prompts_get(request, state, extractors, event_tx, cancel)
                    .await
            }
            "resources/subscribe" | "resources/unsubscribe" => handle_subscribe(request),
            "completion/complete" => handle_completion(request),
            "logging/setLevel" => handle_logging_set_level(request),
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
        &mut self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> JsonRpcResponse {
        let params = request.params.as_ref().unwrap_or(&Value::Null);
        let tool_name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let Some(tool) = find_by_name(state, "tools", tool_name) else {
            return JsonRpcResponse::error(
                request.id.clone(),
                crate::transport::jsonrpc::error_codes::INVALID_PARAMS,
                format!("tool not found: {tool_name}"),
            );
        };

        // Interleaving: logging → progress → sampling → elicitation → response
        self.maybe_send_logging(state, tool_name, event_tx).await;
        self.maybe_send_progress(state, params, tool_name, event_tx)
            .await;
        self.maybe_send_sampling(state, tool_name, event_tx, cancel)
            .await;
        self.maybe_send_elicitation(state, params, tool_name, event_tx, cancel)
            .await;

        dispatch_response(
            &request.id,
            &tool,
            extractors,
            params,
            tool.get("outputSchema"),
            self.raw_synthesize,
            "tools/call",
        )
    }

    /// Handle `prompts/get` with response dispatch and optional elicitation.
    async fn handle_prompts_get(
        &mut self,
        request: &JsonRpcRequest,
        state: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> JsonRpcResponse {
        let params = request.params.as_ref().unwrap_or(&Value::Null);
        let prompt_name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let Some(prompt) = find_by_name(state, "prompts", prompt_name) else {
            return JsonRpcResponse::error(
                request.id.clone(),
                crate::transport::jsonrpc::error_codes::INVALID_PARAMS,
                format!("prompt not found: {prompt_name}"),
            );
        };

        // Elicitation interleaving (before response)
        self.maybe_send_elicitation(state, params, "", event_tx, cancel)
            .await;

        dispatch_response(
            &request.id,
            &prompt,
            extractors,
            params,
            None,
            self.raw_synthesize,
            "prompts/get",
        )
    }

    /// Send a server-initiated JSON-RPC request and wait for the response.
    ///
    /// On HTTP, uses `HttpTransport::send_server_request()` to avoid the
    /// channel-swap bug. On stdio, falls back to `send_message()` +
    /// `receive_message()`, looping past any interleaved notifications or
    /// requests that the client may send before its response (e.g.
    /// `notifications/progress`, `notifications/cancelled`). Interleaved
    /// messages are re-emitted on `event_tx` so they are not lost.
    #[allow(clippy::cognitive_complexity)] // transport branching + select loop is inherently complex
    async fn send_server_request(
        &mut self,
        request: &JsonRpcRequest,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> Option<JsonRpcResponse> {
        if self.transport.transport_type() == TransportType::Http
            && let Some(http) = self.transport.as_any().downcast_ref::<HttpTransport>()
        {
            match http.send_server_request(request).await {
                Ok(resp) => return Some(resp),
                Err(err) => {
                    tracing::warn!(error = %err, "HTTP server request failed");
                    return None;
                }
            }
        }

        // Stdio fallback: send + receive, skipping interleaved messages
        if let Err(err) = self
            .transport
            .send_message(&JsonRpcMessage::Request(request.clone()))
            .await
        {
            tracing::warn!(error = %err, "failed to send server request");
            return None;
        }

        let expected_id =
            serde_json::to_string(&request.id).unwrap_or_else(|_| request.id.to_string());
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

        loop {
            if let Some(resp) = self.buffered_server_responses.remove(&expected_id) {
                return Some(resp);
            }

            let receive = tokio::select! {
                biased;
                () = cancel.cancelled() => return None,
                () = tokio::time::sleep_until(deadline) => {
                    tracing::warn!(method = %request.method, "server request timed out waiting for client response");
                    return None;
                }
                msg = self.transport.receive_message() => msg,
            };

            match receive {
                Ok(Some(JsonRpcMessage::Response(resp))) => {
                    let response_id =
                        serde_json::to_string(&resp.id).unwrap_or_else(|_| resp.id.to_string());
                    if response_id == expected_id {
                        return Some(resp);
                    }
                    if self.buffered_server_responses.len() < MAX_BUFFERED_RESPONSES {
                        tracing::debug!(?resp.id, expected = %expected_id, "buffering unrelated response while awaiting server request");
                        self.buffered_server_responses.insert(response_id, resp);
                    } else {
                        tracing::warn!(?resp.id, "buffered response map full, dropping");
                    }
                }
                Ok(Some(JsonRpcMessage::Notification(notif))) => {
                    tracing::debug!(
                        method = %notif.method,
                        "interleaved notification during server request wait, re-emitting"
                    );
                    let _ = event_tx
                        .send(ProtocolEvent {
                            direction: Direction::Incoming,
                            method: notif.method.clone(),
                            content: notif.params.unwrap_or(Value::Null),
                        })
                        .await;
                }
                Ok(Some(JsonRpcMessage::Request(req))) => {
                    if self.deferred_requests.len() < MAX_DEFERRED_REQUESTS {
                        tracing::debug!(
                            method = %req.method,
                            "interleaved request during server request wait, queueing for later dispatch"
                        );
                        self.deferred_requests.push_back(req);
                    } else {
                        tracing::warn!(method = %req.method, "deferred request queue full, dropping");
                    }
                }
                Ok(None) => {
                    tracing::debug!("transport EOF during server request wait");
                    return None;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "transport error during server request wait");
                    return None;
                }
            }
        }
    }

    /// Send logging notifications if the state has matching logging entries.
    ///
    /// Reads `state["logging"]` array, sends `notifications/message` for each.
    /// Optional `tool` field restricts to specific tool names.
    async fn maybe_send_logging(
        &self,
        state: &Value,
        tool_name: &str,
        event_tx: &mpsc::Sender<ProtocolEvent>,
    ) {
        let Some(entries) = state.get("logging").and_then(Value::as_array) else {
            return;
        };

        for entry in entries {
            // Check tool filter
            if let Some(tool_filter) = entry.get("tool").and_then(Value::as_str)
                && tool_filter != tool_name
            {
                continue;
            }

            let level = entry.get("level").and_then(Value::as_str).unwrap_or("info");
            let data = entry
                .get("data")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));

            let notif = JsonRpcNotification::new(
                "notifications/message",
                Some(json!({
                    "level": level,
                    "data": data,
                })),
            );

            let _ = event_tx
                .send(ProtocolEvent {
                    direction: Direction::Outgoing,
                    method: "notifications/message".to_string(),
                    content: json!({ "level": level, "data": data }),
                })
                .await;

            if let Err(err) = self
                .transport
                .send_message(&JsonRpcMessage::Notification(notif))
                .await
            {
                tracing::warn!(error = %err, "failed to send logging notification");
            }
        }
    }

    /// Send progress notifications if the request includes a progress token.
    ///
    /// Reads `state["progress"]` array. Only fires if the request has
    /// `_meta.progressToken` in params.
    async fn maybe_send_progress(
        &self,
        state: &Value,
        request_params: &Value,
        tool_name: &str,
        event_tx: &mpsc::Sender<ProtocolEvent>,
    ) {
        let Some(entries) = state.get("progress").and_then(Value::as_array) else {
            return;
        };

        let Some(progress_token) = request_params
            .get("_meta")
            .and_then(|m| m.get("progressToken"))
        else {
            return;
        };

        for entry in entries {
            // Check tool filter
            if let Some(tool_filter) = entry.get("tool").and_then(Value::as_str)
                && tool_filter != tool_name
            {
                continue;
            }

            let progress = entry.get("progress").and_then(Value::as_u64).unwrap_or(0);
            let total = entry.get("total").and_then(Value::as_u64);

            let mut notif_params = json!({
                "progressToken": progress_token.clone(),
                "progress": progress,
            });
            if let Some(total_val) = total {
                notif_params
                    .as_object_mut()
                    .unwrap()
                    .insert("total".to_string(), json!(total_val));
            }

            let notif =
                JsonRpcNotification::new("notifications/progress", Some(notif_params.clone()));

            let _ = event_tx
                .send(ProtocolEvent {
                    direction: Direction::Outgoing,
                    method: "notifications/progress".to_string(),
                    content: notif_params,
                })
                .await;

            if let Err(err) = self
                .transport
                .send_message(&JsonRpcMessage::Notification(notif))
                .await
            {
                tracing::warn!(error = %err, "failed to send progress notification");
            }
        }
    }

    /// Check whether the client declared support for a capability.
    ///
    /// Checks `client_capabilities.<name>` — returns `true` if the field
    /// exists (even if empty object), `false` if absent or no capabilities
    /// were captured (i.e., `initialize` hasn't been called yet).
    fn client_supports(&self, capability: &str) -> bool {
        self.client_capabilities
            .as_ref()
            .is_some_and(|caps| caps.get(capability).is_some())
    }

    /// Send a sampling request if the state has matching sampling entries.
    ///
    /// Gated on `client_capabilities.sampling` per MCP 2025-11-25 — the
    /// server MUST NOT send sampling requests to clients that do not
    /// declare sampling support.
    ///
    /// Reads `state["sampling_requests"]` array, sends `sampling/createMessage`
    /// request. Uses first-match-wins with optional `tool` filter.
    async fn maybe_send_sampling(
        &mut self,
        state: &Value,
        tool_name: &str,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) {
        if !self.client_supports("sampling") {
            if state.get("sampling_requests").is_some() {
                tracing::warn!(
                    "sampling_requests defined but client does not declare sampling capability"
                );
            }
            return;
        }

        let Some(entries) = state.get("sampling_requests").and_then(Value::as_array) else {
            return;
        };

        // First-match-wins with tool filter
        let matched = entries.iter().find(|e| {
            e.get("tool")
                .and_then(Value::as_str)
                .is_none_or(|tool_filter| tool_filter == tool_name)
        });

        let Some(entry) = matched else { return };

        let messages = entry.get("messages").cloned().unwrap_or_else(|| json!([]));
        let max_tokens = entry
            .get("maxTokens")
            .and_then(Value::as_u64)
            .unwrap_or(100);

        let sampling_request = JsonRpcRequest {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            method: "sampling/createMessage".to_string(),
            params: Some(json!({
                "messages": messages,
                "maxTokens": max_tokens,
            })),
            id: json!(format!(
                "sampling-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
        };

        let _ = event_tx
            .send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: "sampling/createMessage".to_string(),
                content: serde_json::to_value(&sampling_request).unwrap_or(Value::Null),
            })
            .await;

        if let Some(resp) = self
            .send_server_request(&sampling_request, event_tx, cancel)
            .await
        {
            let _ = event_tx
                .send(ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "sampling/createMessage".to_string(),
                    content: serde_json::to_value(&resp).unwrap_or(Value::Null),
                })
                .await;
        }
    }

    /// Send an elicitation request if the state has a matching elicitation.
    ///
    /// Gated on `client_capabilities.elicitation` per MCP 2025-11-25 §4.3 —
    /// the server MUST NOT send elicitation requests to clients that do not
    /// declare elicitation support.
    ///
    /// Uses first-match-wins semantics per §4.3: iterates elicitations in
    /// order, evaluates each `when` predicate against the request context,
    /// and fires only the first match.
    ///
    /// Includes `mode`, `url`, and `elicitationId` on the wire per MCP spec.
    /// Auto-generates a UUID for url-mode if `elicitationId` is absent in
    /// the OATF document.
    // Complexity: elicitation matching with mode/url/validation branching per MCP spec
    #[allow(clippy::cognitive_complexity)]
    async fn maybe_send_elicitation(
        &mut self,
        state: &Value,
        request_context: &Value,
        tool_name: &str,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) {
        if !self.client_supports("elicitation") {
            if state.get("elicitations").is_some() {
                tracing::warn!(
                    "elicitations defined but client does not declare elicitation capability"
                );
            }
            return;
        }

        let Some(elicitations) = state.get("elicitations").and_then(Value::as_array) else {
            return;
        };

        // First-match-wins (§4.3) with optional tool filter
        let matched = elicitations.iter().find(|e| {
            // Check tool filter first
            if let Some(tool_filter) = e.get("tool").and_then(Value::as_str)
                && tool_filter != tool_name
            {
                return false;
            }
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

        let mode = elicitation
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("form");

        // Build request params with mode, requestedSchema, and url/elicitationId
        let mut params = serde_json::Map::new();
        params.insert("message".to_string(), json!(message));
        params.insert(
            "requestedSchema".to_string(),
            elicitation
                .get("requestedSchema")
                .cloned()
                .unwrap_or(json!({})),
        );

        if mode == "url" {
            // URL-mode: include url and elicitationId
            if let Some(url) = elicitation.get("url").and_then(Value::as_str) {
                params.insert("url".to_string(), json!(url));
            }
            let elicitation_id = elicitation
                .get("elicitationId")
                .and_then(Value::as_str)
                .map_or_else(|| uuid::Uuid::new_v4().to_string(), ToString::to_string);
            params.insert("elicitationId".to_string(), json!(elicitation_id));
        }

        let elicitation_request = JsonRpcRequest {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            method: "elicitation/create".to_string(),
            params: Some(Value::Object(params)),
            id: json!(format!(
                "elicit-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
        };

        // Emit outgoing elicitation event
        let _ = event_tx
            .send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: "elicitation/create".to_string(),
                content: serde_json::to_value(&elicitation_request).unwrap_or(Value::Null),
            })
            .await;

        // Send request and wait for response
        if let Some(resp) = self
            .send_server_request(&elicitation_request, event_tx, cancel)
            .await
        {
            let _ = event_tx
                .send(ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "elicitation/create".to_string(),
                    content: serde_json::to_value(&resp).unwrap_or(Value::Null),
                })
                .await;
        }
    }

    async fn handle_request_message(
        &mut self,
        request: JsonRpcRequest,
        state: &Value,
        extractors: &watch::Receiver<HashMap<String, String>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        cancel: &CancellationToken,
    ) -> Result<Option<DriveResult>, EngineError> {
        let incoming_content = request.params.clone().unwrap_or(Value::Null);
        let _ = event_tx
            .send(ProtocolEvent {
                direction: Direction::Incoming,
                method: request.method.clone(),
                content: incoming_content,
            })
            .await;

        let current_extractors = extractors.borrow().clone();
        let response = self
            .dispatch_request(&request, state, &current_extractors, event_tx, cancel)
            .await;

        apply_side_effects(&self.transport, state, &request.id).await?;

        let response_msg = JsonRpcMessage::Response(response.clone());
        let pending = apply_delivery(&self.transport, state, &response_msg).await?;

        if let Some(handle) = pending
            && let Some(result) = self
                .observe_during_delivery(handle, event_tx, cancel)
                .await?
        {
            return Ok(Some(result));
        }

        let outgoing_content = response.result.clone().unwrap_or_else(|| {
            response.error.as_ref().map_or(Value::Null, |e| {
                serde_json::to_value(e).unwrap_or(Value::Null)
            })
        });
        let _ = event_tx
            .send(ProtocolEvent {
                direction: Direction::Outgoing,
                method: request.method.clone(),
                content: outgoing_content,
            })
            .await;

        self.transport
            .finalize_response()
            .await
            .map_err(|e| EngineError::Driver(format!("finalize error: {e}")))?;

        Ok(None)
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
        event_tx: mpsc::Sender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        loop {
            if let Some(request) = self.deferred_requests.pop_front() {
                if let Some(result) = self
                    .handle_request_message(request, state, &extractors, &event_tx, &cancel)
                    .await?
                {
                    return Ok(result);
                }
                continue;
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(DriveResult::Complete);
                }
                msg = self.transport.receive_message() => {
                    match msg {
                        Ok(Some(JsonRpcMessage::Request(request))) => {
                            if let Some(result) = self
                                .handle_request_message(request, state, &extractors, &event_tx, &cancel)
                                .await?
                            {
                                return Ok(result);
                            }
                        }
                        Ok(Some(JsonRpcMessage::Notification(notif))) => {
                            // Notifications have no response — just emit event
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method: notif.method.clone(),
                                content: notif.params.unwrap_or(Value::Null),
                            }).await;
                        }
                        Ok(Some(JsonRpcMessage::Response(resp))) => {
                            // Unexpected response — log and continue
                            tracing::debug!(id = ?resp.id, "unexpected response from agent");
                        }
                        Ok(None) => {
                            return Ok(DriveResult::TransportClosed);
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
    pub(super) transport: Arc<dyn Transport>,
    pub(super) next_request_id: AtomicU64,
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
    /// Includes `elicitationId` on the wire for url-mode elicitations,
    /// enabling correlation with `notifications/elicitation/complete`.
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
        elicitation_id: Option<&str>,
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
        if let Some(eid) = elicitation_id {
            params.insert("elicitationId".to_string(), json!(eid));
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
