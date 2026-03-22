//! Context-mode transport: LLM API-backed conversation with channel handles.
//!
//! `ContextTransport` owns the LLM conversation and drives a turn-based loop.
//! Actors interact via channel-based handles (`AgUiHandle`, `ServerHandle`) that
//! implement the `Transport` trait, keeping `PhaseDriver` code transport-agnostic.
//!
//! See TJ-SPEC-022 for the full specification.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{EngineError, TransportError};
use crate::transport::{
    ConnectionContext, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    Transport, TransportType,
};

// ============================================================================
// LLM Provider types
// ============================================================================

/// Conversation message for the LLM API.
///
/// Serialized differently per provider (`OpenAI` vs `Anthropic`).
/// The enum is provider-agnostic — each `LlmProvider` implementation
/// handles serialisation internally.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// System instruction message.
    System(String),
    /// User message.
    User(String),
    /// Text-only assistant response.
    AssistantText(String),
    /// Assistant response requesting tool calls.
    AssistantToolUse {
        /// Tool calls requested by the assistant.
        tool_calls: Vec<ToolCall>,
    },
    /// Result of a tool call, keyed by `tool_call_id`.
    ToolResult {
        /// The `tool_call_id` this result corresponds to.
        tool_call_id: String,
        /// The result content (JSON value).
        content: Value,
    },
}

impl ChatMessage {
    /// Creates an `AssistantText` message.
    #[must_use]
    pub fn assistant_text(text: &str) -> Self {
        Self::AssistantText(text.to_string())
    }

    /// Creates an `AssistantToolUse` message.
    #[must_use]
    pub fn assistant_tool_use(calls: &[ToolCall]) -> Self {
        Self::AssistantToolUse {
            tool_calls: calls.to_vec(),
        }
    }

    /// Creates a `ToolResult` message from a JSON-RPC response.
    #[must_use]
    pub fn tool_result(tool_call_id: &str, response: &JsonRpcMessage) -> Self {
        Self::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: extract_result_content(response),
        }
    }

    /// Creates a `User` message.
    #[must_use]
    pub fn user(text: &str) -> Self {
        Self::User(text.to_string())
    }
}

/// Tool definition in LLM-native format (`OpenAI` function calling schema).
///
/// Converted from OATF tool state by `extract_tool_definitions()`.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for tool parameters.
    pub parameters: Value,
}

/// A tool call from the LLM response.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Provider-assigned tool call ID.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool call arguments (JSON object).
    pub arguments: Value,
}

/// Response from an LLM API call.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug)]
pub enum LlmResponse {
    /// Text response (may be truncated).
    Text(TextResponse),
    /// Tool use response with one or more tool calls.
    ToolUse(Vec<ToolCall>),
}

/// Text content from an LLM response.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug)]
pub struct TextResponse {
    /// The text content.
    pub text: String,
    /// True when `finish_reason` was `length` / `stop_reason` was `max_tokens`.
    pub is_truncated: bool,
}

/// Errors from LLM provider operations.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// HTTP error response.
    #[error("HTTP error: {status} {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },
    /// Request transport error.
    #[error("Request error: {0}")]
    Request(#[from] reqwest::Error),
    /// Response parse error.
    #[error("Parse error: {0}")]
    Parse(String),
    /// Rate limited after retries.
    #[error("Rate limited after {retries} retries")]
    RateLimited {
        /// Number of retries attempted.
        retries: u32,
    },
    /// Request timeout.
    #[error("Timeout after {seconds}s")]
    Timeout {
        /// Timeout duration in seconds.
        seconds: u64,
    },
}

/// Async trait for LLM API providers.
///
/// Implementations handle provider-specific serialization (`OpenAI` vs `Anthropic`)
/// and rate limiting. System messages, tool definitions, and conversation
/// history are passed in provider-agnostic form.
///
/// Implements: TJ-SPEC-022 F-001
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Performs a chat completion API call.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` on HTTP, parsing, rate-limiting, or timeout errors.
    async fn chat_completion(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError>;

    /// Returns the provider name (e.g. "openai", "anthropic").
    fn provider_name(&self) -> &'static str;

    /// Returns the model identifier.
    fn model_name(&self) -> &str;
}

// ============================================================================
// ServerActorEntry and ServerRequest
// ============================================================================

/// Entry for a server actor in the `ContextTransport` routing table.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ServerActorEntry {
    /// Actor mode string (e.g. `mcp_server`, `a2a_server`).
    pub mode: String,
    /// Channel sender for dispatching tool calls to this actor.
    pub tx: mpsc::Sender<JsonRpcMessage>,
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

// ============================================================================
// AgUiHandle
// ============================================================================

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

// ============================================================================
// ContextTransport
// ============================================================================

/// Context-mode transport: owns the LLM conversation and drive loop.
///
/// Actors communicate via channel-based handles (`AgUiHandle`, `ServerHandle`).
/// The drive loop calls `LlmProvider::chat_completion()` per turn, routes tool
/// calls to server actors, collects results, and manages conversation history.
///
/// Implements: TJ-SPEC-022 F-001
pub struct ContextTransport {
    provider: Box<dyn LlmProvider>,
    history: Vec<ChatMessage>,
    cli_system_prompt: Option<String>,
    turn_count: u32,
    max_turns: u32,
    agui_tx: mpsc::Sender<JsonRpcMessage>,
    agui_response_rx: mpsc::Receiver<JsonRpcMessage>,
    thread_id: String,
    run_id: String,
    server_actors: HashMap<String, ServerActorEntry>,
    server_tool_watches: Vec<(String, watch::Receiver<Vec<ToolDefinition>>)>,
    tool_result_rx: mpsc::Receiver<JsonRpcMessage>,
    server_request_rx: mpsc::Receiver<ServerRequest>,
}

impl ContextTransport {
    /// Creates a new `ContextTransport`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Box<dyn LlmProvider>,
        cli_system_prompt: Option<String>,
        max_turns: u32,
        agui_tx: mpsc::Sender<JsonRpcMessage>,
        agui_response_rx: mpsc::Receiver<JsonRpcMessage>,
        thread_id: String,
        server_actors: HashMap<String, ServerActorEntry>,
        server_tool_watches: Vec<(String, watch::Receiver<Vec<ToolDefinition>>)>,
        tool_result_rx: mpsc::Receiver<JsonRpcMessage>,
        server_request_rx: mpsc::Receiver<ServerRequest>,
    ) -> Self {
        Self {
            provider,
            history: Vec::new(),
            cli_system_prompt,
            turn_count: 0,
            max_turns,
            agui_tx,
            agui_response_rx,
            thread_id,
            run_id: Uuid::new_v4().to_string(),
            server_actors,
            server_tool_watches,
            tool_result_rx,
            server_request_rx,
        }
    }

    /// Spawns the drive loop as a tokio task, consuming `self`.
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[must_use]
    pub fn spawn_drive_loop(
        mut self,
        cancel: CancellationToken,
    ) -> JoinHandle<Result<(), EngineError>> {
        tokio::spawn(async move { self.drive_loop(cancel).await })
    }

    /// Core drive loop: manages LLM conversation turns.
    ///
    /// Waits for initial `RunAgentInput` from the AG-UI actor, seeds
    /// history, then loops: call LLM → handle response → route tool calls
    /// → collect results → repeat until max turns or completion.
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[allow(
        clippy::too_many_lines,
        clippy::needless_continue,
        clippy::cognitive_complexity
    )]
    async fn drive_loop(&mut self, cancel: CancellationToken) -> Result<(), EngineError> {
        // Wait for initial RunAgentInput from AG-UI actor (30s timeout).
        let initial = tokio::select! {
            result = tokio::time::timeout(
                Duration::from_secs(30),
                self.agui_response_rx.recv(),
            ) => {
                match result {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        self.emit_run_finished().await;
                        return Ok(());
                    }
                    Err(_) => {
                        self.emit_run_finished().await;
                        return Err(EngineError::Driver(
                            "AG-UI actor did not send initial message within 30s".into(),
                        ));
                    }
                }
            }
            () = cancel.cancelled() => {
                self.emit_run_finished().await;
                return Ok(());
            }
        };

        // Seed history from RunAgentInput.messages.
        if let Some(ref cli_prompt) = self.cli_system_prompt {
            self.history.push(ChatMessage::System(cli_prompt.clone()));
        }
        let seed_messages = extract_run_agent_input_messages(&initial)?;
        for msg in seed_messages {
            self.history.push(msg);
        }

        let mut consecutive_truncations: u32 = 0;

        loop {
            self.turn_count += 1;
            if self.turn_count > self.max_turns || cancel.is_cancelled() {
                break;
            }

            // Merge tool definitions from all server actors (per-turn rebuild).
            let mut all_tools = Vec::new();
            let mut tool_router: HashMap<String, String> = HashMap::new();
            for (actor_name, watch_rx) in &self.server_tool_watches {
                let tools = watch_rx.borrow().clone();
                for tool in tools {
                    if tool_router.contains_key(&tool.name) {
                        tracing::warn!(
                            tool = %tool.name,
                            winner = %tool_router[&tool.name],
                            duplicate = %actor_name,
                            "duplicate tool name across actors, first actor wins"
                        );
                    } else {
                        tool_router.insert(tool.name.clone(), actor_name.clone());
                        all_tools.push(tool);
                    }
                }
            }

            // LLM API call (cancellation-aware).
            let response = tokio::select! {
                result = self.provider.chat_completion(&self.history, &all_tools) => {
                    match result {
                        Ok(res) => res,
                        Err(e) => {
                            self.emit_run_finished().await;
                            return Err(EngineError::Driver(format!("LLM API error: {e}")));
                        }
                    }
                },
                () = cancel.cancelled() => break,
            };

            match response {
                LlmResponse::Text(text_resp) => {
                    self.history
                        .push(ChatMessage::assistant_text(&text_resp.text));
                    self.emit_text_content(&text_resp.text).await;

                    if text_resp.is_truncated {
                        consecutive_truncations += 1;
                        if consecutive_truncations >= 2 {
                            self.emit_run_finished().await;
                            return Err(EngineError::Driver(
                                "Repeated truncation — increase --context-max-tokens".into(),
                            ));
                        }
                        self.history.push(ChatMessage::user("Please continue."));
                        continue;
                    }

                    consecutive_truncations = 0;

                    // Wait for AG-UI follow-up (5s timeout).
                    tokio::select! {
                        result = tokio::time::timeout(
                            Duration::from_secs(5),
                            self.agui_response_rx.recv(),
                        ) => {
                            match result {
                                Ok(Some(follow_up)) => {
                                    let user_text = extract_user_message(&follow_up);
                                    self.history.push(ChatMessage::user(&user_text));
                                    continue;
                                }
                                Ok(None) | Err(_) => break,
                            }
                        }
                        () = cancel.cancelled() => break,
                    }
                }
                LlmResponse::ToolUse(calls) => {
                    consecutive_truncations = 0;
                    self.history.push(ChatMessage::assistant_tool_use(&calls));

                    if self.server_actors.is_empty() {
                        // Single-actor: emit tool call events to AG-UI
                        for call in &calls {
                            self.emit_tool_attempt_to_agui(call).await;
                        }
                        // Wait for AG-UI follow-up
                        tokio::select! {
                            result = tokio::time::timeout(
                                Duration::from_secs(5),
                                self.agui_response_rx.recv(),
                            ) => {
                                match result {
                                    Ok(Some(follow_up)) => {
                                        let user_text = extract_user_message(&follow_up);
                                        self.history.push(ChatMessage::user(&user_text));
                                        continue;
                                    }
                                    Ok(None) | Err(_) => break,
                                }
                            }
                            () = cancel.cancelled() => break,
                        }
                    }

                    // Multi-actor: route tool calls to owning actors.
                    let mut pending: HashMap<String, &ToolCall> = HashMap::new();
                    for call in &calls {
                        if let Some(actor_name) = tool_router.get(&call.name) {
                            if let Some(entry) = self.server_actors.get(actor_name) {
                                let msg = Self::tool_call_to_json_rpc(call);
                                let _ = entry.tx.send(msg).await;
                                pending.insert(call.id.clone(), call);
                            }
                        } else {
                            tracing::warn!(
                                tool = %call.name,
                                "no actor owns tool, synthesizing error"
                            );
                            self.history.push(ChatMessage::ToolResult {
                                tool_call_id: call.id.clone(),
                                content: json!({"error": format!(
                                    "no server actor owns tool: {}",
                                    call.name
                                )}),
                            });
                        }
                    }

                    // Collect results with absolute deadline.
                    let deadline = tokio::time::Instant::now()
                        + Duration::from_secs(30 * pending.len() as u64);
                    while !pending.is_empty() {
                        tokio::select! {
                            result = self.tool_result_rx.recv() => {
                                match result {
                                    Some(JsonRpcMessage::Response(ref resp)) => {
                                        let result_id = extract_response_id(resp);
                                        if let Some(call) = pending.remove(&result_id) {
                                            self.history.push(ChatMessage::tool_result(
                                                &call.id,
                                                result.as_ref().unwrap_or(&JsonRpcMessage::Response(resp.clone())),
                                            ));
                                        } else {
                                            tracing::warn!(
                                                id = %result_id,
                                                "unexpected tool result id"
                                            );
                                        }
                                    }
                                    Some(JsonRpcMessage::Notification(ref notif)) => {
                                        tracing::trace!(
                                            method = %notif.method,
                                            "discarding notification in context-mode"
                                        );
                                    }
                                    Some(JsonRpcMessage::Request(_)) => {
                                        tracing::warn!(
                                            "unexpected Request on tool_result_rx"
                                        );
                                    }
                                    None => {
                                        tracing::warn!(
                                            remaining = pending.len(),
                                            "server channel closed, synthesizing errors"
                                        );
                                        for (_id, call) in pending.drain() {
                                            self.history.push(ChatMessage::ToolResult {
                                                tool_call_id: call.id.clone(),
                                                content: json!({"error": "server channel closed"}),
                                            });
                                        }
                                    }
                                }
                            }
                            Some(server_req) = self.server_request_rx.recv() => {
                                let response = self
                                    .handle_server_initiated_request(&server_req, &cancel)
                                    .await?;
                                if let Some(entry) = self.server_actors.get(&server_req.actor_name) {
                                    let _ = entry.tx.send(response).await;
                                }
                            }
                            () = tokio::time::sleep_until(deadline) => {
                                tracing::warn!(
                                    remaining = pending.len(),
                                    "tool result deadline expired, synthesizing errors"
                                );
                                for (_id, call) in pending.drain() {
                                    self.history.push(ChatMessage::ToolResult {
                                        tool_call_id: call.id.clone(),
                                        content: json!({"error": "tool result deadline expired"}),
                                    });
                                }
                            }
                            () = cancel.cancelled() => {
                                for (_id, call) in pending.drain() {
                                    self.history.push(ChatMessage::ToolResult {
                                        tool_call_id: call.id.clone(),
                                        content: json!({"error": "cancelled"}),
                                    });
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        self.emit_run_finished().await;
        Ok(())
    }

    /// Emits `text_message_content` + `text_message_end` to the AG-UI actor.
    async fn emit_text_content(&self, text: &str) {
        let msg_id = Uuid::new_v4().to_string();
        let content_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "text_message_content",
            Some(json!({ "messageId": msg_id, "delta": text })),
        ));
        let end_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "text_message_end",
            Some(json!({ "messageId": msg_id })),
        ));
        let _ = self.agui_tx.send(content_notif).await;
        let _ = self.agui_tx.send(end_notif).await;
    }

    /// Emits `tool_call_start` + `tool_call_end` to the AG-UI actor (single-actor only).
    async fn emit_tool_attempt_to_agui(&self, call: &ToolCall) {
        let tc_id = Uuid::new_v4().to_string();
        let start_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "tool_call_start",
            Some(json!({
                "toolCallId": tc_id,
                "name": call.name,
                "arguments": call.arguments,
            })),
        ));
        let end_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "tool_call_end",
            Some(json!({ "toolCallId": tc_id })),
        ));
        let _ = self.agui_tx.send(start_notif).await;
        let _ = self.agui_tx.send(end_notif).await;
    }

    /// Emits `run_finished` to the AG-UI actor.
    async fn emit_run_finished(&self) {
        let finish_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "run_finished",
            Some(json!({
                "threadId": self.thread_id,
                "runId": self.run_id,
            })),
        ));
        let _ = self.agui_tx.send(finish_notif).await;
    }

    /// Converts a `ToolCall` to a `JsonRpcMessage` for context-mode dispatch.
    ///
    /// In context-mode all server actors (MCP and A2A) are driven by
    /// `McpServerDriver`, so tool calls are always sent as `tools/call`.
    fn tool_call_to_json_rpc(call: &ToolCall) -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": call.name,
                "arguments": call.arguments,
            })),
            id: json!(call.id),
        })
    }

    /// Handles a server-initiated request (elicitation/sampling) via LLM roundtrip.
    async fn handle_server_initiated_request(
        &mut self,
        req: &ServerRequest,
        cancel: &CancellationToken,
    ) -> Result<JsonRpcMessage, EngineError> {
        let (method, params) = match &req.request {
            JsonRpcMessage::Request(r) => (r.method.as_str(), &r.params),
            _ => {
                return Err(EngineError::Driver(
                    "expected Request in ServerRequest".into(),
                ));
            }
        };

        let prompt = format_server_request_as_user_message(method, params);
        self.history.push(ChatMessage::user(&prompt));

        // LLM roundtrip — no tools.
        let response = tokio::select! {
            result = self.provider.chat_completion(&self.history, &[]) => {
                match result {
                    Ok(resp) => resp,
                    Err(e) => {
                        return Err(EngineError::Driver(
                            format!("LLM error during {method}: {e}"),
                        ));
                    }
                }
            },
            () = cancel.cancelled() => {
                return Err(EngineError::Driver(
                    "cancelled during server request".into(),
                ));
            }
        };

        let text = match response {
            LlmResponse::Text(t) => t.text,
            LlmResponse::ToolUse(_) => {
                tracing::warn!("LLM attempted tool use during {method}, using empty response");
                String::new()
            }
        };
        self.history.push(ChatMessage::assistant_text(&text));

        let request_id = match &req.request {
            JsonRpcMessage::Request(r) => r.id.clone(),
            _ => json!(null),
        };
        let result = match method {
            "elicitation/create" => json!({
                "action": "accept",
                "content": text,
            }),
            "sampling/createMessage" => json!({
                "model": "context-mode",
                "role": "assistant",
                "content": { "type": "text", "text": text },
            }),
            _ => json!({ "content": text }),
        };
        Ok(JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: crate::transport::JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id: request_id,
        }))
    }
}

// ============================================================================
// Free functions
// ============================================================================

/// Extracts conversation messages from a `RunAgentInput` JSON-RPC message.
///
/// Maps each message by role: `system` → `System`, `user` → `User`,
/// `assistant` → `AssistantText`. Returns `Err` if the message is malformed.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the message lacks `params` or the
/// `messages` array within params.
///
/// Implements: TJ-SPEC-022 F-001
pub fn extract_run_agent_input_messages(
    msg: &JsonRpcMessage,
) -> Result<Vec<ChatMessage>, EngineError> {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        _ => None,
    };
    let params =
        params.ok_or_else(|| EngineError::Driver("initial AG-UI message missing params".into()))?;

    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EngineError::Driver("initial AG-UI message missing 'messages' array in params".into())
        })?;

    let mut result = Vec::with_capacity(messages.len());
    for entry in messages {
        let role = entry.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = entry.get("content").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" => result.push(ChatMessage::System(content.to_string())),
            "assistant" => result.push(ChatMessage::AssistantText(content.to_string())),
            _ => result.push(ChatMessage::User(content.to_string())),
        }
    }
    Ok(result)
}

/// Extracts the last user message from a follow-up `RunAgentInput`.
///
/// Append-only invariant: only the last user message is extracted from
/// follow-up turns. The drive loop owns the full conversation history.
///
/// Implements: TJ-SPEC-022 F-001
pub fn extract_user_message(msg: &JsonRpcMessage) -> String {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        _ => None,
    };
    if let Some(params) = params
        && let Some(messages) = params.get("messages").and_then(Value::as_array)
    {
        // Find last message with role "user"
        for entry in messages.iter().rev() {
            let role = entry.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "user"
                && let Some(content) = entry.get("content").and_then(Value::as_str)
            {
                return content.to_string();
            }
        }
    }
    // Fallback: serialize entire params
    tracing::warn!("could not extract user message from follow-up, using serialized params");
    params
        .map(|p| serde_json::to_string(p).unwrap_or_default())
        .unwrap_or_default()
}

/// Formats a server-initiated request as a user message for history injection.
///
/// Implements: TJ-SPEC-022 F-001
pub fn format_server_request_as_user_message(method: &str, params: &Option<Value>) -> String {
    let params_str = params
        .as_ref()
        .map(|p| serde_json::to_string(p).unwrap_or_default())
        .unwrap_or_default();
    match method {
        "elicitation/create" => {
            let message = params
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(&params_str);
            format!("[Server elicitation] {message}")
        }
        "sampling/createMessage" => {
            format!("[Server sampling request] {params_str}")
        }
        _ => format!("[Server request: {method}] {params_str}"),
    }
}

/// Extracts tool definitions from an actor's effective state.
///
/// For MCP actors: reads `state.tools[]` and maps `name`, `description`,
/// `inputSchema`. For A2A actors: reads `state.skills[]` with permissive schema.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_tool_definitions(state: &Value) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();

    // MCP tools
    if let Some(tool_array) = state.get("tools").and_then(Value::as_array) {
        for tool in tool_array {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            tools.push(ToolDefinition {
                name,
                description,
                parameters,
            });
        }
    }

    // A2A skills
    if let Some(skill_array) = state.get("skills").and_then(Value::as_array) {
        for skill in skill_array {
            let name = skill
                .get("name")
                .or_else(|| skill.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = skill
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            tools.push(ToolDefinition {
                name,
                description,
                parameters: json!({"type": "object", "additionalProperties": true}),
            });
        }
    }

    tools
}

/// Extracts result content from a `JsonRpcMessage`, handling both success and error.
///
/// For MCP responses: returns `result` field as-is or `error` object.
/// For A2A responses: normalizes parts array into a single string.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_result_content(response: &JsonRpcMessage) -> Value {
    match response {
        JsonRpcMessage::Response(resp) => {
            if let Some(ref error) = resp.error {
                // Preserve error object as-is
                json!({
                    "error": {
                        "code": error.code,
                        "message": error.message,
                    }
                })
            } else if let Some(ref result) = resp.result {
                // Check for A2A response format (has message.parts)
                if let Some(message) = result.get("message")
                    && let Some(parts) = message.get("parts").and_then(Value::as_array)
                {
                    return normalize_a2a_parts(parts);
                }
                // MCP: return result as-is
                result.clone()
            } else {
                json!(null)
            }
        }
        _ => json!(null),
    }
}

/// Normalizes A2A response parts into a single string value.
fn normalize_a2a_parts(parts: &[Value]) -> Value {
    let mut segments = Vec::new();
    for part in parts {
        let kind = part.get("kind").and_then(Value::as_str).unwrap_or("text");
        if kind == "text" {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                segments.push(text.to_string());
            }
        } else {
            // Non-text parts serialized as JSON
            segments.push(format!(
                "[{kind}]: {}",
                serde_json::to_string(part).unwrap_or_default()
            ));
        }
    }
    Value::String(segments.join("\n"))
}

/// Extracts the response ID from a `JsonRpcResponse` as a string.
#[must_use]
pub fn extract_response_id(resp: &JsonRpcResponse) -> String {
    match &resp.id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let msg = ChatMessage::user("hello");
        assert!(matches!(msg, ChatMessage::User(s) if s == "hello"));

        let msg = ChatMessage::assistant_text("world");
        assert!(matches!(msg, ChatMessage::AssistantText(s) if s == "world"));

        let calls = vec![ToolCall {
            id: "tc1".into(),
            name: "search".into(),
            arguments: json!({"q": "test"}),
        }];
        let msg = ChatMessage::assistant_tool_use(&calls);
        assert!(
            matches!(msg, ChatMessage::AssistantToolUse { tool_calls } if tool_calls.len() == 1)
        );
    }

    #[test]
    fn test_extract_tool_definitions_mcp() {
        let state = json!({
            "tools": [
                {
                    "name": "search",
                    "description": "Search the web",
                    "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
                },
                {
                    "name": "read",
                    "description": "Read a file"
                }
            ]
        });
        let tools = extract_tool_definitions(&state);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[1].name, "read");
        assert_eq!(tools[1].parameters, json!({"type": "object"}));
    }

    #[test]
    fn test_extract_tool_definitions_a2a() {
        let state = json!({
            "skills": [
                {"name": "translate", "description": "Translate text"},
                {"id": "summarize", "description": "Summarize text"}
            ]
        });
        let tools = extract_tool_definitions(&state);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "translate");
        assert_eq!(tools[1].name, "summarize");
    }

    #[test]
    fn test_extract_tool_definitions_empty() {
        let tools = extract_tool_definitions(&json!({}));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_extract_result_content_success() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(json!({"content": [{"type": "text", "text": "hello"}]})),
            error: None,
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        assert_eq!(
            content,
            json!({"content": [{"type": "text", "text": "hello"}]})
        );
    }

    #[test]
    fn test_extract_result_content_error() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(crate::transport::JsonRpcError {
                code: -32601,
                message: "tool not found".into(),
                data: None,
            }),
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        assert_eq!(content["error"]["code"], -32601);
    }

    #[test]
    fn test_extract_result_content_a2a() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(json!({
                "message": {
                    "parts": [
                        {"kind": "text", "text": "First"},
                        {"kind": "text", "text": "Second"},
                        {"kind": "file", "uri": "s3://bucket/file"}
                    ]
                }
            })),
            error: None,
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        let s = content.as_str().unwrap();
        assert!(s.starts_with("First\nSecond\n[file]:"));
    }

    #[test]
    fn test_extract_response_id() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!("abc-123"),
        };
        assert_eq!(extract_response_id(&resp), "abc-123");

        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!(42),
        };
        assert_eq!(extract_response_id(&resp), "42");
    }

    #[test]
    fn test_extract_run_agent_input_messages() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [
                    {"role": "system", "content": "You are helpful"},
                    {"role": "user", "content": "Hello"},
                    {"role": "assistant", "content": "Hi there"}
                ]
            })),
            id: json!("1"),
        });
        let messages = extract_run_agent_input_messages(&msg).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[0], ChatMessage::System(s) if s == "You are helpful"));
        assert!(matches!(&messages[1], ChatMessage::User(s) if s == "Hello"));
        assert!(matches!(&messages[2], ChatMessage::AssistantText(s) if s == "Hi there"));
    }

    #[test]
    fn test_extract_run_agent_input_messages_malformed() {
        let msg = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!("1"),
        });
        assert!(extract_run_agent_input_messages(&msg).is_err());
    }

    #[test]
    fn test_extract_user_message() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [
                    {"role": "system", "content": "system"},
                    {"role": "user", "content": "first"},
                    {"role": "user", "content": "second"}
                ]
            })),
            id: json!("1"),
        });
        assert_eq!(extract_user_message(&msg), "second");
    }

    #[test]
    fn test_format_server_request_elicitation() {
        let result = format_server_request_as_user_message(
            "elicitation/create",
            &Some(json!({"message": "Enter your name"})),
        );
        assert_eq!(result, "[Server elicitation] Enter your name");
    }

    #[test]
    fn test_format_server_request_sampling() {
        let result = format_server_request_as_user_message(
            "sampling/createMessage",
            &Some(json!({"messages": []})),
        );
        assert!(result.starts_with("[Server sampling request]"));
    }

    #[test]
    fn test_normalize_a2a_parts() {
        let parts = vec![
            json!({"kind": "text", "text": "Hello"}),
            json!({"kind": "text", "text": "World"}),
        ];
        let result = normalize_a2a_parts(&parts);
        assert_eq!(result, Value::String("Hello\nWorld".into()));
    }
}
