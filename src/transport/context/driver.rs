use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::EngineError;
use crate::transport::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

use super::extraction::{
    extract_response_id, extract_run_agent_input_context, extract_run_agent_input_messages,
    extract_run_agent_input_state, extract_user_message, format_server_request_as_user_message,
};
use super::handles::{ServerActorEntry, ServerRequest};
use super::tool_roster::build_tool_roster;
use super::types::{ChatMessage, LlmProvider, LlmResponse, ToolCall, ToolDefinition};

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
    /// A2A agent roster injected as a system message during history seeding.
    a2a_system_context: Option<String>,
    /// MCP resource content injected as system messages during history seeding.
    resource_context: Option<String>,
    turn_count: u32,
    max_turns: u32,
    agui_tx: mpsc::UnboundedSender<JsonRpcMessage>,
    agui_response_rx: mpsc::UnboundedReceiver<JsonRpcMessage>,
    thread_id: String,
    run_id: String,
    server_actors: HashMap<String, ServerActorEntry>,
    server_tool_watches: Vec<(String, watch::Receiver<Vec<ToolDefinition>>)>,
    tool_result_rx: mpsc::Receiver<JsonRpcMessage>,
    server_request_rx: mpsc::Receiver<ServerRequest>,
    /// Tool fingerprints from the previous LLM turn (name + description
    /// hash), used to detect roster changes caused by phase advancement
    /// during tool execution — including rug-pull description swaps.
    prev_tool_fingerprints: HashSet<String>,
}

impl ContextTransport {
    /// Creates a new `ContextTransport`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Box<dyn LlmProvider>,
        cli_system_prompt: Option<String>,
        a2a_system_context: Option<String>,
        resource_context: Option<String>,
        max_turns: u32,
        agui_tx: mpsc::UnboundedSender<JsonRpcMessage>,
        agui_response_rx: mpsc::UnboundedReceiver<JsonRpcMessage>,
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
            a2a_system_context,
            resource_context,
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
            prev_tool_fingerprints: HashSet::new(),
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
                        self.emit_run_finished();
                        return Ok(());
                    }
                    Err(_) => {
                        self.emit_run_finished();
                        return Err(EngineError::Driver(
                            "AG-UI actor did not send initial message within 30s".into(),
                        ));
                    }
                }
            }
            () = cancel.cancelled() => {
                self.emit_run_finished();
                return Ok(());
            }
        };

        if let Some(ref cli_prompt) = self.cli_system_prompt {
            self.history.push(ChatMessage::System(cli_prompt.clone()));
        }
        // Inject A2A agent roster (R2) — provides Agent Card metadata,
        // skill descriptions, and examples as system context for the LLM.
        if let Some(ref a2a_ctx) = self.a2a_system_context {
            self.history.push(ChatMessage::System(a2a_ctx.clone()));
        }
        // Inject MCP resource content as system messages.
        // Real agent frameworks include resource content in the LLM context
        // when resources are available from connected MCP servers.
        if let Some(ref res_ctx) = self.resource_context {
            self.history.push(ChatMessage::System(res_ctx.clone()));
        }
        // Inject AG-UI context items (key-value state) as a system message.
        // This surfaces run_agent_input.context for state injection scenarios.
        if let Some(context_text) = extract_run_agent_input_context(&initial) {
            self.history.push(ChatMessage::System(context_text));
        }
        // Inject AG-UI shared state as a system message.
        // This surfaces run_agent_input.state for state injection scenarios
        // like OATF-028.
        if let Some(state_text) = extract_run_agent_input_state(&initial) {
            self.history.push(ChatMessage::System(state_text));
        }
        let seed_messages = extract_run_agent_input_messages(&initial)?;
        for msg in seed_messages {
            self.history.push(msg);
        }

        let mut consecutive_truncations: u32 = 0;

        // Outer loop for multi-turn support. Each iteration handles one
        // run_agent_input (user request). After emitting run_finished, the
        // drive loop waits briefly for the AG-UI actor to advance to a new
        // phase and send another run_agent_input. This enables multi-turn
        // scenarios like rug pulls where tool definitions change between
        // user requests, matching how real agent frameworks (CrewAI,
        // LangGraph) issue sequential LLM calls with refreshed context.
        'multi_turn: loop {

        loop {
            self.turn_count += 1;
            if self.turn_count > self.max_turns || cancel.is_cancelled() {
                break;
            }

            // Drain any queued notifications from server actors (e.g.
            // notifications/tools/list_changed sent by on_enter actions). These
            // are processed by PhaseLoop via synthetic events; the drive loop
            // only needs to clear them so they don't interfere with tool result
            // collection later in this turn.
            while let Ok(msg) = self.tool_result_rx.try_recv() {
                if let JsonRpcMessage::Notification(ref n) = msg {
                    tracing::debug!(method = %n.method, "drained queued notification");
                }
            }

            let (all_tools, tool_router) = build_tool_roster(&self.server_tool_watches);

            // Build fingerprints that include both name and description prefix
            // so rug-pull description swaps (same name, changed desc) are
            // detected — not just tool additions/removals.
            let current_fingerprints: HashSet<String> = all_tools
                .iter()
                .map(|t| {
                    let desc_prefix: String = t.description.chars().take(64).collect();
                    format!("{}|{}", t.name, desc_prefix)
                })
                .collect();

            // Detect tool roster changes from phase advancement and notify
            // the LLM. This is the context-mode equivalent of the MCP
            // `notifications/tools/list_changed` mechanism, enabling rug-pull
            // and supply-chain scenarios where tool definitions change
            // mid-conversation.
            if !self.prev_tool_fingerprints.is_empty()
                && current_fingerprints != self.prev_tool_fingerprints
            {
                let current_names: HashSet<&str> =
                    all_tools.iter().map(|t| t.name.as_str()).collect();
                let prev_names: HashSet<&str> = self
                    .prev_tool_fingerprints
                    .iter()
                    .filter_map(|fp| fp.split('|').next())
                    .collect();
                let added: Vec<&&str> = current_names.difference(&prev_names).collect();
                let removed: Vec<&&str> = prev_names.difference(&current_names).collect();

                let mut parts = Vec::new();
                if !added.is_empty() {
                    let names: Vec<&str> = added.into_iter().copied().collect();
                    parts.push(format!("added: {}", names.join(", ")));
                }
                if !removed.is_empty() {
                    let names: Vec<&str> = removed.into_iter().copied().collect();
                    parts.push(format!("removed: {}", names.join(", ")));
                }
                // Same names but descriptions changed (rug pull).
                if parts.is_empty() {
                    parts.push("tool definitions have been updated".to_string());
                }

                let notification = format!(
                    "[System: The available tools have changed. {}. \
                     Please re-read all tool descriptions before proceeding.]",
                    parts.join("; ")
                );
                tracing::info!(
                    notification = %notification,
                    "tool roster changed after phase advance"
                );
                self.history.push(ChatMessage::System(notification));
            }
            self.prev_tool_fingerprints = current_fingerprints;

            let response = tokio::select! {
                result = self.provider.chat_completion(&self.history, &all_tools) => {
                    match result {
                        Ok(res) => res,
                        Err(e) => {
                            self.emit_run_finished();
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
                    self.emit_text_content(&text_resp.text);

                    if text_resp.is_truncated {
                        consecutive_truncations += 1;
                        if consecutive_truncations >= 2 {
                            self.emit_run_finished();
                            return Err(EngineError::Driver(
                                "Repeated truncation — increase --context-max-tokens".into(),
                            ));
                        }
                        self.history.push(ChatMessage::user("Please continue."));
                        continue;
                    }

                    consecutive_truncations = 0;

                    if let Some(user_text) = self.wait_for_followup(&cancel).await {
                        self.history.push(ChatMessage::user(&user_text));
                        continue;
                    }
                    break;
                }
                LlmResponse::ToolUse(calls) => {
                    consecutive_truncations = 0;
                    self.history.push(ChatMessage::assistant_tool_use(&calls));

                    if self.server_actors.is_empty() {
                        // Single-actor: emit tool call events to AG-UI
                        for call in &calls {
                            self.emit_tool_attempt_to_agui(call);
                        }
                        if let Some(user_text) = self.wait_for_followup(&cancel).await {
                            self.history.push(ChatMessage::user(&user_text));
                            continue;
                        }
                        break;
                    }

                    // Multi-actor: route tool calls to owning actors.
                    let mut pending: HashMap<String, &ToolCall> = HashMap::new();
                    for call in &calls {
                        if let Some(actor_name) = tool_router.get(&call.name) {
                            if let Some(entry) = self.server_actors.get(actor_name) {
                                // For A2A actors, the LLM calls the actor-name
                                // tool but McpServerDriver needs the skill name
                                // to find the tool via find_a2a_skill().
                                // For disambiguated MCP tools, strip the actor
                                // prefix so the server receives the original name.
                                let a2a_skill_name = entry
                                    .a2a_skill_rx
                                    .as_ref()
                                    .and_then(|rx| rx.borrow().clone());
                                let dispatch_name = if entry.mode == "a2a_server" {
                                    a2a_skill_name.as_deref().unwrap_or(&call.name)
                                } else {
                                    let prefix = format!("{actor_name}__");
                                    call.name.strip_prefix(&prefix).unwrap_or(&call.name)
                                };
                                let rewritten = ToolCall {
                                    id: call.id.clone(),
                                    name: dispatch_name.to_string(),
                                    arguments: call.arguments.clone(),
                                    provider_metadata: call.provider_metadata.clone(),
                                };
                                let msg = Self::tool_call_to_json_rpc(&rewritten);
                                if entry.tx.send(msg).await.is_ok() {
                                    pending.insert(call.id.clone(), call);
                                } else {
                                    tracing::warn!(
                                        tool = %call.name,
                                        actor = %actor_name,
                                        "server actor channel closed, synthesizing error"
                                    );
                                    self.history.push(ChatMessage::tool_error(
                                        &call.id,
                                        &format!(
                                            "server actor channel closed for tool: {}",
                                            call.name
                                        ),
                                    ));
                                }
                            }
                        } else {
                            tracing::warn!(
                                tool = %call.name,
                                "no actor owns tool, synthesizing error"
                            );
                            self.history.push(ChatMessage::tool_error(
                                &call.id,
                                &format!("no server actor owns tool: {}", call.name),
                            ));
                        }
                    }

                    // Collect results with absolute deadline.
                    let deadline = tokio::time::Instant::now()
                        + Duration::from_secs(30 * pending.len() as u64);
                    while !pending.is_empty() {
                        tokio::select! {
                            result = self.tool_result_rx.recv() => {
                                match result {
                                    Some(ref msg @ JsonRpcMessage::Response(ref resp)) => {
                                        let result_id = extract_response_id(resp);
                                        if let Some(call) = pending.remove(&result_id) {
                                            self.history.push(ChatMessage::tool_result(
                                                &call.id,
                                                msg,
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
                                            self.history.push(ChatMessage::tool_error(
                                                &call.id,
                                                "server channel closed",
                                            ));
                                        }
                                    }
                                }
                            }
                            Some(server_req) = self.server_request_rx.recv() => {
                                match self
                                    .handle_server_initiated_request(&server_req, &cancel)
                                    .await
                                {
                                    Ok(response) => {
                                        if let Some(entry) = self.server_actors.get(&server_req.actor_name) {
                                            let _ = entry.tx.send(response).await;
                                        }
                                    }
                                    Err(err) => {
                                        // Log but don't propagate — the drive loop must
                                        // exit normally to emit run_finished.
                                        tracing::warn!(
                                            actor = %server_req.actor_name,
                                            error = %err,
                                            "server-initiated request failed, continuing"
                                        );
                                    }
                                }
                            }
                            () = tokio::time::sleep_until(deadline) => {
                                tracing::warn!(
                                    remaining = pending.len(),
                                    "tool result deadline expired, synthesizing errors"
                                );
                                for (_id, call) in pending.drain() {
                                    self.history.push(ChatMessage::tool_error(
                                        &call.id,
                                        "tool result deadline expired",
                                    ));
                                }
                            }
                            () = cancel.cancelled() => {
                                for (_id, call) in pending.drain() {
                                    self.history.push(ChatMessage::tool_error(
                                        &call.id,
                                        "cancelled",
                                    ));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        tracing::debug!("multi-turn: emitting run_finished");
        self.emit_run_finished();

        // Multi-turn: wait for the AG-UI actor to potentially advance to a
        // new phase and send another run_agent_input. This happens when the
        // AG-UI actor has a trigger (e.g. event: run_finished) that advances
        // to a phase with a new run_agent_input.
        tracing::debug!("multi-turn: waiting for next run_agent_input (5s timeout)");
        match tokio::time::timeout(Duration::from_secs(5), self.agui_response_rx.recv()).await {
            Ok(Some(next_input)) => {
                let next_messages = extract_run_agent_input_messages(&next_input)?;
                for msg in next_messages {
                    self.history.push(msg);
                }
                self.run_id = Uuid::new_v4().to_string();
                tracing::info!("multi-turn: received next run_agent_input, continuing");
                continue 'multi_turn;
            }
            Ok(None) => {
                tracing::debug!("multi-turn: channel closed, no next input");
                break 'multi_turn;
            }
            Err(_) => {
                tracing::debug!("multi-turn: timeout waiting for next input");
                break 'multi_turn;
            }
        }

        } // end 'multi_turn loop

        Ok(())
    }

    /// Emits `text_message_content` + `text_message_end` to the AG-UI actor.
    /// Waits for an AG-UI follow-up message (5s timeout, cancellation-aware).
    ///
    /// Returns the user message text if a follow-up arrives, or `None` if the
    /// channel closes, times out, or cancellation fires.
    async fn wait_for_followup(&mut self, cancel: &CancellationToken) -> Option<String> {
        tracing::debug!("wait_for_followup: waiting for AG-UI follow-up (5s timeout)");
        tokio::select! {
            result = tokio::time::timeout(
                Duration::from_secs(5),
                self.agui_response_rx.recv(),
            ) => {
                match result {
                    Ok(Some(follow_up)) => {
                        let text = extract_user_message(&follow_up);
                        tracing::debug!(text = %text, "wait_for_followup: received follow-up");
                        Some(text)
                    }
                    Ok(None) => {
                        tracing::debug!("wait_for_followup: channel closed (no follow-up)");
                        None
                    }
                    Err(_) => {
                        tracing::debug!("wait_for_followup: timed out (5s)");
                        None
                    }
                }
            }
            () = cancel.cancelled() => {
                tracing::debug!("wait_for_followup: cancelled");
                None
            }
        }
    }

    fn emit_text_content(&self, text: &str) {
        let msg_id = Uuid::new_v4().to_string();
        let content_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "text_message_content",
            Some(json!({ "messageId": msg_id, "delta": text })),
        ));
        let end_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "text_message_end",
            Some(json!({ "messageId": msg_id })),
        ));
        let _ = self.agui_tx.send(content_notif);
        let _ = self.agui_tx.send(end_notif);
    }

    /// Emits `tool_call_start` + `tool_call_end` to the AG-UI actor (single-actor only).
    fn emit_tool_attempt_to_agui(&self, call: &ToolCall) {
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
        let _ = self.agui_tx.send(start_notif);
        let _ = self.agui_tx.send(end_notif);
    }

    /// Emits `run_finished` to the AG-UI actor.
    fn emit_run_finished(&self) {
        let finish_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "run_finished",
            Some(json!({
                "threadId": self.thread_id,
                "runId": self.run_id,
            })),
        ));
        let _ = self.agui_tx.send(finish_notif);
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
        &self,
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

        let request_id = match &req.request {
            JsonRpcMessage::Request(r) => r.id.clone(),
            _ => json!(null),
        };

        // Elicitation targets the human user, not the LLM. In context-mode
        // there is no real user to interact with, so reject the request.
        // Elicitation attacks need traffic-mode with a real agent/UI.
        if method == "elicitation/create" {
            tracing::warn!(
                actor = %req.actor_name,
                "elicitation not supported in context-mode — requires real user interaction, rejecting"
            );
            return Ok(JsonRpcMessage::Response(JsonRpcResponse::success(
                request_id,
                json!({ "action": "reject", "content": "context-mode: no user to elicit" }),
            )));
        }

        // Sampling targets the LLM — perform a real LLM roundtrip.
        //
        // Per the MCP spec, sampling creates an ISOLATED LLM call using
        // only the server's provided systemPrompt and messages — NOT the
        // main conversation history.  This is critical for faithful
        // simulation: the model sees a blank context with just the
        // server's content, with no anchoring from the main conversation.
        let mut fork: Vec<ChatMessage> = Vec::new();

        if let Some(params_val) = params {
            // Extract systemPrompt if provided by the server
            if let Some(sys) = params_val
                .get("systemPrompt")
                .and_then(serde_json::Value::as_str)
            {
                fork.push(ChatMessage::System(sys.to_string()));
            }

            // Extract messages array from the sampling request
            if let Some(messages) = params_val
                .get("messages")
                .and_then(serde_json::Value::as_array)
            {
                for msg in messages {
                    let role = msg
                        .get("role")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("user");
                    let text = msg
                        .get("content")
                        .and_then(|c| {
                            // Content can be a string or {"type":"text","text":"..."}
                            c.as_str().map(String::from).or_else(|| {
                                c.get("text")
                                    .and_then(serde_json::Value::as_str)
                                    .map(String::from)
                            })
                        })
                        .unwrap_or_default();

                    match role {
                        "assistant" => fork.push(ChatMessage::AssistantText(text)),
                        "user" => fork.push(ChatMessage::User(text)),
                        _ => fork.push(ChatMessage::System(text)),
                    }
                }
            }
        }

        // Fallback: if no messages were extracted, use the formatted prompt
        if fork.is_empty() {
            let prompt = format_server_request_as_user_message(method, params);
            fork.push(ChatMessage::User(prompt));
        }

        let response = tokio::select! {
            result = self.provider.chat_completion(&fork, &[]) => {
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

        let result = match method {
            "sampling/createMessage" => json!({
                "model": "context-mode",
                "role": "assistant",
                "content": { "type": "text", "text": text },
            }),
            _ => json!({ "content": text }),
        };
        Ok(JsonRpcMessage::Response(JsonRpcResponse::success(
            request_id, result,
        )))
    }
}
