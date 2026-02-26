# TJ-SPEC-015: Multi-Actor Orchestration

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-015` |
| **Title** | Multi-Actor Orchestration |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | High |
| **Version** | v1.0.0 |
| **Depends On** | TJ-SPEC-013 (OATF Integration), TJ-SPEC-014 (Verdict & Evaluation Output) |
| **Required By** | TJ-SPEC-016 (AG-UI Protocol Support), TJ-SPEC-017 (A2A Protocol Support) |
| **Tags** | `#orchestration` `#multi-actor` `#cross-protocol` `#concurrency` `#readiness` |

## 1. Context

### 1.1 Motivation

Everything before this spec runs one actor at a time. TJ-SPEC-013 runs one MCP server. TJ-SPEC-016 will run one AG-UI client. TJ-SPEC-017 will run one A2A server or client. None of them can run *together*.

Cross-protocol attacks require concurrent actors. A rug pull driven by AG-UI needs an MCP server accepting tool calls *while* the AG-UI client sends messages. An A2A skill poisoning attack with MCP tool exploitation needs an A2A server serving an Agent Card *while* an MCP server serves poisoned tools *while* an AG-UI client drives the agent to discover and use both.

This spec defines the orchestration layer that manages multiple concurrent actors, enforces readiness ordering, shares extractors across actor boundaries, merges protocol traces, and coordinates shutdown.

### 1.2 Scope

This spec covers:

- Actor lifecycle management (spawn, ready, run, terminate)
- Readiness semantics (OATF §5.1: servers before clients)
- Cross-actor extractor store (shared, thread-safe)
- Merged protocol trace with actor attribution
- Coordinated shutdown (all actors terminate when attack completes)
- Actor failure handling (partial completion, error propagation)
- Concurrency model (async tasks, channels)
- CLI integration for multi-actor documents

This spec does **not** cover:

- Protocol-specific transport or handler logic (TJ-SPEC-013, 016, 017)
- Verdict computation (TJ-SPEC-014 — orchestrator provides the merged trace, verdict pipeline consumes it)
- Single-actor execution (existing specs handle this; orchestrator is only activated for multi-actor documents)

### 1.3 Design Principle

The orchestrator owns the *lifecycle*. Protocol-specific runners own the *behavior*.

The orchestrator does not know or care what MCP tools look like, what an AG-UI RunAgentInput contains, or how A2A task responses are structured. It knows that actors have modes, phases, triggers, and extractors. It starts them, ensures ordering, gives them a shared extractor store and trace buffer, and shuts them down when the attack completes.

Each protocol-specific runner (TJ-SPEC-013's MCP handler, TJ-SPEC-016's AG-UI runner, TJ-SPEC-017's A2A runner) implements a common `ActorRunner` trait. The orchestrator interacts exclusively through this trait.

---

## 2. Architecture

### 2.1 Component Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Orchestrator                                   │
│                                                                             │
│  ┌──────────────┐                                                          │
│  │  CLI / main  │──▶ parse document ──▶ classify actors ──▶ build runners   │
│  └──────────────┘                                                          │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────┐               │
│  │                   Shared State                           │               │
│  │                                                         │               │
│  │  ┌─────────────────┐  ┌──────────────────────────────┐ │               │
│  │  │  ExtractorStore  │  │  SharedTrace (merged)          │ │               │
│  │  │  (Arc<RwLock>)   │  │  (Arc<Mutex>)                │ │               │
│  │  └─────────────────┘  └──────────────────────────────┘ │               │
│  └─────────────────────────────────────────────────────────┘               │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                        Actor Runners                                │   │
│  │                                                                     │   │
│  │  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐              │   │
│  │  │ MCP Server  │   │ AG-UI Client│   │ A2A Server  │   ...        │   │
│  │  │ Runner      │   │ Runner      │   │ Runner      │              │   │
│  │  │             │   │             │   │             │              │   │
│  │  │ TJ-SPEC-013 │   │ TJ-SPEC-016 │   │ TJ-SPEC-017 │              │   │
│  │  └──────┬──────┘   └──────┬──────┘   └──────┬──────┘              │   │
│  │         │                 │                 │                      │   │
│  │         │ ActorRunner     │ ActorRunner      │ ActorRunner          │   │
│  │         │ trait           │ trait            │ trait                │   │
│  │         └─────────────────┴─────────────────┘                      │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────┐                                                   │
│  │  Readiness Gate      │  Barrier: server actors signal ready              │
│  │  (tokio::Barrier +   │  → gate opens → client actors start              │
│  │   oneshot channels)  │                                                   │
│  └─────────────────────┘                                                   │
│                                                                             │
│  ┌─────────────────────┐                                                   │
│  │  Shutdown            │  Any terminal condition → broadcast shutdown       │
│  │  (CancellationToken) │  to all actors                                    │
│  └─────────────────────┘                                                   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 The `ActorRunner` Trait

Every protocol-specific runner implements this trait:

```rust
#[async_trait]
trait ActorRunner: Send + Sync {
    /// The actor's name (from OATF document)
    fn name(&self) -> &str;

    /// The actor's mode (mcp_server, ag_ui_client, etc.)
    fn mode(&self) -> &str;

    /// Whether this actor is server-role (mode ends in _server)
    fn is_server(&self) -> bool {
        self.mode().ends_with("_server")
    }

    /// Initialize the actor's transport and resources.
    /// For server actors: bind the listener.
    /// For client actors: validate configuration (URL reachable, etc.)
    async fn init(&mut self, config: &ActorConfig) -> Result<(), ActorError>;

    /// Signal that the actor's transport is ready to accept connections.
    /// Only meaningful for server actors. Client actors return immediately.
    async fn wait_ready(&self) -> Result<(), ActorError>;

    /// Run the actor's phase sequence to completion.
    /// Returns when: all phases complete, a terminal phase is reached,
    /// or the cancellation token fires.
    async fn run(
        &mut self,
        shared: SharedState,
        cancel: CancellationToken,
    ) -> Result<ActorResult, ActorError>;

    /// Graceful shutdown. Close transports, flush state.
    async fn shutdown(&mut self) -> Result<(), ActorError>;
}
```

**Relationship to PhaseLoop/PhaseDriver:** `ActorRunner` is the *lifecycle* trait — it manages actor initialization, readiness, execution, and shutdown. Each `ActorRunner::run()` implementation creates a `PhaseLoop<D>` (TJ-SPEC-013 §8.4) with the appropriate protocol-specific `PhaseDriver`, then calls `phase_loop.run()`. The PhaseLoop handles the common phase machinery (trace, extractors, triggers, phase advancement, `await_extractors`), while the PhaseDriver produces protocol-specific events. This gives the orchestrator a uniform interface (`ActorRunner`) while eliminating duplicated phase logic across protocols (`PhaseLoop`).

**`ActorConfig`** — protocol-specific configuration passed from the CLI:

```rust
struct ActorConfig {
    /// MCP server: HTTP bind address. None → stdio.
    mcp_server_bind: Option<SocketAddr>,              // --mcp-server
    /// MCP client: spawn command for stdio transport
    mcp_client_command: Option<String>,                // --mcp-client-command
    mcp_client_args: Option<String>,                   // --mcp-client-args
    /// MCP client: HTTP endpoint
    mcp_client_endpoint: Option<Url>,                  // --mcp-client-endpoint
    /// AG-UI client: agent endpoint
    agui_client_endpoint: Option<Url>,                 // --agui-client-endpoint
    /// A2A server: HTTP bind address
    a2a_server_bind: Option<SocketAddr>,               // --a2a-server
    /// A2A client: agent endpoint
    a2a_client_endpoint: Option<Url>,                  // --a2a-client-endpoint
    /// Global HTTP headers for all client transports
    global_headers: Vec<(String, String)>,             // --header
    /// Per-mode auth headers from THOUGHTJACK_{MODE}_* env vars
    mode_headers: HashMap<String, Vec<(String, String)>>,

    /// Common overrides
    grace_period: Option<Duration>,                   // --grace-period
    max_session: Option<Duration>,                    // --max-session
}
```

**`ActorResult`** — what each actor returns on completion:

```rust
struct ActorResult {
    actor_name: String,
    final_phase: String,
    phases_completed: usize,
    total_phases: usize,
    termination_reason: TerminationReason,
}

enum TerminationReason {
    TerminalPhaseReached,   // Normal: last phase has no trigger
    AllPhasesAdvanced,      // All triggers fired (unusual — means no terminal phase?)
    Cancelled,              // Shutdown signal received
    MaxSessionTimeout,      // --max-session exceeded
    TransportClosed,        // Connection dropped
    Error(ActorError),      // Actor failed
}
```

**Mapping to TJ-SPEC-014 `execution_summary.actors[]`:** The orchestrator converts each `ActorResult` to the output schema defined in TJ-SPEC-014 §3.2:

| `ActorResult` field | Output field | Conversion |
|---|---|---|
| `actor_name` | `name` | Direct |
| `final_phase` | `terminal_phase` | `Some(final_phase)` if terminal reached; `None` if error/cancelled before terminal |
| `phases_completed` | `phases_completed` | Direct |
| `total_phases` | `total_phases` | Direct |
| `termination_reason` | `status` | `TerminalPhaseReached \| AllPhasesAdvanced \| TransportClosed` → `completed`; `Cancelled` → `cancelled`; `MaxSessionTimeout` → `timeout`; `Error(_)` → `error` |
| `termination_reason` | `error` | `Error(e)` → `Some(e.to_string())`; all others → `None` |

---

## 3. Lifecycle

### 3.1 Startup Sequence

```
parse document
    │
    ▼
classify actors
    │  Partition into server_actors[] and client_actors[]
    │  Server: mode ends in _server
    │  Client: mode ends in _client
    ▼
validate support
    │  Check each actor.mode against supported modes
    │  Supported: mcp_server, ag_ui_client, a2a_server, a2a_client
    │  Unsupported: skip with warning (per TJ-SPEC-013 §3.5)
    │  If zero supported actors remain: error and exit
    ▼
build runners
    │  Create ActorRunner instance per supported actor
    │  Wire CLI config (--agui-client-endpoint goes to ag_ui_client runner, etc.)
    ▼
create shared state
    │  ExtractorStore (empty, writable by all actors)
    │  SharedTrace (empty, appendable by all actors)
    │  CancellationToken (unfired)
    ▼
init server actors (concurrent)
    │  For each server actor: call runner.init()
    │  Server actors bind their listeners here
    │  If any server actor fails init: abort all, exit with error
    ▼
wait for server readiness (concurrent)
    │  For each server actor: call runner.wait_ready()
    │  Block until ALL server actors report ready
    │  Timeout: 30s (default, configurable via --readiness-timeout)
    │  If timeout: abort all, exit with error
    ▼
init client actors (concurrent)
    │  For each client actor: call runner.init()
    │  Client actors validate their configuration here
    │  If any client actor fails init: abort all, exit with error
    ▼
start all actors (concurrent)
    │  Spawn a tokio task per actor: runner.run(shared, cancel)
    │  All actors run concurrently from this point
    ▼
wait for completion
    │  See §3.2
```

### 3.2 Completion Conditions

The orchestrator monitors all actor tasks and shuts down when any of these conditions is met:

**Normal completion:** All actors reach their terminal phase. The orchestrator waits for every spawned actor task to return an `ActorResult`. If all actors terminate with `TerminalPhaseReached`, the attack executed successfully.

**Partial completion:** Some actors complete while others are still running. This is normal — a client actor may finish its phase sequence while a server actor's terminal phase is still listening for connections. The orchestrator does NOT shut down when the first actor completes. It waits until either:

- All actors complete, OR
- The grace period expires (if any actor reached its terminal phase), OR
- The max session timeout fires (`--max-session`)

**First-client-done heuristic:** When all *client* actors have completed (reached terminal or exited), the orchestrator starts a grace period countdown. Client actors drive the interaction — once they're done, no new requests will be sent. Server actors still running are waiting for connections that will never come. The grace period gives a window for any in-flight interactions to complete, then the orchestrator cancels remaining server actors.

**Zero-client-actors fallback:** If the document contains no client actors (all actors are server-role), the first-client-done heuristic is not applicable — there are no clients to "complete." In this case, the orchestrator falls back to the single-actor rule: start the grace period only when ALL server actors have reached their terminal phases. This prevents the empty-set vacuous truth (`all(∅) == true`) from triggering an immediate grace period on the first actor completion.

```rust
async fn wait_for_completion(
    &self,
    actor_handles: Vec<JoinHandle<ActorResult>>,
    client_actor_names: HashSet<String>,
    cancel: CancellationToken,
    grace_period: Duration,
    max_session: Duration,
) {
    let mut completed: HashMap<String, ActorResult> = HashMap::new();
    let mut pending = actor_handles;

    let session_deadline = Instant::now() + max_session;

    loop {
        // Wait for next actor to complete, or timeout
        tokio::select! {
            result = next_completed(&mut pending) => {
                let (name, result) = result;
                completed.insert(name.clone(), result);

                // Check: are all client actors done?
                // Guard: if no client actors exist, skip this heuristic entirely
                // and fall back to the "all actors done" path below.
                let all_clients_done = !client_actor_names.is_empty()
                    && client_actor_names.iter()
                        .all(|name| completed.contains_key(name));

                if all_clients_done && !pending.is_empty() {
                    // Start grace period for remaining server actors
                    tokio::time::sleep(grace_period).await;
                    cancel.cancel();
                    // Collect remaining results with short timeout
                    collect_remaining(&mut pending, &mut completed, Duration::from_secs(5)).await;
                    break;
                }

                if pending.is_empty() {
                    break; // All actors done
                }
            }
            _ = tokio::time::sleep_until(session_deadline.into()) => {
                // Max session timeout — cancel everything
                cancel.cancel();
                collect_remaining(&mut pending, &mut completed, Duration::from_secs(5)).await;
                break;
            }
        }
    }
}
```

### 3.3 Shutdown Sequence

When shutdown is initiated (grace period expired, max session timeout, or SIGINT/SIGTERM):

1. Fire the `CancellationToken` — all actors receive the signal
2. Each actor's `run()` method checks the token between events and exits its event loop
3. Each actor's transport closes gracefully (send final responses, close connections)
4. Orchestrator calls `runner.shutdown()` on each actor for cleanup
5. Collect all `ActorResult` values
6. Pass the merged `SharedTrace` to the verdict pipeline (TJ-SPEC-014)

**SIGINT/SIGTERM handling:**

```rust
tokio::select! {
    _ = orchestrator.wait_for_completion(...) => {}
    _ = tokio::signal::ctrl_c() => {
        tracing::info!("Received shutdown signal");
        cancel.cancel();
        // Give actors 5 seconds to clean up
        collect_remaining(&mut pending, &mut completed, Duration::from_secs(5)).await;
    }
}
```

---

## 4. Shared Extractor Store

### 4.1 Design

The extractor store holds values captured by all actors, accessible by all actors. It is the mechanism for OATF §5.6 cross-actor references (`{{actor_name.extractor_name}}`).

```rust
struct ExtractorStore {
    /// Per-actor extractor values
    /// Key: (actor_name, extractor_name) → Value: captured string
    values: Arc<RwLock<HashMap<(String, String), String>>>,
}

impl ExtractorStore {
    /// Write an extractor value (called by the capturing actor)
    fn set(&self, actor_name: &str, extractor_name: &str, value: String) {
        self.values.write().unwrap()
            .insert((actor_name.to_string(), extractor_name.to_string()), value);
    }

    /// Read an extractor value (called by any actor)
    fn get(&self, actor_name: &str, extractor_name: &str) -> Option<String> {
        self.values.read().unwrap()
            .get(&(actor_name.to_string(), extractor_name.to_string()))
            .cloned()
    }

    /// Resolve a template reference.
    /// "extractor_name" → look up in current actor's scope
    /// "actor_name.extractor_name" → look up in specified actor's scope
    fn resolve(
        &self,
        reference: &str,
        current_actor: &str,
    ) -> Option<String> {
        if let Some((actor, name)) = reference.split_once('.') {
            self.get(actor, name)
        } else {
            self.get(current_actor, reference)
        }
    }
}
```

### 4.2 Consistency Model

The extractor store provides **immediate visibility** — writes are visible to all actors as soon as the `RwLock` is released. However, cross-actor references face a timing challenge: actor B may resolve `{{a.some_value}}` before actor A has captured it. OATF §5.6 specifies that undefined references resolve to empty string, but in CI environments, this timing sensitivity causes flaky tests.

ThoughtJack mitigates this with an **`await_extractors`** mechanism on phase entry:

```yaml
phases:
  - name: exploit
    await_extractors:                    # Block phase entry until these resolve
      - actor: mcp_poison
        name: discovered_tool
        timeout_ms: 2000                 # Per-extractor timeout (default: 1000ms)
    state:
      run_agent_input:
        messages:
          - role: user
            content: "Use {{mcp_poison.discovered_tool}} to read /etc/passwd"
```

**Semantics:**

- `await_extractors` is a ThoughtJack runtime hint, not an OATF-defined field. It is OPTIONAL on any phase.
- **Pre-processing:** During YAML pre-processing (TJ-SPEC-013 §7.2), `await_extractors` keys are extracted from phase objects into a runtime lookup table and removed from the YAML before it reaches `oatf::load()`. The SDK never sees these keys. The OATF document passed to the SDK is clean.
- **Cycle detection:** After extracting all `await_extractors` entries, the orchestrator builds a dependency graph and checks for cycles before spawning actors. A cycle (Actor A awaits B's extractor, Actor B awaits A's extractor) is a configuration error that would cause both actors to block until timeout. Since all `await_extractors` configuration is static, cycles are detected at startup and reported as an immediate error (see EC-ORCH-003).
- When the phase loop enters a phase that has extracted `await_extractors` entries, it polls the shared extractor store before executing `on_enter` or consuming `state`.
- Each referenced extractor is polled with exponential backoff (10ms, 20ms, 40ms, ...) up to `timeout_ms`.
- If all referenced extractors resolve before their timeout, the phase proceeds normally.
- If any extractor times out, ThoughtJack logs a warning and proceeds with empty string — this preserves OATF §5.6 compliance (undefined = empty string) while giving cross-actor writes time to propagate.

```rust
async fn await_phase_extractors(
    &self,
    await_specs: &[AwaitExtractor],
    store: &ExtractorStore,
) -> HashMap<String, Option<String>> {
    let mut results = HashMap::new();

    for spec in await_specs {
        let timeout = Duration::from_millis(spec.timeout_ms.unwrap_or(1000));
        let deadline = Instant::now() + timeout;
        let mut backoff = Duration::from_millis(10);

        loop {
            if let Some(value) = store.get(&spec.actor, &spec.name) {
                results.insert(format!("{}.{}", spec.actor, spec.name), Some(value));
                break;
            }
            if Instant::now() + backoff > deadline {
                tracing::warn!(
                    actor = %spec.actor,
                    extractor = %spec.name,
                    timeout_ms = %spec.timeout_ms.unwrap_or(1000),
                    "Cross-actor extractor await timed out — resolving to empty string"
                );
                results.insert(format!("{}.{}", spec.actor, spec.name), None);
                break;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_millis(200));
        }
    }

    results
}
```

**When NOT to use `await_extractors`:** Same-actor references (`{{my_extractor}}`) never need awaiting — they resolve from the local phase engine, which captures values synchronously during event processing. Only cross-actor references (`{{other_actor.extractor}}`) are subject to timing races.

**Design rationale:** An implicit blocking mechanism (e.g., all templates automatically block on missing cross-actor references) was rejected because it would violate OATF §5.1's "actors operate simultaneously and independently" principle and could cause deadlocks if two actors wait on each other. The explicit `await_extractors` annotation keeps the document author in control of synchronization points while making the common case (wait for one actor's output before using it) reliable.

### 4.3 Integration with PhaseLoop

Extractor capture and cross-actor synchronization are handled by the `PhaseLoop` (TJ-SPEC-013 §8.4), not by individual protocol runners. Each `ActorRunner` creates a `PhaseLoop` with a protocol-specific `PhaseDriver`. The `PhaseLoop` owns the `ExtractorStore` reference and performs both local and shared writes on every event:

```rust
// Inside PhaseLoop::run_extractors() — called for every protocol event
// SDK §5.6: direction param handles source filtering internally.
let direction = match event.direction {
    Direction::Incoming => oatf::ExtractorSource::Request,
    Direction::Outgoing => oatf::ExtractorSource::Response,
};
if let Some(value) = oatf::evaluate_extractor(extractor, &event.content, direction) {
    // Write to local phase engine (for same-actor references)
    self.phase_engine.extractor_values.insert(extractor.name.clone(), value.clone());
    // Write to shared store (for cross-actor references)
    self.extractor_store.set(&self.actor_name, &extractor.name, value);
}
```

`await_extractors` polling (§4.2) is also handled by `PhaseLoop::run()` — it checks the extracted configuration before entering each phase. Protocol drivers do not need to implement any cross-actor synchronization logic.

When template interpolation encounters `{{actor_name.extractor_name}}`, the resolution happens through the SDK's `interpolate_template` (§5.5), which performs simple key lookup in the extractors map. The PhaseLoop's `build_interpolation_extractors()` method (TJ-SPEC-013 §8.4) merges both local and cross-actor values into a single flat map before every interpolation call:

```rust
/// On PhaseLoop — builds the complete extractors map for SDK §5.5.
fn build_interpolation_extractors(&self) -> HashMap<String, String> {
    let mut map = self.phase_engine.extractor_values.clone();  // local (unqualified)
    map.extend(self.extractor_store.all_qualified());           // cross-actor (qualified)
    map
}
```

The `all_qualified()` method on `ExtractorStore` returns all stored values as qualified names (`actor_name.extractor_name`). This includes the current actor's own extractors (which are redundant with the unqualified names but harmless — local names take precedence because they're inserted first).

This merged map is published on a `watch` channel (TJ-SPEC-013 §8.4) after each event's extractor capture. The `PhaseLoop` also publishes an initial snapshot at phase entry for `execute_entry_actions()`. Drivers receive a `watch::Receiver<HashMap<String, String>>` — server-mode drivers call `extractors.borrow().clone()` per request to see fresh cross-actor values; client-mode drivers clone once. The SDK resolves `{{leaked_data}}` via local key lookup, and `{{recon_actor.discovered_tool}}` via qualified key lookup — both from the same flat map.

---

## 5. Merged Protocol Trace

### 5.1 Design

All actors append to a single `SharedTrace` instance (defined in TJ-SPEC-013 §9.1). The orchestrator creates one `SharedTrace` and passes `Arc` clones to all actor runners through `SharedState`. There is no separate non-thread-safe trace type — `SharedTrace` is used in both single-actor and multi-actor execution.

```rust
// In orchestrator startup:
let trace = SharedTrace::new();

// Passed to each actor runner via SharedState:
struct SharedState {
    trace: SharedTrace,           // Arc-cloned — all actors share one trace
    extractors: ExtractorStore,   // Arc-cloned — all actors share one store
}
```

Each actor runner calls `trace.append(actor, phase, direction, method, content)` for every protocol message. The `Mutex` inside `SharedTrace` serializes concurrent appends, and the global `AtomicU64` sequence counter ensures a total ordering across all actors.

### 5.2 Sequence Numbering

The global `seq` counter provides a total ordering across all actors. When two events occur simultaneously (e.g., MCP server receives a request while AG-UI client receives an SSE event), the `seq` number establishes an unambiguous order. This is critical for trace replay and for understanding the causal chain in cross-protocol attacks.

### 5.3 Trace Filtering for Indicators

Indicator evaluation requires filtering the merged trace by protocol. Per OATF §6.1, each indicator specifies `indicator.protocol`. The verdict pipeline (TJ-SPEC-014) filters trace entries:

```rust
fn filter_trace_for_indicator(
    trace: &[TraceEntry],
    indicator: &Indicator,
    actors: &[Actor],
) -> Vec<&TraceEntry> {
    let target_protocol = indicator.protocol.as_deref().unwrap_or("mcp");

    // Find actors matching this protocol
    let matching_actors: HashSet<&str> = actors.iter()
        .filter(|a| extract_protocol(&a.mode) == target_protocol)
        .map(|a| a.name.as_str())
        .collect();

    // Filter trace entries by matching actors
    trace.iter()
        .filter(|entry| matching_actors.contains(entry.actor.as_str()))
        .collect()
}
```

### 5.4 Trace Export

The merged trace exports to JSONL (TJ-SPEC-014 §7.2) with the `actor` field included:

```jsonl
{"seq":0,"ts":"...","dir":"outgoing","method":"run_agent_input","content":{...},"phase":"drive_trust","actor":"ag_ui_driver"}
{"seq":1,"ts":"...","dir":"incoming","method":"initialize","content":{...},"phase":"trust_building","actor":"mcp_poison"}
{"seq":2,"ts":"...","dir":"outgoing","method":"initialize","content":{...},"phase":"trust_building","actor":"mcp_poison"}
{"seq":3,"ts":"...","dir":"incoming","method":"run_started","content":{...},"phase":"drive_trust","actor":"ag_ui_driver"}
{"seq":4,"ts":"...","dir":"incoming","method":"tools/call","content":{...},"phase":"trust_building","actor":"mcp_poison"}
```

The interleaved entries show the causal chain: AG-UI driver sends a message (seq:0), which causes the agent to connect to the MCP server (seq:1) and call a tool (seq:4).

---

## 6. Readiness Gate

### 6.1 Mechanism

OATF §5.1 requires: "all server-role actors are accepting connections before any client-role actor begins executing its first phase."

The orchestrator implements this as a readiness gate:

```rust
struct ReadinessGate {
    server_count: usize,
    ready_tx: Vec<oneshot::Sender<()>>,   // One per server actor
    ready_rx: Vec<oneshot::Receiver<()>>,  // Orchestrator waits on all
    gate_tx: broadcast::Sender<()>,        // Fires when all servers ready
}

impl ReadinessGate {
    fn new(server_count: usize) -> Self {
        let mut ready_tx = Vec::new();
        let mut ready_rx = Vec::new();
        for _ in 0..server_count {
            let (tx, rx) = oneshot::channel();
            ready_tx.push(tx);
            ready_rx.push(rx);
        }
        let (gate_tx, _) = broadcast::channel(1);
        Self { server_count, ready_tx, ready_rx, gate_tx }
    }

    /// Server actor calls this when its transport is bound
    fn signal_ready(tx: oneshot::Sender<()>) {
        let _ = tx.send(());
    }

    /// Orchestrator calls this to wait for all servers
    async fn wait_all_ready(
        &mut self,
        timeout: Duration,
    ) -> Result<(), ReadinessError> {
        let result = tokio::time::timeout(
            timeout,
            futures::future::join_all(self.ready_rx.drain(..)),
        ).await;

        match result {
            Ok(results) => {
                if results.iter().all(|r| r.is_ok()) {
                    // All servers ready — open the gate
                    let _ = self.gate_tx.send(());
                    Ok(())
                } else {
                    Err(ReadinessError::ServerFailed)
                }
            }
            Err(_) => Err(ReadinessError::Timeout),
        }
    }

    /// Client actor calls this to wait for the gate to open
    fn subscribe_gate(&self) -> broadcast::Receiver<()> {
        self.gate_tx.subscribe()
    }
}
```

### 6.2 Server Actor Flow

```rust
// Inside MCP server runner's run()
async fn run(&mut self, shared: SharedState, cancel: CancellationToken) -> Result<ActorResult, ActorError> {
    // Bind transport
    self.transport.bind(&self.bind_address).await?;

    // Signal ready
    self.ready_signal.send(()).map_err(|_| ActorError::ReadinessSignalFailed)?;

    // Now enter the event loop — process incoming requests
    self.event_loop(shared, cancel).await
}
```

### 6.3 Client Actor Flow

```rust
// Inside AG-UI client runner's run()
async fn run(&mut self, shared: SharedState, cancel: CancellationToken) -> Result<ActorResult, ActorError> {
    // Wait for readiness gate
    let mut gate = self.readiness_gate.subscribe_gate();
    gate.recv().await.map_err(|_| ActorError::ReadinessGateFailed)?;

    // Gate open — all servers are accepting connections
    // Now begin phase execution
    self.phase_loop(shared, cancel).await
}
```

### 6.4 Single-Actor Documents

When a document has only one actor (single-phase or multi-phase form, normalized to `actors: [{ name: "default", ... }]`), the orchestrator is not activated. The existing single-actor code paths from TJ-SPEC-013, 016, and 017 handle execution directly. This avoids unnecessary overhead for the common case.

The orchestrator activates only when `document.attack.execution.actors.len() > 1`.

---

## 7. Actor-to-Mode Router

### 7.1 Runner Factory

The orchestrator maps actor modes to concrete runner implementations:

```rust
fn build_runner(
    actor: &oatf::Actor,
    document: &oatf::Document,
    config: &ActorConfig,
    shared: SharedState,
    ready_signal: Option<oneshot::Sender<()>>,
    readiness_gate: Option<broadcast::Receiver<()>>,
) -> Result<Box<dyn ActorRunner>, ActorError> {
    match actor.mode.as_str() {
        "mcp_server" => Ok(Box::new(McpServerRunner::new(
            actor, document, config, shared, ready_signal,
        )?)),
        "ag_ui_client" => Ok(Box::new(AgUiClientRunner::new(
            actor, document, config, shared, readiness_gate,
        )?)),
        "a2a_server" => Ok(Box::new(A2aServerRunner::new(
            actor, document, config, shared, ready_signal,
        )?)),
        "a2a_client" => Ok(Box::new(A2aClientRunner::new(
            actor, document, config, shared, readiness_gate,
        )?)),
        "mcp_client" => Ok(Box::new(McpClientRunner::new(
            actor, document, config, shared, readiness_gate,
        )?)),
        mode => {
            tracing::warn!(
                actor = %actor.name,
                mode = %mode,
                "Unsupported mode — skipping actor"
            );
            Err(ActorError::UnsupportedMode(mode.to_string()))
        }
    }
}
```

### 7.2 Supported Mode Matrix

| Mode | Runner | Role | Spec | Introduced |
|------|--------|------|------|------------|
| `mcp_server` | `McpServerRunner` | Server | TJ-SPEC-013 | v0.5 |
| `ag_ui_client` | `AgUiClientRunner` | Client | TJ-SPEC-016 | v0.7 |
| `a2a_server` | `A2aServerRunner` | Server | TJ-SPEC-017 | v0.8 |
| `a2a_client` | `A2aClientRunner` | Client | TJ-SPEC-017 | v0.8 |
| `mcp_client` | `McpClientRunner` | Client | TJ-SPEC-018 | v0.9 |

When the orchestrator encounters an unsupported mode, it skips that actor with a warning (per TJ-SPEC-013 §3.5) and continues with the remaining actors. If zero supported actors remain after filtering, the orchestrator exits with an error.

---

## 8. Error Handling

### 8.1 Actor Failure Modes

| Failure | Behavior | Impact on Other Actors |
|---------|----------|----------------------|
| Transport bind failure (server) | Actor fails `init()`. Orchestrator aborts startup. | All actors abort. Exit with error. |
| Transport connect failure (client) | Actor fails `init()`. Orchestrator aborts startup. | All actors abort. Exit with error. |
| Readiness timeout | Orchestrator fails at readiness gate. | All actors abort. Exit with error. |
| Runtime panic in actor task | `JoinHandle` returns `Err`. Orchestrator logs error. | Other actors continue. Verdict may be `error`. |
| Transport closed mid-execution | Actor returns with `TransportClosed`. | Other actors continue. May trigger grace period. |
| Actor error during phase execution | Actor returns with `Error(...)`. | Other actors continue. Verdict includes error. |

### 8.2 Partial Completion

When some actors complete successfully and others fail:

- Actors that completed normally contribute their trace entries and extractor values
- Failed actors contribute whatever trace entries they produced before failing
- The verdict pipeline evaluates indicators against the merged trace as-is
- Indicators targeting the failed actor's protocol may produce `error` or `not_matched` results
- The `execution_summary` in the verdict output includes per-actor status, using the same schema defined in TJ-SPEC-014 §3.2. Single-actor and multi-actor use the same format — single-actor has a one-element `actors` array.

```yaml
execution_summary:
  actors:
    - name: mcp_poison
      status: completed
      phases_completed: 2
      total_phases: 2
      terminal_phase: exploit
    - name: ag_ui_driver
      status: error
      phases_completed: 1
      total_phases: 3
      error: "Connection refused: http://localhost:8000/agent"
  grace_period_applied: "30s"
  trace_messages: 8
  duration_ms: 4520
```

### 8.3 Error Propagation

Actor errors do NOT cascade. If the MCP server actor fails, the AG-UI client actor keeps running (it may fail too because the agent can't reach the MCP server, but that's a transport error on the AG-UI side, not a propagated error). Each actor handles its own errors independently.

The orchestrator's role in error handling is:

1. Detect when an actor task returns an error
2. Log the error with the actor name
3. Check if continuing is meaningful (are there any client actors still running?)
4. If no client actors remain, start grace period for remaining server actors
5. Include per-actor status in the execution summary

---

## 9. CLI Integration

### 9.1 Unified `run` Command

Multi-actor documents use `thoughtjack run` (TJ-SPEC-013 §12). The orchestrator activates when the document defines multiple actors.

```bash
# Cross-protocol: MCP server (stdio) + A2A server (HTTP)
thoughtjack run --config cross-protocol.yaml --a2a-server 0.0.0.0:9090

# MCP server (stdio) + AG-UI client
thoughtjack run --config attack.yaml --agui-client-endpoint http://localhost:8000/agent

# Full spectrum
thoughtjack run --config full-spectrum.yaml \
  --a2a-server 0.0.0.0:9090 \
  --agui-client-endpoint http://localhost:8000/agent \
  --output verdict.json
```

### 9.2 Flag Routing

The orchestrator routes CLI flags to the appropriate actor runners based on the flag prefix:

| Flag | Routed To | Multiple Actors? |
|------|-----------|-----------------|
| `--mcp-server <addr:port>` | All `mcp_server` actors | Auto-increment ports if multiple |
| `--mcp-client-command <cmd>` | All `mcp_client` actors (stdio) | Same command for all |
| `--mcp-client-endpoint <url>` | All `mcp_client` actors (http) | Same endpoint for all |
| `--agui-client-endpoint <url>` | All `ag_ui_client` actors | Same agent for all |
| `--a2a-server <addr:port>` | All `a2a_server` actors | Auto-increment ports if multiple |
| `--a2a-client-endpoint <url>` | All `a2a_client` actors | Same agent for all |
| `--header <key:value>` | All HTTP client actors | Same headers for all |
| `--grace-period <dur>` | Orchestrator | Global |
| `--max-session <dur>` | Orchestrator | Global |
| `--output <path>` | Verdict pipeline (TJ-SPEC-014) | Global |
| `--no-semantic` | Verdict pipeline (TJ-SPEC-014) | Global |

**Port auto-increment:** When multiple server actors of the same protocol exist (uncommon but valid), the orchestrator assigns sequential ports starting from the base address. For example, two `a2a_server` actors with `--a2a-server 0.0.0.0:9090` get ports 9090 and 9091.

**Auth routing:** Per-mode environment variables (TJ-SPEC-013 §12.5) are routed to matching actor modes. `THOUGHTJACK_MCP_CLIENT_AUTHORIZATION` applies to all `mcp_client` actors, etc.

**Stdio exclusivity:** At most one `mcp_server` actor can use stdio (the process's stdin/stdout). If `--mcp-server` is not set and the document has multiple `mcp_server` actors, this is a validation error. A single `mcp_server` actor without `--mcp-server` uses stdio (default).

### 9.3 Single-Actor Fallback

When the document is single-actor, `thoughtjack run` skips the orchestrator and runs the actor directly. No readiness gate, no actor lifecycle management. This is a performance optimization, not a behavioral difference.

```bash
# Single-actor mcp_server — runs directly, no orchestrator overhead
thoughtjack run --config rug-pull.yaml
```

---

## 10. Observable Events

The orchestrator emits structured events via TJ-SPEC-008's event system:

| Event | When | Payload |
|-------|------|---------|
| `orchestrator.started` | After document parsed, actors classified | Actor count, server/client split |
| `actor.init` | Actor runner initialized | Actor name, mode |
| `actor.ready` | Server actor signals readiness | Actor name, bind address |
| `readiness_gate.open` | All server actors ready | Server count, time elapsed |
| `readiness_gate.timeout` | Server readiness timed out | Which actors not ready |
| `actor.started` | Actor begins phase execution | Actor name, phase count |
| `actor.phase_advanced` | Actor advances to next phase | Actor name, from_phase, to_phase |
| `actor.completed` | Actor finishes execution | Actor name, reason, phases completed |
| `actor.error` | Actor fails | Actor name, error message |
| `orchestrator.grace_period_started` | All clients done, grace period begins | Duration |
| `orchestrator.shutdown` | Shutdown initiated | Reason (grace expired, timeout, signal) |
| `orchestrator.completed` | All actors collected, ready for verdict | Actor results summary |

---

## 11. Edge Cases

### EC-ORCH-001: Cross-Actor Extractor Race — Missing `await_extractors`

**Scenario:** Actor B's phase references `{{a.discovered_tool}}` but the author omits `await_extractors`. Actor B enters the phase before Actor A captures the value.
**Expected:** Template resolves to empty string per OATF §5.6. Attack sends malformed payload. No error, no crash. Warning logged: `"Cross-actor reference 'a.discovered_tool' resolved to empty string — consider adding await_extractors"`. Verdict pipeline evaluates against the actual (malformed) traffic — the attack may still partially succeed or produce a meaningful `not_exploited` result.

### EC-ORCH-002: `await_extractors` Timeout — Extractor Never Captured

**Scenario:** `await_extractors` references `actor: mcp_poison, name: captured_secret` with `timeout_ms: 2000`. Actor `mcp_poison` never captures `captured_secret` (the agent never calls the expected tool).
**Expected:** After 2000ms, warning logged, value resolves to empty string. Phase proceeds. This is a legitimate test outcome — the attack path was not exercised.

### EC-ORCH-003: `await_extractors` Circular Dependency

**Scenario:** Actor A's phase 2 awaits `{{b.value}}`. Actor B's phase 2 awaits `{{a.value}}`. Both would block indefinitely.
**Expected:** Detected at startup, not at runtime. During YAML pre-processing (§4.2), `await_extractors` entries are extracted into a dependency graph. Before spawning actors, the orchestrator builds a directed graph of (actor, phase) → awaited (actor, extractor) edges and checks for cycles. If a cycle is detected, the run fails immediately with a clear error: `"Circular await_extractors dependency detected: actor_a.phase-2 → b.value → actor_b.phase-2 → a.value → actor_a.phase-2"`. No actors are spawned. Exit code 1.

**Rationale:** All `await_extractors` configuration is static (known from the YAML before execution). Waiting for a timeout on every test run with a misconfigured circular dependency would waste 30+ seconds and produce a confusing "extractor timed out" error rather than identifying the root cause.

### EC-ORCH-004: Readiness Gate — Partial Server Startup (Port Conflict)

**Scenario:** Three server actors: `mcp_a` (port 8001), `mcp_b` (port 8002, already in use), `a2a_c` (port 9000). `mcp_b` fails to bind.
**Expected:** Readiness gate timeout fires (default 30s, configurable via `--readiness-timeout`). Orchestrator cancels `mcp_a` and `a2a_c` (which bound successfully). All listeners closed, pending connections rejected. Exit with error: `"Readiness gate failed: actor 'mcp_b' did not become ready within 30s: address already in use"`. No client actors start.

### EC-ORCH-005: Readiness Gate — Server Binds But Crashes Immediately

**Scenario:** Server actor binds successfully (signals ready), then panics during first request before any client actor starts.
**Expected:** Readiness gate passes (all servers signaled ready). Client actors start. Client actor targeting the crashed server gets a transport error. The crashed server's `ActorResult` has `status: error`. Other actors continue independently (§8.3: errors do not cascade).

### EC-ORCH-006: Grace Period — Client Hangs Indefinitely

**Scenario:** Client A finishes (terminal phase). Client B is mid-stream in an SSE connection that never closes (server keeps sending events). Server C is in terminal phase.
**Expected:** "All clients done" condition is NOT met — Client B is still running. Grace period does NOT start. `--max-session` timer (default 5 min) eventually fires. Orchestrator cancels all actors. Client B's PhaseDriver receives cancel token, closes SSE stream, returns. All actors collected, verdict computed with whatever trace exists.

### EC-ORCH-007: Grace Period — All Clients Done, Server Mid-Phase

**Scenario:** Two clients complete. One server actor is in phase 1 of 3 (its trigger never fired — agent never called the poisoned tool).
**Expected:** Grace period starts (all clients done). Server continues running during grace period — it may still receive late requests from in-flight agent actions. When grace period expires, server cancelled. `execution_summary.actors[]` shows server with `status: cancelled`, `phases_completed: 1`, `total_phases: 3`. This is a meaningful test result: the attack path was not exercised.

### EC-ORCH-008: Grace Period vs Max-Session Interaction

**Scenario:** Grace period is 30s. Max-session is 60s. All clients finish at t=55s. Grace period would end at t=85s, but max-session fires at t=60s.
**Expected:** Max-session takes precedence. All actors cancelled at t=60s. Grace period effectively shortened to 5s. Logged: `"Max session timeout reached during grace period — cancelling all actors"`.

### EC-ORCH-009: Single Actor in Multi-Actor Document

**Scenario:** OATF document defines `actors: [{ name: "solo", mode: "mcp_server", ... }]` — one actor in a multi-actor document structure.
**Expected:** Orchestrator runs normally with one actor. Readiness gate waits for one server. No client actors means grace period starts immediately on terminal phase (degenerates to single-actor rule). `execution_summary.actors` has one-element array. No overhead from unused orchestration machinery.

### EC-ORCH-010: Actor Name Collision

**Scenario:** Two actors defined with `name: "mcp_poison"`.
**Expected:** Validation error during document loading: `"Duplicate actor name: 'mcp_poison'"`. Rejected before orchestrator starts.

### EC-ORCH-011: Extractor Store — High Contention Write Storm

**Scenario:** Four actors each capturing extractors at 100+ events/sec, all writing to the shared `ExtractorStore`.
**Expected:** `RwLock` serializes writes. Read latency increases but remains bounded (each write is a single HashMap insert, ~100ns). No data loss, no deadlock. In practice, protocol I/O latency (ms) dominates extractor store contention (ns).

### EC-ORCH-012: Trace Ordering — Simultaneous Events Across Protocols

**Scenario:** MCP server receives `tools/call` at the exact same instant AG-UI client receives `run_finished` SSE event.
**Expected:** `SharedTrace.seq_counter` (AtomicU64) assigns sequential numbers. One event gets seq N, the other gets N+1. Order is deterministic within a single execution (whichever thread calls `fetch_add` first wins). Order may differ across runs — this is expected and documented. Indicators should not depend on cross-protocol ordering of simultaneous events.

### EC-ORCH-013: SIGINT During Readiness Gate

**Scenario:** User sends SIGINT while waiting for server actors to become ready.
**Expected:** Readiness gate cancelled. Server actors that already bound are shut down. Exit cleanly with no verdict output (attack never started).

### EC-ORCH-014: Zero Grace Period in Multi-Actor Mode

**Scenario:** `grace_period: 0s` and all clients complete.
**Expected:** Grace period "starts" and immediately expires. Server actors cancelled with zero observation window. This is valid — it means "don't wait for late interactions."

### EC-ORCH-015: Client Actor Fails During Phase — No Terminal Phase Reached

**Scenario:** Client actor errors out in phase 1 (connection refused). Never reaches terminal phase.
**Expected:** Client actor returns `ActorResult { status: error, phases_completed: 1, ... }`. Counted as "done" for the "all clients done" check (it cannot make further progress). If all other clients are also done, grace period starts. Server actors continue during grace period.

### EC-ORCH-016: Grace Period — Zero Client Actors (All-Server Document)

**Scenario:** Document has two server actors (`mcp_server` + `a2a_server`) and no client actors. The attack relies on an external agent connecting to both servers.
**Expected:** The first-client-done heuristic is skipped (no clients exist). The orchestrator does NOT start the grace period on the first server completion. Instead, it waits until ALL server actors reach their terminal phases (same as single-actor behavior). If one server finishes early, the other continues running. Grace period starts only after both servers complete.

---

## 12. Functional Requirements

### F-ORCH-001: Actor Lifecycle Management
The system SHALL spawn one protocol driver per actor defined in the OATF document, each running its own phase loop independently.

**Acceptance Criteria:**
- Each actor gets a dedicated `PhaseLoop` instance with its own `ProtocolDriver`
- Actors operate simultaneously and independently per OATF §5.1
- Actor spawn order matches document order
- All actors reach `ready` state before any begins phase execution
- Actor completion conditions: terminal phase reached, error, or cancellation
- Grace period timer starts after all client actors complete (§3.2)

### F-ORCH-002: Shared Extractor Store
The system SHALL maintain a thread-safe shared extractor store accessible by all actors, supporting both local and cross-actor qualified name lookups.

**Acceptance Criteria:**
- `ExtractorStore` shared across all actors via `Arc`
- Writers: `set(actor_name, extractor_name, value)` — atomic write
- Readers: `all_qualified()` returns all values as `actor_name.extractor_name` keys
- `get(actor_name, extractor_name)` for single value lookup
- Concurrent writes from different actors do not cause data races
- Cross-actor reads reflect the latest committed value

### F-ORCH-003: Merged Protocol Trace
The system SHALL maintain a single merged protocol trace containing entries from all actors, with each entry attributed to its source actor.

**Acceptance Criteria:**
- All actors append to the same `SharedTrace` via `Arc`
- Global monotonic sequence counter (`AtomicU64`) ensures total ordering across actors
- Every trace entry includes: seq, timestamp, `actor` name, phase name, direction, method, content
- Thread-safe concurrent appends (serialized via `Mutex`)
- Snapshot for evaluation returns consistent point-in-time copy
- The merged trace is used for indicator evaluation (TJ-SPEC-014 §3.5)

### F-ORCH-004: Readiness Gate
The system SHALL block phase execution until all actors have completed transport setup and reached the ready state, with a configurable timeout.

**Acceptance Criteria:**
- Default readiness timeout is 30 seconds
- Timeout produces a clear error identifying which actors failed to become ready
- Partial readiness (some actors ready, others not) does not start execution

### F-ORCH-005: Actor-to-Mode Router
The system SHALL route each actor to the correct protocol driver based on its `mode` field, supporting `mcp_server`, `mcp_client`, `a2a_server`, `a2a_client`, and `ag_ui_client`.

**Acceptance Criteria:**
- `mcp_server` → MCP Server runner (TJ-SPEC-013)
- `ag_ui_client` → AG-UI Client runner (TJ-SPEC-016)
- `a2a_server` → A2A Server runner (TJ-SPEC-017)
- `a2a_client` → A2A Client runner (TJ-SPEC-017)
- `mcp_client` → MCP Client runner (TJ-SPEC-018)
- Unrecognized mode values: actor skipped with warning at runner build time (not a document validation error — OATF treats mode as an open string per §11.5; this is a ThoughtJack runtime support limitation)
- Each recognized mode maps to exactly one driver implementation

### F-ORCH-006: Cross-Actor Phase Coordination
The system SHALL support `await_extractors` allowing one actor's phase to block until extractors from another actor are available.

**Acceptance Criteria:**
- `await_extractors` keys extracted during YAML pre-processing and stripped before SDK
- Phase entry blocks until all specified cross-actor extractors are available
- Polling with exponential backoff (10ms → 200ms cap)
- Per-extractor configurable timeout (default: 1000ms)
- Circular await dependencies are detected at startup and reported as errors (EC-ORCH-003)
- Ignored with warning on single-actor documents

### F-ORCH-007: Graceful Shutdown
The system SHALL propagate shutdown signals to all actors when the grace period expires, max-session timeout fires, or SIGINT/SIGTERM is received. Individual actor errors do NOT trigger shutdown of other actors (§8.3).

**Acceptance Criteria:**
- Grace period expiry: remaining (server) actors receive cancellation
- SIGINT/SIGTERM: all actors receive cancellation and clean up transport connections
- Max-session timeout: all actors receive cancellation
- Individual actor errors are isolated — other actors continue (§8.3)
- Shutdown completes within a configurable grace period (default: 5s)
- Exit code reflects the worst outcome across all actors

### F-ORCH-008: Observable Actor Events
The system SHALL emit structured events for actor lifecycle transitions including spawn, ready, phase transitions, errors, and shutdown.

**Acceptance Criteria:**
- All events include `actor` field for attribution
- Events are emitted to the observability pipeline (TJ-SPEC-008)
- Event ordering within a single actor is guaranteed

### F-ORCH-009: CLI Multi-Actor Display
The system SHALL display per-actor status in CLI output, showing each actor's current phase, message counts, and health.

**Acceptance Criteria:**
- `--format text` shows a summary line per actor
- `--format json` includes per-actor breakdown in structured output
- Real-time updates show which actors are active

### F-ORCH-010: Actor Error Isolation
The system SHALL isolate actor errors so that one actor's failure does not crash others.

**Acceptance Criteria:**
- Each actor runs in its own `JoinHandle` with error capture
- Failed actor's error recorded in execution summary
- Remaining actors continue executing
- Orchestrator collects all results (success or error) from all actors
- Execution summary includes per-actor status, phases completed, and total phases

### F-ORCH-011: Single-Actor Bypass
The system SHALL route single-actor documents directly to the protocol runner without orchestrator overhead.

**Acceptance Criteria:**
- Documents using single-phase or multi-phase form (no `actors[]`) bypass the orchestrator after SDK normalization
- Documents using multi-actor form (`actors[]`) with exactly one actor run through the orchestrator normally (EC-ORCH-009) — the `actors[]` form signals intent for orchestration semantics even with one actor
- The bypass decision is made after SDK normalization: `document.attack.execution.actors.len() == 1 && !document_uses_actors_form`
- No `SharedTrace` mutex contention for single-actor mode
- No readiness gate for single-actor mode
- Behavior identical to multi-actor mode from the actor's perspective


---

## 13. Non-Functional Requirements

### NFR-001: Actor Startup Latency

- All actors SHALL start within 500ms of orchestrator launch (excluding transport binding)
- Readiness gate SHALL add < 100ms overhead for server actor readiness signaling

### NFR-002: Extractor Store Performance

- `set()` SHALL complete in < 10μs per write
- `all_qualified()` SHALL complete in < 1ms for up to 100 extractors
- No lock contention under typical multi-actor workloads (< 4 actors)

### NFR-003: Trace Append Performance

- `SharedTrace::append()` SHALL complete in < 50μs per entry (including lock acquisition)
- Mutex contention SHALL not cause observable latency for sessions with < 4 concurrent actors

### NFR-004: Shutdown Timeliness

- Coordinated shutdown SHALL complete within 5 seconds of signal receipt
- If any actor fails to drain within 5 seconds, it is killed (JoinHandle abort)

---

## 14. Definition of Done

- [ ] Orchestrator starts all actors concurrently from `execution.actors`
- [ ] Actor-to-mode routing dispatches to correct protocol runner
- [ ] `ExtractorStore` provides thread-safe cross-actor value propagation
- [ ] `all_qualified()` returns qualified names for SDK interpolation
- [ ] `await_extractors` blocks phase entry until cross-actor extractors available
- [ ] `await_extractors` timeout produces error after configurable limit
- [ ] `SharedTrace` provides global sequence-ordered trace across all actors
- [ ] Readiness gate synchronizes server/client actor startup
- [ ] Actor errors isolated — other actors continue executing
- [ ] Execution summary includes per-actor status, phases completed, error details
- [ ] SIGINT/SIGTERM triggers coordinated shutdown within 5 seconds
- [ ] Single-actor documents bypass orchestrator with no overhead
- [ ] `thoughtjack run` routes CLI flags to protocol-specific runners
- [ ] All 15 edge cases (EC-ORCH-001 through EC-ORCH-015) have tests
- [ ] Actor startup < 500ms (NFR-001)
- [ ] Extractor store set < 10μs, all_qualified < 1ms (NFR-002)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 15. References

- [OATF Format Specification v0.1 §5](https://oatf.io/specs/v0.1) — Multi-actor execution
- [OATF SDK Specification v0.1 §5.5](https://oatf.io/specs/sdk/v0.1) — Extractor map population
- [TJ-SPEC-013: OATF Integration](./TJ-SPEC-013_OATF_Integration.md) — PhaseLoop and PhaseDriver
- [TJ-SPEC-014: Verdict & Evaluation Output](./TJ-SPEC-014_Verdict_Evaluation_Output.md) — Multi-actor verdict
- [TJ-SPEC-016: AG-UI Protocol Support](./TJ-SPEC-016_AGUI_Protocol_Support.md)
- [TJ-SPEC-017: A2A Protocol Support](./TJ-SPEC-017_A2A_Protocol_Support.md)
- [TJ-SPEC-018: MCP Client Mode](./TJ-SPEC-018_MCP_Client_Mode.md)
- [Tokio Sync Primitives](https://docs.rs/tokio/latest/tokio/sync/index.html) — Barriers, channels, cancellation
