# TJ-SPEC-022: Context Mode

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-022` |
| **Title** | Context Mode |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | High |
| **Version** | v1.2.0 |
| **Depends On** | TJ-SPEC-002 (Transport Abstraction), TJ-SPEC-013 (OATF Integration), TJ-SPEC-014 (Verdict & Evaluation), TJ-SPEC-015 (Multi-Actor Orchestration), TJ-SPEC-016 (AG-UI Protocol Support), TJ-SPEC-017 (A2A Protocol Support) |
| **Tags** | `#context-mode` `#llm` `#transport` `#provider` `#simulation` |

## 1. Overview

Context-mode tests model susceptibility to agent-layer attacks by calling an LLM API directly and injecting adversarial payloads into the conversation history as tool results, rather than running real protocol infrastructure. Every context-mode scenario requires an `ag_ui_client` actor. Single-actor scenarios test history manipulation; multi-actor scenarios (AG-UI + one or more MCP/A2A server actors) test tool-result poisoning and cross-protocol attacks.

### 1.1 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Transport, not mode** | Context-mode is a delivery mechanism. The `PhaseDriver` trait, `PhaseEngine`, and verdict pipeline operate unchanged. AG-UI requires a context-mode driver (`ContextAgUiDriver`) because the traffic-mode `AgUiDriver` uses a concrete `AgUiTransport` with HTTP-specific methods (`send_run()`, SSE streams) that can't work with `AgUiHandle`. |
| **AG-UI actor required** | The AG-UI actor owns the LLM conversation and provides the user-facing interaction surface. |
| **Trace via PhaseLoop** | Trace entries are recorded by PhaseLoop's `process_protocol_event()`, not by the transport. Drivers emit `ProtocolEvent` objects on `event_tx` as in traffic-mode. |
| **Provider config is runtime** | LLM provider configuration is CLI/env, not in OATF documents. |

### 1.2 Scope

**In scope:** `ContextTransport` with drive loop, `ContextAgUiDriver` implementing `PhaseDriver`, `AgUiHandle` and `ServerHandle` implementing `Transport`, LLM provider abstraction and configuration, tool definition synchronization via watch channel, required changes to `PhaseLoopConfig` and `apply_delivery`.

**Out of scope:** PhaseDriver/PhaseEngine changes (none required), verdict evaluation logic changes (none required; minor output schema addition in TJ-SPEC-014 for mode attribution), `--max-turns` flag (TJ-SPEC-007 update).

**Elicitation/sampling support:** MCP server elicitation and sampling requests work in context-mode. `ServerHandle.send_message()` routes `JsonRpcMessage::Request` (server-initiated requests) to a dedicated `server_request_rx` channel, separate from tool results. The drive loop performs an LLM roundtrip for each request and routes the response back to the originating actor. No changes to the MCP server driver are required — the existing `send_server_request()` flow works unchanged via the `Transport` trait.

**Interaction model:** Context-mode supports multi-turn AG-UI conversations. The `ContextAgUiDriver` builds `RunAgentInput` from state and sends it to the drive loop. On the first message, the drive loop seeds history from the full `RunAgentInput.messages` array (preserving system messages, prior turns, and context). The LLM responds (potentially calling tools), and the AG-UI PhaseDriver's existing trigger/phase machinery decides whether to send a follow-up. On follow-up messages, the drive loop extracts only the last user message and appends it to the existing history. The conversation ends when the AG-UI actor runs out of phases (channel closes) or the `--max-turns` ceiling is reached.

---

## 2. Architecture

### 2.1 ContextTransport

The `ContextTransport` owns the LLM API connection and conversation history. Actors do not access it directly -- each actor receives a `Transport`-implementing handle that communicates via dedicated channels. `spawn_drive_loop()` takes ownership of the struct, so fields consumed only by the drive loop (`history`, `tool_result_rx`) do not need synchronization primitives.

```rust
pub struct ServerActorEntry {
    pub mode: ActorMode,  // mcp_server or a2a_server
    pub tx: mpsc::Sender<JsonRpcMessage>,
}

pub struct ContextTransport {
    provider: Box<dyn LlmProvider>,
    history: Vec<ChatMessage>,              // Owned by drive_loop, no Mutex needed
    /// CLI/env system prompt (--context-system-prompt). Prepended to history
    /// during initial seeding. Default is None (blank).
    cli_system_prompt: Option<String>,
    turn_count: u32,                        // Local counter, no AtomicU32 needed
    max_turns: u32,
    agui_tx: mpsc::Sender<JsonRpcMessage>,

    /// Return channel from AG-UI actor. When the AG-UI PhaseDriver's trigger
    /// matches and phase advances, PhaseLoop calls transport.send_message()
    /// which forwards the follow-up user message here. Channel close means
    /// the AG-UI actor has finished all phases.
    agui_response_rx: mpsc::Receiver<JsonRpcMessage>,

    /// Stable conversation thread ID (UUID, generated by orchestrator, shared with ContextAgUiDriver).
    thread_id: String,
    /// Run ID (UUID, generated once at construction — one run per drive loop).
    run_id: String,

    /// Per-server-actor info, keyed by actor name.
    /// Stores the protocol mode and channel for each actor.
    server_actors: HashMap<String, ServerActorEntry>,

    /// Per-server-actor tool definition watches.
    /// Each entry is (actor_name, watch::Receiver) — rebuilt into a
    /// merged tool list and routing table before each API call.
    server_tool_watches: Vec<(String, watch::Receiver<Vec<ToolDefinition>>)>,

    /// Single result channel shared by all ServerHandles (via cloned Senders).
    /// Drive loop is sole consumer. Results matched by JSON-RPC id.
    tool_result_rx: mpsc::Receiver<JsonRpcMessage>,

    /// Server-initiated request channel shared by all ServerHandles.
    /// Receives elicitation/sampling requests that require an LLM roundtrip.
    /// Each request is tagged with the actor name for response routing.
    server_request_rx: mpsc::Receiver<ServerRequest>,
}
```

### 2.2 Handle Types

Two handle types implement `Transport` (TJ-SPEC-002 F-001):

**`AgUiHandle`** -- receives AG-UI events via `rx`, sends follow-up user messages back to the drive loop via `response_tx` (see S2.8 for canonical event schema):

```rust
pub struct AgUiHandle {
    rx: Mutex<mpsc::Receiver<JsonRpcMessage>>,
    /// Return channel to ContextTransport. When the AG-UI PhaseDriver
    /// advances a phase and calls send_message() with a follow-up user
    /// message, it is forwarded here for the drive loop to append to history.
    response_tx: mpsc::Sender<JsonRpcMessage>,
    created_at: Instant,
}

#[async_trait]
impl Transport for AgUiHandle {
    async fn send_message(&self, message: &JsonRpcMessage) -> Result<()> {
        // Forward follow-up messages to the drive loop's agui_response_rx
        self.response_tx.send(message.clone()).await
            .map_err(|_| TransportError::ConnectionClosed("drive loop closed".into()))?;
        Ok(())
    }
    async fn send_raw(&self, _bytes: &[u8]) -> Result<()> {
        Err(TransportError::ConnectionClosed("send_raw not supported in context-mode".into()))
    }
    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await) // None when channel closes (drive loop complete)
    }
    fn transport_type(&self) -> TransportType { TransportType::Context }
    async fn finalize_response(&self) -> Result<()> { Ok(()) }
    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext { connection_id: 0, remote_addr: None, is_exclusive: true, connected_at: self.created_at }
    }
    fn as_any(&self) -> &dyn std::any::Any { self }
}
```

**`ServerHandle`** -- receives tool call `JsonRpcRequest` objects, sends results and server-initiated requests back through separate channels:

```rust
/// A server-initiated request (elicitation/sampling) routed to the drive loop
/// for an LLM roundtrip. Tagged with actor name for response routing.
pub struct ServerRequest {
    pub actor_name: String,
    pub request: JsonRpcMessage,
}

pub struct ServerHandle {
    rx: Mutex<mpsc::Receiver<JsonRpcMessage>>,
    result_tx: mpsc::Sender<JsonRpcMessage>,
    /// Channel for server-initiated requests (elicitation, sampling).
    /// These require an LLM roundtrip, so they go to the drive loop
    /// separately from tool results.
    server_request_tx: mpsc::Sender<ServerRequest>,
    actor_name: String,
    created_at: Instant,
}

#[async_trait]
impl Transport for ServerHandle {
    async fn send_message(&self, message: &JsonRpcMessage) -> Result<()> {
        match message {
            JsonRpcMessage::Request(_) => {
                // Server-initiated request (elicitation/sampling) —
                // route to drive loop for LLM roundtrip
                self.server_request_tx.send(ServerRequest {
                    actor_name: self.actor_name.clone(),
                    request: message.clone(),
                }).await
                    .map_err(|_| TransportError::ConnectionClosed("drive loop closed".into()))?;
            }
            _ => {
                // Tool result or notification — route to result channel
                self.result_tx.send(message.clone()).await
                    .map_err(|_| TransportError::ConnectionClosed("context transport closed".into()))?;
            }
        }
        Ok(())
    }
    async fn send_raw(&self, _bytes: &[u8]) -> Result<()> {
        Err(TransportError::ConnectionClosed("send_raw not supported in context-mode".into()))
    }
    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }
    fn transport_type(&self) -> TransportType { TransportType::Context }
    async fn finalize_response(&self) -> Result<()> { Ok(()) }
    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext { connection_id: 0, remote_addr: None, is_exclusive: true, connected_at: self.created_at }
    }
    fn as_any(&self) -> &dyn std::any::Any { self }
}
```

**How this enables elicitation/sampling in context-mode:** In traffic-mode, the MCP server driver's `send_server_request()` calls `transport.send_message(request)` to send an elicitation/sampling request to the agent, then calls `transport.receive_message()` to wait for the response. In context-mode, `send_message()` routes `JsonRpcMessage::Request` to `server_request_tx` (not `result_tx`). The drive loop receives it on `server_request_rx`, performs an LLM roundtrip, and sends the response back via `server_actors[actor_name].tx` → `ServerHandle.rx`. The server driver's `receive_message()` picks it up and continues tool dispatch. No changes to the MCP server driver code.

**Note on `capture_raw_writer()`:** The actual `Transport` trait includes `capture_raw_writer()` with a default implementation returning `Ok(None)`. Both `AgUiHandle` and `ServerHandle` inherit this default — context-mode has no per-request raw byte writers. No override needed.

### 2.3 TransportType Extension

Add `Context` variant to `TransportType` in `src/transport/mod.rs` and `Self::Context => write!(f, "context")` to the `Display` impl.

### 2.4 Channel Topology and Construction Order

```
ContextTransport (drive_loop task)
    |-- agui_tx ------------> agui_rx (AgUiHandle.rx)           // TO AG-UI
    |-- agui_response_rx <--- response_tx (AgUiHandle)          // FROM AG-UI
    |
    |-- server_actors["mcp_poison"].tx --> server_rx (ServerHandle A .rx)
    |-- server_actors["a2a_skill"].tx  --> server_rx (ServerHandle B .rx)
    |   ... one entry per server actor ...
    |
    |-- tool_result_rx <------- result_tx (cloned, for JsonRpcResponse)
    |-- server_request_rx <---- server_request_tx (cloned, for JsonRpcRequest)
```

All `ServerHandle` instances share two cloned senders: `result_tx` (tool results, `JsonRpcMessage::Response`) and `server_request_tx` (server-initiated requests, `JsonRpcMessage::Request`). `ServerHandle.send_message()` routes by message variant. The drive loop selects on both `tool_result_rx` and `server_request_rx` during result collection. The `agui_response_rx` channel carries follow-up user messages from the AG-UI PhaseDriver when it advances phases.

Construction order:

1. Orchestrator generates a shared `thread_id` (UUID)
2. Orchestrator creates `agui_tx`/`agui_rx` channel (bounded, capacity: 16)
3. Orchestrator creates `agui_response_tx`/`agui_response_rx` channel (bounded, capacity: 16)
4. Orchestrator creates one shared `tool_result_tx`/`tool_result_rx` channel (bounded, capacity: 16)
5. Orchestrator creates one shared `server_request_tx`/`server_request_rx` channel (bounded, capacity: 16)
6. For each server actor (`mcp_server` or `a2a_server`), **in OATF document order**:
   a. Creates a `server_tx`/`server_rx` channel (bounded, capacity: 16)
   b. Extracts initial tool definitions from the actor's first phase state via `extract_tool_definitions(compute_effective_state(phase_0))`
   c. Creates `watch::channel(initial_tools)` for that actor
   d. Constructs `ServerHandle` with `server_rx`, `result_tx.clone()`, `server_request_tx.clone()`, and `actor_name`
   e. Records `ServerActorEntry { mode, tx: server_tx }` in `server_actors` HashMap
   f. Records `(actor_name, tool_watch_rx)` in `server_tool_watches` Vec
   g. Passes `tool_watch_tx` to `PhaseLoopConfig` for that actor's PhaseLoop

**Ordering invariant:** `server_tool_watches` must be populated and iterated in OATF document order (the order actors appear in `execution.actors`). This determines the "first actor wins" collision resolution in EC-CTX-017. This ordering must be preserved across any future refactoring.
7. Constructs `ContextTransport` with `thread_id.clone()`, `agui_tx`, `agui_response_rx`, `server_actors`, `server_tool_watches`, `tool_result_rx`, `server_request_rx`
8. Constructs `AgUiHandle` with `agui_rx` and `agui_response_tx`
9. For the AG-UI actor: constructs `ContextAgUiDriver::new(Box::new(agui_handle), thread_id.clone())` and spawns its `PhaseLoop` directly. The orchestrator owns this — `run_actor()` is not called for the AG-UI actor in context-mode.
10. For each server actor: calls `run_actor()` with the `ServerHandle` as transport. `run_actor()` constructs the protocol-specific `PhaseDriver` (MCP or A2A) as in traffic-mode.
11. Spawns `ContextTransport::spawn_drive_loop()`

For single-actor scenarios (AG-UI only), steps 5-6 are skipped entirely. `server_actors` is empty, `server_tool_watches` is empty, and the drive loop takes the single-actor path. The `agui_response_rx` channel is still created — the AG-UI actor can still advance phases and send follow-up messages even without server actors.

### 2.5 Drive Loop

```rust
impl ContextTransport {
    pub fn spawn_drive_loop(mut self, cancel: CancellationToken) -> JoinHandle<Result<(), EngineError>> {
        tokio::spawn(async move { self.drive_loop(cancel).await })
    }

    async fn drive_loop(&mut self, cancel: CancellationToken) -> Result<(), EngineError> {
        // Wait for initial RunAgentInput from AG-UI actor.
        // The ContextAgUiDriver builds RunAgentInput from state and sends it
        // via transport.send_message() → agui_response_rx. The drive loop must
        // parse the full messages array before making any LLM calls.
        let initial = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), self.agui_response_rx.recv()) => {
                match result {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        self.emit_run_finished().await;
                        return Ok(()); // AG-UI actor exited without sending
                    }
                    Err(_) => {
                        self.emit_run_finished().await;
                        return Err(EngineError::Driver(
                            "AG-UI actor did not send initial message within 30s".into()
                        ));
                    }
                }
            }
            _ = cancel.cancelled() => {
                self.emit_run_finished().await;
                return Ok(());
            }
        };

        // Seed history from the full RunAgentInput.messages array.
        // This preserves system messages, prior turns, and context
        // exactly as the OATF document defines them.
        // Seed history from RunAgentInput.messages.
        // CLI system prompt (if set) goes first, then OATF messages as-is.
        // System messages in the OATF array stay in history — the LlmProvider
        // handles serialisation per its API (OpenAI: system role messages;
        // Anthropic: extracted to top-level system parameter).
        if let Some(ref cli_prompt) = self.cli_system_prompt {
            self.history.push(ChatMessage::System(cli_prompt.clone()));
        }
        let seed_messages = extract_run_agent_input_messages(&initial)?;
        for msg in seed_messages {
            self.history.push(msg);
        }

        let mut consecutive_truncations: u32 = 0;

        loop {
            // Each LLM API call counts as one turn. A tool-using interaction
            // consumes at least two turns: one for the tool call response, one
            // for the synthesis. Authors should set --max-turns accordingly.
            self.turn_count += 1;
            // Cancellation checked here (between turns) and also inside
            // blocking operations via tokio::select! (provider call, result recv).
            if self.turn_count > self.max_turns || cancel.is_cancelled() {
                break;
            }

            // Merge tool definitions from all server actors and build
            // per-turn routing table (tool_name → actor_name).
            // Rebuilt each turn so phase advances are reflected immediately.
            // Only tools that win routing are included in all_tools —
            // duplicates are excluded so the LLM sees one definition per name.
            let mut all_tools = Vec::new();
            let mut tool_router: HashMap<String, String> = HashMap::new(); // tool_name → actor_name
            for (actor_name, watch_rx) in &self.server_tool_watches {
                let tools = watch_rx.borrow().clone();
                for tool in tools {
                    if tool_router.contains_key(&tool.name) {
                        // Tool name collision: first actor wins, duplicate excluded
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

            // LLM API call — cancellation-aware so the orchestrator's
            // global timeout can abort a slow provider request mid-flight.
            // System messages live in history (CLI prepended, OATF as-is).
            // The LlmProvider handles serialisation per its API format.
            let response = tokio::select! {
                result = self.provider.chat_completion(
                    &self.history, &all_tools,
                ) => {
                    match result {
                        Ok(res) => res,
                        Err(e) => {
                            self.emit_run_finished().await;
                            return Err(EngineError::Driver(format!("LLM API error: {e}")));
                        }
                    }
                },
                _ = cancel.cancelled() => break,
            };

            match response {
                LlmResponse::Text(text_resp) => {
                    self.history.push(ChatMessage::assistant_text(&text_resp.text));
                    // Always emit content + end (but NOT run_finished yet)
                    self.emit_text_content(&text_resp.text).await;

                    if text_resp.is_truncated {
                        consecutive_truncations += 1;
                        if consecutive_truncations >= 2 {
                            self.emit_run_finished().await;
                            return Err(EngineError::Driver(
                                "Repeated truncation — increase --context-max-tokens".into()
                            ));
                        }
                        // Give the model another turn to finish its thought
                        self.history.push(ChatMessage::user("Please continue."));
                        continue; // No run_finished — conversation continues
                    }

                    consecutive_truncations = 0;

                    // Wait for AG-UI follow-up, channel close, or cancellation.
                    // The AG-UI PhaseDriver's trigger matching is local (no network
                    // I/O), so if a follow-up is coming, it arrives in milliseconds.
                    // 5s timeout is generous; it covers any PhaseLoop overhead.
                    tokio::select! {
                        result = tokio::time::timeout(
                            Duration::from_secs(5),
                            self.agui_response_rx.recv()
                        ) => {
                            match result {
                                Ok(Some(follow_up)) => {
                                    // AG-UI actor advanced phase, sent a follow-up user message
                                    let user_text = extract_user_message(&follow_up);
                                    self.history.push(ChatMessage::user(&user_text));
                                    continue; // Next LLM turn with follow-up
                                }
                                Ok(None) | Err(_) => {
                                    // Channel closed (AG-UI done with all phases) or
                                    // timeout (no more phases to advance)
                                    break;
                                }
                            }
                        }
                        _ = cancel.cancelled() => break,
                    }
                }
                LlmResponse::ToolUse(calls) => {
                    consecutive_truncations = 0;
                    self.history.push(ChatMessage::assistant_tool_use(&calls));

                    if self.server_actors.is_empty() {
                        // Single-actor: emit AG-UI tool_call events per S2.8
                        for call in &calls {
                            self.emit_tool_attempt_to_agui(call).await;
                        }
                        // Wait for AG-UI follow-up (same as text path).
                        // The AG-UI PhaseDriver sees the tool_call events and may
                        // advance a phase and send a follow-up user message.
                        tokio::select! {
                            result = tokio::time::timeout(
                                Duration::from_secs(5),
                                self.agui_response_rx.recv()
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
                            _ = cancel.cancelled() => break,
                        }
                    }

                    // Multi-actor: route each tool call to the owning actor
                    let mut pending: HashMap<String, &ToolCall> = HashMap::new();
                    for call in &calls {
                        match tool_router.get(&call.name) {
                            Some(actor_name) => {
                                if let Some(entry) = self.server_actors.get(actor_name) {
                                    let msg = self.tool_call_to_json_rpc(call, entry.mode);
                                    let _ = entry.tx.send(msg).await;
                                    pending.insert(call.id.clone(), call);
                                }
                            }
                            None => {
                                // No actor owns this tool — synthesize error inline
                                tracing::warn!(tool = %call.name, "no actor owns tool, synthesizing error");
                                self.history.push(ChatMessage::tool_result(
                                    &call.id,
                                    &synthesize_unknown_tool_error(&call.name),
                                ));
                            }
                        }
                    }

                    // Collect results, matching by JSON-RPC id (not FIFO order).
                    // Absolute deadline prevents deadlock and ensures interleaved
                    // server requests (elicitation/sampling) cannot extend the wait
                    // indefinitely. Budget: 30s per pending call, computed once.
                    let deadline = tokio::time::Instant::now()
                        + Duration::from_secs(30 * pending.len() as u64);
                    while !pending.is_empty() {
                        // Select on tool results, server-initiated requests,
                        // absolute deadline, and cancellation.
                        tokio::select! {
                            result = self.tool_result_rx.recv() => {
                                match result {
                                    Some(JsonRpcMessage::Response(resp)) => {
                                        let result_id = extract_response_id(&resp);
                                        if let Some(call) = pending.remove(&result_id) {
                                            self.history.push(
                                                ChatMessage::tool_result(&call.id,
                                                    &JsonRpcMessage::Response(resp))
                                            );
                                        } else {
                                            tracing::warn!(id = %result_id, "unexpected tool result id");
                                        }
                                    }
                                    Some(JsonRpcMessage::Notification(notif)) => {
                                        // MCP notifications (resources/updated, progress, logging)
                                        // from entry actions. No response id, no effect on pending
                                        // count. Discarded — see S2.12 parity notes.
                                        tracing::trace!(
                                            method = %notif.method,
                                            "discarding notification in context-mode"
                                        );
                                    }
                                    Some(JsonRpcMessage::Request(_)) => {
                                        // Unexpected — server-initiated requests should go to
                                        // server_request_tx, not result_tx. Log and ignore.
                                        tracing::warn!("unexpected Request on tool_result_rx");
                                    }
                                    None => {
                                        tracing::warn!(
                                            remaining = pending.len(),
                                            "server channel closed, synthesizing errors"
                                        );
                                        for (_id, call) in pending.drain() {
                                            self.history.push(ChatMessage::tool_result(
                                                &call.id,
                                                &synthesize_channel_closed_error(&call.id),
                                            ));
                                        }
                                    }
                                }
                            }
                            Some(server_req) = self.server_request_rx.recv() => {
                                // Server-initiated request (elicitation/sampling).
                                // Perform LLM roundtrip and route response back to the actor.
                                // Does NOT reset the deadline — total budget is fixed.
                                let response = self.handle_server_initiated_request(
                                    &server_req, &cancel
                                ).await?;
                                if let Some(entry) = self.server_actors.get(&server_req.actor_name) {
                                    let _ = entry.tx.send(response).await;
                                }
                                // Do NOT remove from pending — the real tool result is still coming
                            }
                            _ = tokio::time::sleep_until(deadline) => {
                                tracing::warn!(
                                    remaining = pending.len(),
                                    "tool result deadline expired, synthesizing errors"
                                );
                                for (id, call) in pending.drain() {
                                    self.history.push(ChatMessage::tool_result(
                                        &call.id,
                                        &synthesize_timeout_error(&id),
                                    ));
                                }
                            }
                            _ = cancel.cancelled() => {
                                for (_id, call) in pending.drain() {
                                    self.history.push(ChatMessage::tool_result(
                                        &call.id,
                                        &synthesize_cancelled_error(&call.id),
                                    ));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
        // All exit paths (max-turns, cancellation, normal completion, single-actor)
        // converge here. Emit run_finished once, then drop channels to signal EOF.
        self.emit_run_finished().await;
        Ok(())
    }
}
```

**Drive loop helper functions:**

- `emit_text_content(text)` — sends `text_message_content` + `text_message_end` to `agui_tx`. Does NOT send `run_finished`. Used for both final text and truncated text.
- `emit_tool_attempt_to_agui(call)` — sends `tool_call_start` + `tool_call_end` to `agui_tx`. Single-actor only.
- `emit_run_finished()` — sends `run_finished` to `agui_tx`. Called once after the loop exits (all `break` paths converge), and also before `return Err(...)` on both the repeated-truncation path and the provider-error path.
- `extract_run_agent_input_messages(msg)` — parses the full `RunAgentInput.messages` array from the initial `JsonRpcMessage` into `Vec<ChatMessage>`. Maps each message by role: `system` → `ChatMessage::System`, `user` → `ChatMessage::User`, `assistant` → `ChatMessage::AssistantText`. All messages including system messages flow into history as-is. The `LlmProvider` implementations handle serialisation per their API format. **Returns `Err(EngineError::Driver)` if the message is malformed** (missing `params`, missing `messages` array, or unparseable role entries). This is a hard error because a silent fallback to empty history would drop all OATF-defined context.
- `extract_user_message(msg)` — extracts the **last user message** from a follow-up `RunAgentInput`. Used for follow-up turns only (not initial seeding). **Append-only invariant:** context-mode treats follow-ups as single new user turns appended to existing history. The follow-up `RunAgentInput` may contain the full messages array rebuilt from the new phase state, but the drive loop already owns the conversation history from prior turns, tool results, and LLM responses — re-seeding would discard that accumulated state. The helper extracts the last entry with `role: "user"` from `params.messages` and appends only that. If the OATF document modifies system messages or earlier turns in a later phase, those changes are not reflected in the LLM history. This is by design: the drive loop's history is the source of truth for the LLM conversation, and phase state changes to `run_agent_input.messages` beyond appending a new user message are silently ignored. Returns the text content as a `String`. If the message format is unexpected, logs a warning and returns the full serialized params as fallback.
- `format_server_request_as_user_message(method, params)` — formats a server-initiated request as a plain text string for injection into history as `ChatMessage::User`. For `elicitation/create`: `"[Server elicitation] {params.message}"`. For `sampling/createMessage`: `"[Server sampling request] {serialized params}"` (full params, not just `params.messages`, so the LLM sees the complete request context including model hints and metadata). The `[Server ...]` prefix makes the request origin visible to the LLM without adding enum complexity. Indicators match on the raw `ProtocolEvent` in the trace (which captures the full JSON-RPC request), not on the formatted history string.
- `handle_server_initiated_request(req, cancel)` — handles an elicitation or sampling request from a server actor by performing an LLM roundtrip:

```rust
async fn handle_server_initiated_request(
    &mut self,
    req: &ServerRequest,
    cancel: &CancellationToken,
) -> Result<JsonRpcMessage, EngineError> {
    let (method, params) = match &req.request {
        JsonRpcMessage::Request(r) => (r.method.as_str(), &r.params),
        _ => return Err(EngineError::Driver("expected Request in ServerRequest".into())),
    };

    // Add the server's request to history as a user message so the LLM sees it.
    // Format depends on method: elicitation shows the prompt, sampling shows the context.
    let prompt = format_server_request_as_user_message(method, params);
    self.history.push(ChatMessage::user(&prompt));

    // LLM roundtrip — no tools, just answer the elicitation/sampling prompt.
    // System messages already in history from seeding.
    let response = tokio::select! {
        result = self.provider.chat_completion(
            &self.history, &[], // Empty tools — answer the request
        ) => match result {
            Ok(res) => res,
            Err(e) => {
                return Err(EngineError::Driver(
                    format!("LLM error during {method}: {e}")
                ));
            }
        },
        _ = cancel.cancelled() => {
            return Err(EngineError::Driver("cancelled during server request".into()));
        }
    };

    // Extract text and build JSON-RPC response matching the request id
    let text = match response {
        LlmResponse::Text(t) => t.text,
        LlmResponse::ToolUse(_) => {
            tracing::warn!("LLM attempted tool use during {method}, using empty response");
            String::new()
        }
    };
    self.history.push(ChatMessage::assistant_text(&text));

    // Build response in the format the MCP server driver expects.
    // Differentiate by method to match upstream protocol formats.
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
        jsonrpc: JSONRPC_VERSION.to_string(),
        result: Some(result),
        error: None,
        id: request_id,
    }))
}
```

### 2.6 Tool Call to JSON-RPC Conversion

The drive loop converts `ToolCall` (from the LLM response) to a `JsonRpcMessage` appropriate for the owning actor's protocol. The conversion dispatches on `ActorMode`:

```rust
impl ContextTransport {
    fn tool_call_to_json_rpc(&self, call: &ToolCall, mode: ActorMode) -> JsonRpcMessage {
        // Use the LLM's tool_call.id as the JSON-RPC request id.
        // This preserves the correlation so tool_result can reference it.
        match mode {
            ActorMode::McpServer => {
                // MCP: standard tools/call request
                JsonRpcMessage::Request(JsonRpcRequest {
                    jsonrpc: JSONRPC_VERSION.to_string(),
                    method: "tools/call".to_string(),
                    params: Some(json!({
                        "name": call.name,
                        "arguments": call.arguments,
                    })),
                    id: json!(call.id),
                })
            }
            ActorMode::A2aServer => {
                // A2A: message/send request wrapping the tool call as a task message.
                // The A2A PhaseDriver (TJ-SPEC-017) dispatches on method "message/send"
                // and extracts the task content from params.message.
                JsonRpcMessage::Request(JsonRpcRequest {
                    jsonrpc: JSONRPC_VERSION.to_string(),
                    method: "message/send".to_string(),
                    params: Some(json!({
                        "message": {
                            "role": "user",
                            "parts": [{
                                "kind": "text",
                                "text": serde_json::to_string(&call.arguments)
                                    .expect("ToolCall.arguments is always valid JSON"),
                            }],
                        },
                        "metadata": {
                            "tool_name": call.name,
                        },
                    })),
                    id: json!(call.id),
                })
            }
            _ => unreachable!("only mcp_server and a2a_server actors in server_actors"),
        }
    }
}
```

**MCP path:** The MCP server driver receives a `tools/call` request and processes it via `dispatch_request()` unchanged.

**A2A path:** The A2A server driver receives a `message/send` request. The tool name is passed in `metadata.tool_name` so the PhaseDriver can match it against OATF state. The tool call arguments are serialized as the text part of the A2A message. The A2A driver's response is returned via `result_tx` as a standard `JsonRpcResponse`.

**Result normalization in `extract_result_content()`:**

| Protocol | Success response | Error response |
|----------|-----------------|----------------|
| MCP | `response.result` (the JSON value as-is) | `response.error` object preserved as-is |
| A2A | Concatenated text from `response.result.message.parts` where `part.kind == "text"`, joined with newlines. Non-text parts (files, data) are serialized as JSON. | `response.error` object preserved as-is |

**A2A normalization output shape:** The return value is always a `serde_json::Value::String`. Text parts are joined with `"\n"`. Non-text parts are appended as `"\n[{kind}]: {serde_json::to_string(part)}"`. Example: an A2A response with two text parts and one file part produces `"First text\nSecond text\n[file]: {\"kind\":\"file\",\"uri\":\"s3://...\"}"`. This ensures `ChatMessage::ToolResult.content` is always a single string value that serializes cleanly into both OpenAI and Anthropic message formats.

This normalization ensures that `ChatMessage::ToolResult.content` is always a `serde_json::Value` that the LLM can read, regardless of protocol origin. MCP results pass through unchanged; A2A results are flattened from the parts array into a text-primary representation.

**Tool definition extraction for A2A actors:** A2A Agent Cards expose `skills` with `id`, `name`, and `description`. The orchestrator's `extract_tool_definitions()` maps each A2A skill to a `ToolDefinition` with the skill name as the tool name and a permissive `parameters` schema (since A2A skills don't define typed input schemas the way MCP tools do).

### 2.7 ContextAgUiDriver

The traffic-mode `AgUiDriver` (TJ-SPEC-016) uses a concrete `AgUiTransport` with HTTP-specific methods (`send_run()` returns an SSE stream). It cannot work with `AgUiHandle` which implements the generic `Transport` trait. Context-mode requires a separate `ContextAgUiDriver` implementing `PhaseDriver`.

```rust
pub struct ContextAgUiDriver {
    transport: Box<dyn Transport>,  // AgUiHandle in context-mode
    thread_id: String,              // Shared with ContextTransport, passed by orchestrator
}

impl ContextAgUiDriver {
    pub fn new(transport: Box<dyn Transport>, thread_id: String) -> Self {
        Self {
            transport,
            thread_id,
        }
    }
}

#[async_trait]
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

        // Build RunAgentInput from state (same as AgUiDriver)
        let input = build_run_agent_input(state, &current_extractors, &self.thread_id)?;
        let input_value = serde_json::to_value(&input)
            .map_err(|e| EngineError::Driver(format!("serialize RunAgentInput: {e}")))?;

        // Emit outgoing request event (same as AgUiDriver)
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "run_agent_input".to_string(),
            content: input_value.clone(),
        }).await;

        // Send via Transport trait (AgUiHandle forwards to agui_response_tx)
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "run_agent_input".to_string(),
            params: Some(input_value),
            id: json!(uuid::Uuid::new_v4().to_string()),
        });
        self.transport.send_message(&msg).await
            .map_err(|e| EngineError::Driver(format!("send RunAgentInput: {e}")))?;

        // Receive events from drive loop via transport.receive_message()
        // (ContextTransport sends AG-UI events to agui_tx → AgUiHandle.rx)
        loop {
            tokio::select! {
                result = self.transport.receive_message() => {
                    match result {
                        Ok(Some(msg)) => {
                            let (method, content) = extract_event_from_message(&msg);
                            let _ = event_tx.send(ProtocolEvent {
                                direction: Direction::Incoming,
                                method,
                                content,
                            }).await;
                        }
                        Ok(None) => return Ok(DriveResult::TransportClosed),
                        Err(e) => {
                            tracing::warn!(error = %e, "receive error");
                            return Ok(DriveResult::TransportClosed);
                        }
                    }
                }
                _ = cancel.cancelled() => return Ok(DriveResult::Complete),
            }
        }
    }
}
```

**Required visibility change:** `build_run_agent_input()` in `src/protocol/agui.rs` is currently private (`fn`). It must become `pub(crate)` so `ContextAgUiDriver` (in `src/protocol/context_agui.rs`, same crate) can call it. Alternatively, `ContextAgUiDriver` can live in `src/protocol/agui.rs` alongside the existing driver — this avoids the visibility change but couples the two drivers in one file.

**Shared `thread_id`:** The orchestrator generates one UUID and passes it to both `ContextTransport` and `ContextAgUiDriver::new()`. This ensures the `threadId` in `run_finished` events matches the `threadId` in `RunAgentInput` trace entries — mirroring traffic-mode where `AgUiTransport` owns the single source of truth.

The `ContextAgUiDriver` reuses `build_run_agent_input()` from the existing AG-UI module. The key difference from `AgUiDriver`: instead of sending HTTP POST and parsing SSE, it sends via `Transport::send_message()` (which forwards to the drive loop's `agui_response_rx`) and receives events via `Transport::receive_message()` (which reads from `agui_tx`). PhaseLoop processes events through the same extractor/trigger/trace path regardless of which driver produced them.

**`extract_event_from_message()`** maps `JsonRpcNotification` method names to `ProtocolEvent` fields. The drive loop emits notifications using OATF event names (see event vocabulary below), and this helper passes them through unchanged.

### 2.8 AG-UI Event Vocabulary

The canonical AG-UI event vocabulary for context-mode (snake_case per OATF section 7.3):

| Event | Required fields | Emitted when |
|-------|----------------|--------------|
| `text_message_content` | `messageId`, `delta` | LLM generates text |
| `text_message_end` | `messageId` | After `text_message_content` |
| `tool_call_start` | `toolCallId`, `name`, `arguments` | LLM requests a tool call (single-actor only) |
| `tool_call_end` | `toolCallId` | After `tool_call_start` (single-actor only) |
| `run_finished` | `threadId`, `runId` | Conversation complete |

**No other event names are used.** All references to `text_message` or `tool_use` elsewhere in this spec are shorthand for the sequences above.

**ID generation:** `threadId` is a UUID generated once by the orchestrator and shared between `ContextTransport` and `ContextAgUiDriver` (stable across all turns and trace entries). `runId` is a UUID generated once per drive loop invocation. `messageId` is a UUID generated per logical message (shared across the `text_message_content` and `text_message_end` pair for that message). `toolCallId` is a UUID generated per logical tool call (shared across the `tool_call_start` and `tool_call_end` pair for that call). All are auto-generated; none need to be deterministic for tests (indicators match on content, not IDs).

**Note on delta field:** In traffic-mode SSE streams, `text_message_content` events arrive as multiple chunks with incremental deltas. In context-mode, the full text is sent as a single `delta` in one event. This is valid — the `ContextAgUiDriver`'s event handling and PhaseLoop's accumulation logic handle both single and multi-chunk content correctly since extractors/triggers operate on individual events.

**Single-actor tool call events:** When there is no server actor, `tool_call_start` and `tool_call_end` are emitted for the LLM's tool call attempts but no tool execution occurs between them. After emitting, the drive loop waits on `agui_response_rx` for a follow-up (same as the text path). This allows the AG-UI actor to react to tool attempts with a follow-up turn. Indicators should match on `name` and `arguments` in `tool_call_start` to detect what the model attempted.

```rust
// emit_text_content(): text_message_content + text_message_end (no run_finished)
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

// emit_tool_attempt_to_agui(): tool_call_start + tool_call_end (single-actor only)
let tc_id = Uuid::new_v4().to_string();
let start_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
    "tool_call_start",
    Some(json!({ "toolCallId": tc_id, "name": call.name, "arguments": call.arguments })),
));
let end_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
    "tool_call_end",
    Some(json!({ "toolCallId": tc_id })),
));
let _ = self.agui_tx.send(start_notif).await;
let _ = self.agui_tx.send(end_notif).await;

// emit_run_finished(): called once post-loop or before error return
let finish_notif = JsonRpcMessage::Notification(JsonRpcNotification::new(
    "run_finished",
    Some(json!({ "threadId": self.thread_id, "runId": self.run_id })),
));
let _ = self.agui_tx.send(finish_notif).await;
```

### 2.9 Tool Definition Synchronization

Each server actor gets its own `watch::channel` for tool definitions. The orchestrator creates the channel per actor (see S2.4 step 6c) and passes the `Sender` to that actor's `PhaseLoopConfig.tool_watch_tx`.

**Required change to `PhaseLoopConfig`** in `src/engine/phase_loop.rs`:

```rust
pub struct PhaseLoopConfig {
    // ... existing fields ...
    pub tool_watch_tx: Option<watch::Sender<Vec<ToolDefinition>>>,  // NEW
}
```

PhaseLoop publishes after `advance_phase()`:

```rust
if let Some(ref tx) = self.tool_watch_tx {
    let effective = self.phase_engine.compute_effective_state();
    let tools = extract_tool_definitions(&effective);
    let _ = tx.send(tools);
}
```

Traffic-mode passes `tool_watch_tx: None` (no behaviour change).

**Debuggability note:** When a phase advance occurs mid-conversation (rug pull), the transition is visible in two places: the `--events-file` stream contains a phase transition event with timestamps (TJ-SPEC-008), and the `--export-trace` includes a `phase` field on every trace entry — consecutive entries with different phase names indicate where the transition occurred.

### 2.10 Trace Entry Flow

**Trace entries are NOT emitted by `ContextTransport` or handles.** The `Transport` trait has no access to `SharedTrace`, `actor_name`, or `current_phase`. All trace entries flow through PhaseLoop's `process_protocol_event()` via driver-emitted `ProtocolEvent` objects.

**Text-only responses**: `ContextTransport` sends AG-UI-formatted notifications (`text_message_content`, `text_message_end`) to `agui_tx` via `emit_text_content()`. The `ContextAgUiDriver` receives these via `transport.receive_message()` and emits them as `ProtocolEvent`s. If a trigger matches and the phase advances, PhaseLoop calls `transport.send_message()` with the follow-up user message — this arrives on `agui_response_rx` in the drive loop, which appends it to history and continues. After the drive loop exits, `emit_run_finished()` sends `run_finished`. PhaseLoop records all events in the trace. `receive_message()` returning `None` means strictly "channel closed".

**Tool call attempts (single-actor)**: `ContextTransport` sends AG-UI-formatted `tool_call_start` / `tool_call_end` notifications per S2.8 so indicators can detect what the model attempted.

### 2.11 Behavioural Modifier Handling

The `Transport` trait does not have a `supports_behavior` method. Behaviours are handled in `apply_delivery()` in `src/engine/mcp_server/behavior.rs`.

**Required change** -- add guard in `"slow_stream"` branch:

```rust
"slow_stream" => {
    if transport.transport_type() == TransportType::Context {
        tracing::warn!("slow_stream delivery not supported in context-mode, using normal");
        transport.send_message(response_msg).await
            .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
        return Ok(None);
    }
    // ... existing slow_stream implementation ...
}
```

Other modes work unchanged: `"delayed"` (sleep + `send_message()`), `"unbounded"` (inflate + `send_message()`), `"normal"`.

### 2.12 Traffic-Mode Parity Notes

Context-mode runs the same PhaseDriver, PhaseEngine, and verdict code as traffic-mode. The following behaviours are the known differences:

**Transport-layer attacks not simulated:** `slow_stream` (byte dripping), `unbounded_line` (oversized frames), and `pipe_deadlock` (stdin/stdout contention) require a real wire to manipulate. In context-mode, `slow_stream` falls back to normal delivery with a warning (S2.11). The other two are physically impossible — there is no pipe or TCP connection. OATF scenarios whose success criteria depend on transport-layer effects (e.g., measuring whether a client disconnects under slow delivery) should use traffic-mode.

**Notification-triggered framework behaviour not simulated:** In traffic-mode, MCP notifications (`notifications/resources/updated`, `notifications/message`, progress, logging) are sent to the real agent framework via the wire. The framework may react autonomously — e.g., on `resources/updated`, the framework might call `resources/read` back to ThoughtJack, receive updated (possibly poisoned) content, and inject it into the LLM's context. The LLM never sees the notification itself; it sees the *consequence* (updated resource content on its next turn). In context-mode, notifications reach the `ServerHandle` and are forwarded to `result_tx` as `JsonRpcMessage::Notification`. The drive loop's result collection has an explicit `Notification` branch that discards them with a `trace!`-level log — they do not affect the pending count or conversation history. No autonomous re-read occurs because there is no framework to trigger one. This means OATF scenarios that rely on notification-driven framework behaviour (e.g., testing whether poisoned resource content reaches the LLM via an autonomous re-read) should use traffic-mode. Scenarios that test whether the LLM complies with poisoned content work correctly in context-mode — the poisoned content arrives via tool responses and phase advancement, which operate identically across both modes.

---

## 3. LLM Provider

### 3.1 Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_completion(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError>;
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;
}

pub enum LlmResponse {
    Text(TextResponse),
    ToolUse(Vec<ToolCall>),
}

pub struct TextResponse {
    pub text: String,
    /// True when finish_reason was 'length' / stop_reason was 'max_tokens'
    pub is_truncated: bool,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}
```

**Finish reason handling:** Each `LlmProvider` implementation must check `finish_reason` (OpenAI) / `stop_reason` (Anthropic) in the API response. On `length` / `max_tokens` (truncation), the provider returns `LlmResponse::Text` with the partial content and logs a warning — it does not attempt to parse truncated tool call JSON. The drive loop handles repeated truncation per EC-CTX-014.

```rust
/// Conversation message for the LLM API.
/// Serialized differently per provider (OpenAI vs Anthropic).
pub enum ChatMessage {
    System(String),
    User(String),
    /// Text-only assistant response
    AssistantText(String),
    /// Assistant response requesting tool calls
    AssistantToolUse { tool_calls: Vec<ToolCall> },
    /// Result of a tool call, keyed by tool_call_id
    ToolResult { tool_call_id: String, content: serde_json::Value },
}

impl ChatMessage {
    pub fn assistant_text(text: &str) -> Self { Self::AssistantText(text.to_string()) }
    pub fn assistant_tool_use(calls: &[ToolCall]) -> Self {
        Self::AssistantToolUse { tool_calls: calls.to_vec() }
    }
    pub fn tool_result(tool_call_id: &str, response: &JsonRpcMessage) -> Self {
        // Extract the result content from the JsonRpcResponse
        Self::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: extract_result_content(response),
        }
    }
    pub fn user(text: &str) -> Self { Self::User(text.to_string()) }
}

/// Tool definition in LLM-native format (OpenAI function calling schema).
/// Converted from OATF tool state by extract_tool_definitions().
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema from OATF inputSchema
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {status} {body}")]
    Http { status: u16, body: String },
    #[error("Request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Rate limited after {retries} retries")]
    RateLimited { retries: u32 },
    #[error("Timeout after {seconds}s")]
    Timeout { seconds: u64 },
}
```

**Provider-specific serialization**: `ChatMessage` and `ToolDefinition` serialize differently per provider. Each provider's `chat_completion()` inspects the history array and handles serialisation internally — the `ChatMessage` enum is provider-agnostic. Key differences: `ChatMessage::System` — the `OpenAiCompatibleProvider` emits `{role: "system", content: "..."}` messages in the array (OpenAI supports multiple system messages). The `AnthropicProvider` extracts all `System` entries, concatenates their text, and passes it as the top-level `system` parameter (Anthropic requires system prompt outside the messages array). `ChatMessage::ToolResult` — OpenAI: `{role: "tool", tool_call_id: "..."}`. Anthropic: `tool_result` content block inside a `user` role message.

### 3.2 Implementations

**OpenAI-compatible** (covers OpenAI, Groq, Together, DeepSeek, Ollama, vLLM):

```rust
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: f32,      // Default: 0.0
    max_tokens: Option<u32>,
}
```

**Anthropic native** (Messages API):

```rust
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    temperature: f32,
    max_tokens: u32,       // Required by Anthropic API
}
```

### 3.3 Configuration

Runtime-only (not in OATF documents).

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `THOUGHTJACK_CONTEXT_PROVIDER` | No | `openai` | Provider type: `openai`, `anthropic` |
| `THOUGHTJACK_CONTEXT_API_KEY` | Conditional | -- | API key. Required for hosted providers (OpenAI, Anthropic, Groq, etc.). May be omitted or set to any value for local providers (Ollama, vLLM) that don't authenticate. |
| `THOUGHTJACK_CONTEXT_MODEL` | Yes | -- | Model identifier |
| `THOUGHTJACK_CONTEXT_BASE_URL` | No | Provider default | Base URL override |
| `THOUGHTJACK_CONTEXT_TEMPERATURE` | No | `0.0` | Sampling temperature |
| `THOUGHTJACK_CONTEXT_MAX_TOKENS` | No | `4096` | Max tokens per response |
| `THOUGHTJACK_CONTEXT_SYSTEM_PROMPT` | No | None (blank) | System prompt simulating agent framework instructions |
| `THOUGHTJACK_CONTEXT_TIMEOUT` | No | `120` | Per-request timeout (seconds) |

CLI flags `--context-*` override environment variables. **System prompt handling:** System messages live in history alongside other messages. If `--context-system-prompt` is set, it is prepended as the first `ChatMessage::System` during initial seeding (before OATF messages). OATF system messages from `run_agent_input.messages` follow in their original position. The `LlmProvider` handles serialisation — OpenAI supports system messages in the array; Anthropic extracts and concatenates them into the top-level `system` parameter. The default for `--context-system-prompt` is blank (not set) — benchmarks are not polluted with instructions the user didn't choose. If neither CLI nor OATF defines a system message, the LLM receives no system prompt.

---

## 4. Context-Mode Activation

### 4.1 CLI Flag

Activated via `--context` on `thoughtjack run`:

```bash
thoughtjack run attack.yaml --context \
  --context-model gpt-4o \
  --context-api-key $OPENAI_API_KEY
```

**Validation:** at least one `ag_ui_client` actor required (error with hint if absent); `--context-model` required; `--context-api-key` required unless `--context-base-url` points to a local or private address (`localhost`, `127.0.0.1`, `0.0.0.0`, `host.docker.internal`, or any `10.*`, `172.16-31.*`, `192.168.*` range), in which case it defaults to `"no-key"`. Users can also set `--context-api-key ""` or `--context-api-key no-key` explicitly to bypass the requirement for other non-authenticated endpoints. Traffic-mode flags ignored with warning.

### 4.2 Actor Routing

| Actor mode | Context-mode routing |
|-----------|---------------------|
| `ag_ui_client` | Receives messages via `AgUiHandle` |
| `mcp_server` | Receives tool calls via `ServerHandle` (one per actor) |
| `a2a_server` | Receives delegations via `ServerHandle` (one per actor) |
| `mcp_client` | Error: not supported |
| `a2a_client` | Error: not supported |

Multiple `mcp_server` and `a2a_server` actors are supported. Each gets its own `ServerHandle` with a dedicated `server_rx` channel. Tool calls are routed by tool name — the drive loop builds a `tool_name → actor_name` routing table per turn from the merged tool definitions (see S2.5).

**v1 constraint:** At most one `ag_ui_client` actor. Documents with multiple `ag_ui_client` actors are rejected with an error.

Readiness gate: server actors immediately ready (no port binding). History is seeded from the initial `RunAgentInput.messages` array (preserving system prompts, prior turns, and context), then grows as the conversation progresses with follow-up user messages, LLM responses, and tool results. `--max-turns` (TJ-SPEC-007) provides turn ceiling; defaults to `20` in context-mode if unset. Verdict output includes `transport: "context"`, `context_provider`, `context_model` in `execution_summary`.

---

## 5. Edge Cases

### EC-CTX-001: LLM calls unknown tool
MCP server PhaseDriver returns JSON-RPC error via `dispatch_request()` (e.g., `{"error": {"code": -32601, "message": "tool not found: foo"}}`). `ServerHandle.send_message()` forwards this to `tool_result_rx`. `extract_result_content()` must handle both success responses (extracts `result` field) and error responses (extracts `error` object as-is). The error is preserved in `ChatMessage::ToolResult.content` so the LLM sees the failure. Conversation continues.

### EC-CTX-002: Empty LLM response
`emit_text_content()` sends `text_message_content` with empty delta + `text_message_end`. Drive loop waits on `agui_response_rx` for follow-up (5s timeout). If no follow-up, loop breaks. `emit_run_finished()` fires post-loop. Verdict evaluates with captured trace.

### EC-CTX-003: HTTP 429
Exponential backoff (1s, 2s, 4s), max 3 retries. Then `EngineError`.

### EC-CTX-004: HTTP 401/403
Fail immediately. No retry.

### EC-CTX-005: Multiple tool calls
Each call routed to the owning actor via `tool_router`. Calls to unroutable tools get synthesized errors inline. All routed calls collected via `tool_result_rx` before next API call.

### EC-CTX-006: Phase advances mid-conversation
Watch channel updates. Next API call uses new tool definitions.

### EC-CTX-007: --max-turns reached
Drive loop exits. Channels close. Handles return `None`. Verdict evaluates immediately with captured trace. Grace period is not applicable in context-mode — there is no open transport to observe for delayed effects. If the OATF document specifies `grace_period`, it is ignored with a warning in context-mode.

### EC-CTX-008: Malformed provider JSON
Retry once. Then `EngineError`.

### EC-CTX-009: Single AG-UI actor, no tools
Valid. Tool call attempts and text sent as notifications to AG-UI handle.

### EC-CTX-010: Network timeout
120s timeout (configurable). `EngineError` on expiry.

### EC-CTX-011: Context window exceeded
Log token estimate. `EngineError`. No retry.

### EC-CTX-012: Invalid tool call arguments
Passed through to MCP server driver without validation.

### EC-CTX-013: Server handle active after drive loop exit
Drive loop dropped `tool_result_rx`. Server's `send_message()` returns `Err(ConnectionClosed)`.

### EC-CTX-014: LLM response truncated due to max_tokens
The API returns HTTP 200 but `finish_reason` is `length` (OpenAI) or `stop_reason` is `max_tokens` (Anthropic). The response may contain truncated tool call JSON (e.g., `{"name": "search", "arguments": {"query": "renew`). `LlmProvider` must check `finish_reason` / `stop_reason` and distinguish `stop` (normal), `tool_use` (tool calling), and `length` (truncation). On truncation: log a warning with the truncated content, do not attempt to parse as tool calls, return `LlmResponse::Text` with `is_truncated: true`. The drive loop calls `emit_text_content()` (`text_message_content` + `text_message_end`), appends a "Please continue" user message, and `continue`s to give the model another turn. If truncation happens 2+ consecutive turns, the drive loop calls `emit_run_finished()` and terminates with `EngineError::Driver("Repeated truncation — increase --context-max-tokens")`. The `consecutive_truncations` counter tracks this.

### EC-CTX-015: Tool result timeout (server unresponsive)
The server PhaseDriver crashes, drops a tool call, or returns an unmatchable id. The drive loop's absolute deadline (30s per pending call, computed once at the start of result collection) fires via `tokio::time::sleep_until`. All remaining pending tool calls receive synthesized error results (`{"error": "tool result deadline expired"}`). These are appended to history so the LLM can see the failure and respond accordingly. The conversation continues — the next API call includes the timeout errors as tool results. Because the deadline is absolute, interleaved server-initiated requests (elicitation/sampling) cannot extend the wait — the LLM roundtrip time for those requests counts against the same budget.

### EC-CTX-016: Late tool result arrival after timeout
If a timed-out server eventually sends the real tool result during a subsequent turn, it arrives on `tool_result_rx` and is pulled by `recv()`. The `HashMap` id check fails (the id was already drained during the timeout), triggering the `"unexpected tool result id"` warning. The stale result is safely discarded. This is correct behaviour — no special handling needed.

### EC-CTX-017: Tool name collision across server actors
Two server actors expose a tool with the same name (e.g., both `mcp_poison` and `a2a_skill` define `search`). The first actor in document order wins the routing entry. The second actor's duplicate is excluded from the merged tool list with a warning: `"duplicate tool name across actors, first actor wins"`. The LLM only sees one `search` tool. This mirrors real-world behaviour when agents connect to multiple servers with overlapping tool names.

### EC-CTX-018: LLM calls tool with no owning actor (unroutable)
The LLM generates a tool call for a name not in `tool_router` — either the tool was hallucinated or it existed in a previous phase but was removed by a rug pull. A synthesized error result is appended to history inline (not routed to any server): `{"error": "no server actor owns tool: <name>"}`. The error is visible to the LLM on the next turn. No pending entry is created, so it does not affect the result collection loop.

### EC-CTX-019: Orchestrator cancellation during blocking operation
The orchestrator fires `CancellationToken` (e.g., global `THOUGHTJACK_CONTEXT_TIMEOUT` exceeded) while the drive loop is blocked. All blocking waits are cancellation-aware via `tokio::select!`: the initial 30s AG-UI message wait, `provider.chat_completion()`, tool result collection, the 5s follow-up waits, and server-initiated request LLM roundtrips. On cancellation during the provider call: the in-flight HTTP request is dropped, the drive loop breaks. On cancellation during tool result collection: synthesized error results are inserted for all remaining pending calls, the while loop breaks, and the outer loop exits at the next `cancel.is_cancelled()` check. On cancellation during follow-up waits: the drive loop breaks immediately. In all cases, `emit_run_finished()` fires post-loop.

### EC-CTX-020: AG-UI follow-up timeout (no more phases)
After the LLM returns non-truncated text and the drive loop emits `text_message_content` + `text_message_end`, it waits 5 seconds on `agui_response_rx` for a follow-up user message. If the AG-UI actor has no more phases to advance (all triggers exhausted), it will not call `transport.send_message()`. The `agui_response_tx` remains open (the AG-UI PhaseLoop hasn't exited yet), so `recv()` blocks until the 5-second timeout fires. On timeout: the drive loop breaks and `emit_run_finished()` fires post-loop. This is the normal termination path for single-phase AG-UI scenarios. If the AG-UI PhaseLoop has already exited and dropped `agui_response_tx`, `recv()` returns `None` immediately — same result, faster.

### EC-CTX-021: Server-initiated request during tool dispatch (elicitation/sampling)
During tool call result collection, a server actor sends an elicitation or sampling request via `ServerHandle.send_message()`. The `JsonRpcMessage::Request` is routed to `server_request_tx` → `server_request_rx` (not `result_tx`). The drive loop receives it in the `select!`, calls `handle_server_initiated_request()` which adds the request to history, performs an LLM roundtrip with empty tools, extracts the text response, and builds a `JsonRpcResponse`. The response is sent back to the originating actor via `server_actors[actor_name].tx` → `ServerHandle.rx`. The MCP server driver's `receive_message()` picks it up and continues tool dispatch. The pending tool result count is unchanged — the actual tool result arrives later on `tool_result_rx` as normal. If the LLM returns `ToolUse` instead of text during the elicitation roundtrip, the tool calls are ignored and an empty response is returned with a warning. **Why no MCP server driver changes are needed:** `send_server_request()`'s stdio fallback path already handles interleaved messages — it buffers unexpected `JsonRpcRequest` messages into `deferred_requests` and unexpected `JsonRpcResponse` messages into `buffered_server_responses` while waiting for the matching response id. In context-mode, `ServerHandle` provides the same `send_message()`/`receive_message()` interface, so the existing buffering logic works unchanged.

### EC-CTX-022: AG-UI actor fails to send initial message
The drive loop waits 30 seconds on `agui_response_rx` for the initial `RunAgentInput` from the `ContextAgUiDriver`, with `cancel.cancelled()` in the `select!`. If the AG-UI actor's PhaseLoop fails to start (bad state, missing `run_agent_input`), the channel may close (`Ok(None)`) or the timeout fires (`Err`). On `Ok(None)`: `emit_run_finished()` fires, drive loop returns `Ok(())`. On timeout: `emit_run_finished()` fires, drive loop returns `EngineError::Driver("AG-UI actor did not send initial message within 30s")`. On cancellation: `emit_run_finished()` fires, drive loop returns `Ok(())`.

---

## 6. Required Changes to Existing Code

| File / Spec | Change |
|-------------|--------|
| `src/transport/mod.rs` | Add `TransportType::Context` variant and `Display` arm |
| `src/engine/phase_loop.rs` (`PhaseLoopConfig`) | Add `pub tool_watch_tx: Option<watch::Sender<Vec<ToolDefinition>>>` |
| `src/engine/phase_loop.rs` (`PhaseLoop`) | Publish tool definitions on watch channel after `advance_phase()` when `Some` |
| `src/engine/mcp_server/behavior.rs` (`apply_delivery`) | Add `TransportType::Context` guard in `"slow_stream"`, fall back to normal with warning |
| `src/orchestration/orchestrator.rs` (`orchestrate_context`) | New function: create per-actor `ServerHandle`s with watch channels and shared `result_tx`/`server_request_tx`; AG-UI actor spawned directly, server actors via `run_context_server_actor()` |
| `src/orchestration/runner.rs` (`ActorConfig`) | Add context-mode provider fields |
| `src/protocol/agui.rs` | Change `build_run_agent_input()` visibility from `fn` to `pub(crate) fn` |
| `src/protocol/context_agui.rs` (NEW) | `ContextAgUiDriver` implementing `PhaseDriver` for AG-UI actors in context-mode |
| `src/transport/context.rs` (`ServerHandle`) | Route `JsonRpcMessage::Request` to `server_request_tx`, `Response`/`Notification` to `result_tx` |
| TJ-SPEC-014 verdict output | Add `transport`, `context_provider`, `context_model` to `execution_summary` |

---

## 7. Definition of Done

- [ ] `ContextTransport` with drive loop implemented and tested
- [ ] Drive loop waits for initial AG-UI message (30s timeout, cancellation-aware) before entering turn loop (EC-CTX-022)
- [ ] Initial history seeded from full `RunAgentInput.messages` array (system, user, assistant messages preserved)
- [ ] Malformed initial `RunAgentInput` returns `EngineError::Driver` (not silent fallback)
- [ ] Follow-up turns extract only the last user message (append-only invariant documented; not runtime-validated)
- [ ] `ContextAgUiDriver` implements `PhaseDriver` using `Transport` trait (not `AgUiTransport`)
- [ ] `ContextAgUiDriver` has `thread_id` field (shared with `ContextTransport`, passed by orchestrator)
- [ ] `ContextAgUiDriver` returns `DriveResult::TransportClosed` on channel close (not `Complete`)
- [ ] `ContextAgUiDriver` reuses `build_run_agent_input()` from AG-UI module
- [ ] `build_run_agent_input()` visibility changed to `pub(crate)` in `src/protocol/agui.rs`
- [ ] AG-UI actor: `ContextAgUiDriver` constructed and `PhaseLoop` spawned directly by orchestrator (not via `run_actor()`)
- [ ] `AgUiHandle` implements `Transport` trait
- [ ] `AgUiHandle.send_message()` forwards follow-up messages to `agui_response_tx` (not a no-op)
- [ ] `agui_response_rx` channel created and wired between `AgUiHandle` and `ContextTransport`
- [ ] Drive loop waits for AG-UI follow-up (5s timeout) after non-truncated text response
- [ ] AG-UI follow-up appended to history as `ChatMessage::User` and loop continues
- [ ] Channel close or timeout on `agui_response_rx` causes loop break (conversation ends)
- [ ] `ServerHandle` implements `Transport` trait with result channel
- [ ] Both handles inherit default `capture_raw_writer()` returning `Ok(None)`
- [ ] Multiple `ServerHandle` instances share one `result_tx` (cloned)
- [ ] `TransportType::Context` added to enum
- [ ] Per-turn tool routing table built from merged `server_tool_watches`
- [ ] Tool name collisions resolved first-actor-wins with warning (EC-CTX-017)
- [ ] Duplicate tools excluded from `all_tools` (not just from `tool_router`)
- [ ] Unroutable tool calls get synthesized error inline (EC-CTX-018)
- [ ] No trace emission from transport or handles -- all via PhaseLoop
- [ ] AG-UI events use canonical vocabulary only: `text_message_content`, `text_message_end`, `tool_call_start`, `tool_call_end`, `run_finished`
- [ ] `emit_text_content()` sends content + end only (no `run_finished`)
- [ ] `emit_run_finished()` called once post-loop (all `break` paths converge) and before `return Err(...)` on both repeated-truncation and provider-error paths
- [ ] `tool_call_start` includes `arguments` field
- [ ] Single-actor `tool_call_start`/`tool_call_end` events indicate attempt only (no execution)
- [ ] Single-actor tool-use path waits on `agui_response_rx` for follow-up (not immediate break)
- [ ] `server_tool_watches` populated in OATF document order (ordering invariant for EC-CTX-017)
- [ ] `--context-api-key` conditionally required: omitted for local/private endpoints (localhost, private ranges, `host.docker.internal`)
- [ ] AG-UI IDs: `threadId` per transport, `runId` per drive loop, `messageId` per logical message (shared across content/end pair), `toolCallId` per logical tool call (shared across start/end pair)
- [ ] `LlmProvider` trait with OpenAI-compatible and Anthropic providers
- [ ] `ChatMessage`, `ToolDefinition`, `ProviderError` types defined
- [ ] `extract_result_content()` preserves JSON-RPC error objects (not just success results)
- [ ] `extract_result_content()` normalizes A2A responses: concatenates text parts with newlines, non-text parts as `[{kind}]: {json}`, returns `Value::String`
- [ ] A2A `serde_json::to_string` on tool arguments uses `expect` (not `unwrap_or_default`)
- [ ] `tool_call_to_json_rpc()` dispatches on `ActorMode` — MCP `tools/call` vs A2A `message/send`
- [ ] A2A skills extracted as `ToolDefinition` from Agent Card during construction
- [ ] System messages live in history; CLI system prompt prepended during seeding if set; `LlmProvider` handles serialisation per API; blank CLI default
- [ ] Tool results matched by JSON-RPC `id` field, not FIFO order
- [ ] Absolute deadline (30s per pending call) prevents deadlock; interleaved server requests cannot extend wait
- [ ] All blocking waits cancellation-aware: initial 30s, provider call, result collection, 5s follow-ups
- [ ] Channel close (`Ok(None)`) synthesizes error results for remaining pending calls (same as timeout)
- [ ] `ContextAgUiDriver` processes context-mode events via `Transport::receive_message()` and feeds PhaseLoop
- [ ] `LlmProvider` checks `finish_reason` / `stop_reason` for truncation (EC-CTX-014)
- [ ] `consecutive_truncations` counter terminates on 2+ consecutive truncations
- [ ] Truncated responses append "Please continue" and `continue` (not `break`)
- [ ] Tool definitions watch channels seeded with initial phase state per actor before `spawn_drive_loop()`
- [ ] `--context` flag activates context-mode
- [ ] Provider config via CLI flags and env vars
- [ ] AG-UI actor required validation; at most one `ag_ui_client`
- [ ] Multiple `mcp_server` and `a2a_server` actors supported
- [ ] `ServerHandle.send_message()` routes `Request` to `server_request_tx`, `Response`/`Notification` to `result_tx`
- [ ] `server_request_rx` channel created and wired between `ServerHandle`s and `ContextTransport`
- [ ] `handle_server_initiated_request()` performs LLM roundtrip for elicitation/sampling
- [ ] Response routed back to originating actor via `server_actors[actor_name].tx`
- [ ] Elicitation/sampling scenarios work end-to-end in context-mode (EC-CTX-021)
- [ ] `format_server_request_as_user_message()` formats elicitation/sampling requests as prefixed user messages
- [ ] Orchestrator generates one `thread_id` UUID shared between `ContextTransport` and `ContextAgUiDriver`
- [ ] `PhaseLoopConfig.tool_watch_tx` added; PhaseLoop publishes on advance
- [ ] `apply_delivery()` guards `slow_stream` on `TransportType::Context`
- [ ] `--max-turns` defaults to 20 in context-mode when unset
- [ ] Grace period ignored with warning in context-mode
- [ ] Multi-actor (multiple servers) and single-actor context-mode tested
- [ ] `execution_summary` includes transport and provider attribution
- [ ] All 22 edge cases (EC-CTX-001 through EC-CTX-022) have tests
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [TJ-SPEC-002: Transport Abstraction](./TJ-SPEC-002_Transport_Abstraction.md)
- [TJ-SPEC-013: OATF Integration](./TJ-SPEC-013_OATF_Integration.md)
- [TJ-SPEC-014: Verdict & Evaluation](./TJ-SPEC-014_Verdict_Evaluation_Output.md)
- [TJ-SPEC-015: Multi-Actor Orchestration](./TJ-SPEC-015_Multi_Actor_Orchestration.md)
- [TJ-SPEC-016: AG-UI Protocol Support](./TJ-SPEC-016_AGUI_Protocol_Support.md)
- [TJ-SPEC-017: A2A Protocol Support](./TJ-SPEC-017_A2A_Protocol_Support.md)
- [TJ-SPEC-018: MCP Client Mode](./TJ-SPEC-018_MCP_Client_Mode.md) -- Multiplexer precedent
- [OpenAI Chat Completions API](https://platform.openai.com/docs/api-reference/chat)
- [Anthropic Messages API](https://docs.anthropic.com/en/api/messages)
