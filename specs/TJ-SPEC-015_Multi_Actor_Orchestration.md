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

Each protocol mode is handled by a branch in the `run_actor()` function, which creates the appropriate transport and `PhaseDriver`. The orchestrator calls `run_actor()` for each actor.

---

## 2. Architecture

### 2.1 Component Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Orchestrator                                   │
│                                                                             │
│  ┌──────────────┐                                                          │
│  │  CLI / main  │──▶ load document ──▶ classify actors ──▶ spawn tasks     │
│  └──────────────┘                                                          │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────┐               │
│  │                   Shared Resources                       │               │
│  │                                                         │               │
│  │  ┌─────────────────┐  ┌──────────────────────────────┐ │               │
│  │  │  ExtractorStore  │  │  SharedTrace (merged)          │ │               │
│  │  │  (Arc<DashMap>)  │  │  (Arc<Mutex>)                │ │               │
│  │  └─────────────────┘  └──────────────────────────────┘ │               │
│  └─────────────────────────────────────────────────────────┘               │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Actor Tasks (JoinSet)                             │   │
│  │                                                                     │   │
│  │  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐              │   │
│  │  │ MCP Server  │   │ AG-UI Client│   │ A2A Server  │   ...        │   │
│  │  │ run_actor() │   │ run_actor() │   │ run_actor() │              │   │
│  │  │             │   │             │   │             │              │   │
│  │  │ TJ-SPEC-013 │   │ TJ-SPEC-016 │   │ TJ-SPEC-017 │              │   │
│  │  └─────────────┘   └─────────────┘   └─────────────┘              │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────┐                                                   │
│  │  Readiness Gate      │  Server actors signal ready via oneshot            │
│  │  (oneshot + broad-   │  → gate opens → client actors start              │
│  │   cast channels)     │                                                   │
│  └─────────────────────┘                                                   │
│                                                                             │
│  ┌─────────────────────┐                                                   │
│  │  Shutdown            │  Any terminal condition → broadcast shutdown       │
│  │  (CancellationToken) │  to all actors                                    │
│  └─────────────────────┘                                                   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 The `run_actor()` Function

Rather than a trait-based approach, the actor runner is implemented as a single free function that pattern-matches on the actor's mode:

```rust
pub async fn run_actor(
    actor_index: usize,
    document: oatf::Document,
    config: &ActorConfig,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    ready_tx: Option<oneshot::Sender<()>>,        // Server: signal readiness
    gate_rx: Option<broadcast::Receiver<()>>,      // Client: wait for gate
    events: &EventEmitter,
) -> Result<ActorResult, EngineError> {
    match actor.mode.as_str() {
        "mcp_server" => run_mcp_server_actor(...).await,
        "ag_ui_client" => run_agui_client_actor(...).await,
        "a2a_server" => run_a2a_server_actor(...).await,
        "a2a_client" => run_a2a_client_actor(...).await,
        "mcp_client" => run_mcp_client_actor(...).await,
        other => Err(EngineError::Driver(...)),
    }
}
```

Each per-mode function handles initialization, transport binding/connection, readiness signaling, and phase loop creation internally. Server-mode actors signal readiness via `ready_tx` after binding. Client-mode actors wait for the readiness gate via `gate_rx` before starting protocol I/O.

**Design rationale:** A trait-based `ActorRunner` abstraction was considered but rejected. The `PhaseDriver` trait (TJ-SPEC-013 §8.4) already provides protocol abstraction. Adding a second trait layer for lifecycle management introduces dynamic dispatch and object-safety constraints without meaningful benefit. The free function with match is simpler, more idiomatic Rust, and easier to extend.

**`ActorConfig`** — runtime configuration for actor execution, derived from CLI flags:

```rust
struct ActorConfig {
    /// MCP server: HTTP bind address string. None → stdio.
    mcp_server_bind: Option<String>,                  // --mcp-server
    /// MCP client: spawn command for stdio transport
    mcp_client_command: Option<String>,                // --mcp-client-command
    mcp_client_args: Option<String>,                   // --mcp-client-args
    /// MCP client: HTTP endpoint
    mcp_client_endpoint: Option<String>,               // --mcp-client-endpoint
    /// AG-UI client: agent endpoint
    agui_client_endpoint: Option<String>,              // --agui-client-endpoint
    /// A2A server: HTTP bind address string
    a2a_server_bind: Option<String>,                   // --a2a-server
    /// A2A client: agent endpoint
    a2a_client_endpoint: Option<String>,               // --a2a-client-endpoint
    /// Extra HTTP headers for client-mode transports
    headers: Vec<(String, String)>,                    // --header
    /// Bypass synthesize output validation
    raw_synthesize: bool,                              // --raw-synthesize

    /// Common overrides
    grace_period: Option<Duration>,                   // --grace-period
    max_session: Duration,                            // --max-session (always set)
    readiness_timeout: Duration,                      // --readiness-timeout
}
```

**Design rationale:** All endpoint and bind address fields use `String` rather than parsed types (`SocketAddr`, `Url`). Parsing is deferred to the point of use, keeping `ActorConfig` a simple data bag that maps 1:1 to CLI flags. The `max_session` is non-optional with a CLI default (e.g., `"5m"`), ensuring every orchestration has a session limit. Per-mode auth headers (`THOUGHTJACK_{MODE}_*` env vars) are not yet implemented and are omitted from the config.

**`ActorResult`** — what each actor returns on completion:

```rust
struct ActorResult {
    actor_name: String,
    termination: TerminationReason,
    phases_completed: usize,
    total_phases: usize,
    final_phase: Option<String>,
}

enum TerminationReason {
    TerminalPhaseReached,   // Normal: last phase has no trigger
    Cancelled,              // Shutdown signal received
    MaxSessionExpired,      // --max-session exceeded
    TransportClosed,        // Connection dropped (e.g., stdio EOF)
}
```

`ActorResult` is returned by `PhaseLoop::run()`. The `final_phase` is `Some(name)` if the phase has a name, `None` otherwise. `total_phases` is always set from the actor's phase count. Actor errors are NOT represented in `TerminationReason` — they propagate as `Result::Err(EngineError)` from `run_actor()` and are wrapped in `ActorOutcome::Error` by the orchestrator.

**Mapping to TJ-SPEC-014 `execution_summary.actors[]`:** The orchestrator converts each `ActorOutcome` to the output schema defined in TJ-SPEC-014 §3.2:

| `ActorOutcome` | Output field | Conversion |
|---|---|---|
| `Success(r).actor_name` | `name` | Direct |
| `Success(r).final_phase` | `terminal_phase` | Direct (already `Option<String>`) |
| `Success(r).phases_completed` | `phases_completed` | Direct |
| `Success(r).total_phases` | `total_phases` | Direct |
| `Success(r).termination` | `status` | `TerminalPhaseReached \| TransportClosed` → `"completed"`; `Cancelled` → `"cancelled"`; `MaxSessionExpired` → `"timeout"` |
| `Error { error, .. }` | `status` | → `"error"` |
| `Error { error, .. }` | `error` | → `Some(error)` |

---

## 3. Lifecycle

### 3.1 Startup Sequence

```
load document
    │  Load and validate OATF document
    │  Extract await_extractors into runtime lookup table
    │  Detect await_extractors cycles (EC-ORCH-003)
    ▼
classify actors
    │  Partition into servers and clients
    │  Server: mode contains "server"
    │  Client: everything else
    ▼
create shared resources
    │  ExtractorStore (empty Arc<DashMap>)
    │  SharedTrace (empty, appendable by all actors)
    │  CancellationToken (unfired)
    │  ReadinessGate (if any servers: one oneshot per server)
    ▼
spawn all actor tasks (concurrent via JoinSet)
    │  For each actor: spawn tokio task calling run_actor()
    │  Server actors: receive ready_tx (oneshot::Sender)
    │  Client actors: receive gate_rx (broadcast::Receiver)
    │  Each task creates transport + driver + PhaseLoop internally
    │  All tasks start executing immediately
    ▼
wait for server readiness
    │  ReadinessGate::wait_all_ready(readiness_timeout)
    │  Timeout: 30s (default, configurable via --readiness-timeout)
    │  If timeout: cancel all, abort tasks, exit with error
    │  On success: broadcast fires, client actors unblock
    ▼
wait for completion
    │  See §3.2
```

**Design rationale:** All actor tasks are spawned upfront into a `JoinSet` before waiting for server readiness. Server actors handle their own transport binding and readiness signaling internally. Client actors block on the gate receiver before starting protocol I/O. This eliminates the need for separate init/start phases and simplifies the orchestrator to a spawn-then-wait pattern.

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
    mut join_set: JoinSet<ActorTaskResult>,
    config: &ActorConfig,
    cancel: &CancellationToken,
    total_clients: usize,
) {
    let mut outcomes: Vec<ActorOutcome> = Vec::new();
    let mut clients_done = 0;
    let max_session_deadline = Instant::now() + config.max_session;

    loop {
        if join_set.is_empty() { break; }

        tokio::select! {
            Some(join_result) = join_set.join_next() => {
                let (outcome, is_server) = unpack_join_result(join_result);

                if is_server == Some(false) {
                    clients_done += 1;
                }
                outcomes.push(outcome);

                // Check shutdown conditions
                if total_clients > 0 && clients_done >= total_clients {
                    // All clients done → grace period → cancel servers
                    apply_grace_and_cancel(config, cancel).await;
                } else if total_clients == 0 && join_set.is_empty() {
                    // Zero-client mode: all servers done → grace → cancel
                    apply_grace_and_cancel(config, cancel).await;
                }
            }
            _ = tokio::time::sleep_until(max_session_deadline) => {
                // Max session timeout — cancel everything
                cancel.cancel();
            }
            _ = cancel.cancelled() => {
                break;
            }
        }
    }

    // Drain any remaining tasks after cancel
    join_set.abort_all();
    while let Some(join_result) = join_set.join_next().await {
        outcomes.push(unpack_join_result(join_result).0);
    }
}
```

### 3.3 Shutdown Sequence

When shutdown is initiated (grace period expired, max session timeout, or external cancellation):

1. Fire the `CancellationToken` — all actors receive the signal via `cancel.cancelled()`
2. Each actor's `PhaseLoop::run()` exits its `tokio::select!` loop
3. Each actor's transport closes gracefully (send final responses, close connections)
4. Orchestrator calls `join_set.abort_all()` to terminate any remaining tasks
5. Drain remaining `JoinSet` results into `ActorOutcome` collection
6. Pass the merged `SharedTrace` to the verdict pipeline (TJ-SPEC-014)

**SIGINT/SIGTERM handling:** Signal handling is done at the CLI level (`main.rs`), not inside the orchestrator. The CLI wraps the orchestrator call in a `tokio::select!` with `tokio::signal::ctrl_c()`. On signal, it cancels the shared `CancellationToken`, which propagates to all actors via child tokens.

---

## 4. Shared Extractor Store

### 4.1 Design

The extractor store holds values captured by all actors, accessible by all actors. It is the mechanism for OATF §5.6 cross-actor references (`{{actor_name.extractor_name}}`).

```rust
struct ExtractorStore {
    /// Per-actor extractor values
    /// Key: (actor_name, extractor_name) → Value: captured string
    store: Arc<DashMap<(String, String), String>>,
}

impl ExtractorStore {
    /// Write an extractor value (called by the capturing actor)
    fn set(&self, actor_name: &str, extractor_name: &str, value: String) {
        self.store.insert(
            (actor_name.to_string(), extractor_name.to_string()),
            value,
        );
    }

    /// Read an extractor value (called by any actor)
    fn get(&self, actor_name: &str, extractor_name: &str) -> Option<String> {
        self.store
            .get(&(actor_name.to_string(), extractor_name.to_string()))
            .map(|v| v.value().clone())
    }

    /// Returns all extractors as qualified `actor_name.extractor_name` keys.
    fn all_qualified(&self) -> HashMap<String, String> {
        self.store.iter()
            .map(|entry| {
                let (actor, name) = entry.key();
                (format!("{actor}.{name}"), entry.value().clone())
            })
            .collect()
    }
}
```

**Design rationale:** `DashMap` provides lock-free concurrent reads and fine-grained write locking per shard, eliminating the `RwLock` contention described in the earlier design. Template reference resolution (`"extractor_name"` vs `"actor.extractor_name"`) is handled by `build_interpolation_extractors()` in the `PhaseLoop`, not by the store itself. The store is a simple key-value map; the PhaseLoop merges local and cross-actor values into a flat map for SDK interpolation.

### 4.2 Consistency Model

The extractor store provides **immediate visibility** — writes are visible to all actors as soon as the `DashMap` shard lock is released. However, cross-actor references face a timing challenge: actor B may resolve `{{a.some_value}}` before actor A has captured it. OATF §5.6 specifies that undefined references resolve to empty string, but in CI environments, this timing sensitivity causes flaky tests.

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

Extractor capture and cross-actor synchronization are handled by the `PhaseLoop` (TJ-SPEC-013 §8.4), not by individual protocol drivers. Each `run_actor()` branch creates a `PhaseLoop` with a protocol-specific `PhaseDriver`. The `PhaseLoop` owns the `ExtractorStore` reference and performs both local and shared writes on every event:

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

All actors append to a single `SharedTrace` instance (defined in TJ-SPEC-013 §9.1). The orchestrator creates one `SharedTrace` and one `ExtractorStore`, then passes clones to each actor task as individual parameters. There is no `SharedState` wrapper struct — both types implement `Clone` via inner `Arc` and are passed directly to `run_actor()`.

```rust
// In orchestrator startup:
let trace = SharedTrace::new();
let extractor_store = ExtractorStore::new();

// Cloned into each actor task:
let tr = trace.clone();        // Arc-cloned — all actors share one trace
let es = extractor_store.clone(); // Arc-cloned — all actors share one store
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
    ready_rxs: Vec<(String, oneshot::Receiver<()>)>,  // (actor_name, receiver) per server
    gate_tx: broadcast::Sender<()>,                    // Fires when all servers ready
}

impl ReadinessGate {
    /// Creates a gate for the given server actor names.
    /// Returns (gate, senders) — each server gets a named oneshot::Sender.
    fn new(server_actors: &[String]) -> (Self, Vec<(String, oneshot::Sender<()>)>) {
        let (gate_tx, _) = broadcast::channel(1);
        let mut receivers = Vec::new();
        let mut senders = Vec::new();
        for name in server_actors {
            let (tx, rx) = oneshot::channel();
            receivers.push((name.clone(), rx));
            senders.push((name.clone(), tx));
        }
        let gate = Self { ready_rxs: receivers, gate_tx };
        (gate, senders)
    }

    /// Client actor calls this to subscribe for the gate broadcast.
    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.gate_tx.subscribe()
    }

    /// Orchestrator calls this to wait for all servers (consumes self).
    async fn wait_all_ready(self, timeout: Duration) -> Result<(), GateError> {
        let result = tokio::time::timeout(timeout, wait_all_receivers(self.ready_rxs)).await;
        match result {
            Ok(Ok(())) => {
                let _ = self.gate_tx.send(());  // Open the gate
                Ok(())
            }
            Ok(Err(gate_err)) => Err(gate_err),
            Err(_) => Err(GateError::Timeout { not_ready: ... }),
        }
    }
}

enum GateError {
    Timeout { not_ready: Vec<String> },         // Which actors didn't signal
    ServerFailed { actor: String },             // Which actor dropped its sender
}
```

The `new()` constructor takes `&[String]` (server actor names) rather than a count, enabling the gate to report *which* actors failed to become ready. `wait_all_ready()` consumes `self` — the gate is single-use.

### 6.2 Server Actor Flow

```rust
// Inside run_actor() → mcp_server branch
async fn run_mcp_server_actor(
    ..., ready_tx: Option<oneshot::Sender<()>>, cancel: CancellationToken,
) -> Result<ActorResult, EngineError> {
    // Create transport (stdio or HTTP based on config)
    let transport = create_transport(config)?;

    // Signal ready (after transport is bound, if HTTP)
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    // Create driver and PhaseLoop, then run
    let driver = McpServerDriver::new(transport);
    let phase_loop = PhaseLoop::new(driver, ...);
    phase_loop.run().await
}
```

### 6.3 Client Actor Flow

```rust
// Inside run_actor() → ag_ui_client branch
async fn run_agui_client_actor(
    ..., gate_rx: Option<broadcast::Receiver<()>>, cancel: CancellationToken,
) -> Result<ActorResult, EngineError> {
    // Wait for readiness gate (if multi-actor)
    if let Some(mut rx) = gate_rx {
        let _ = rx.recv().await;
    }

    // Gate open — all servers are accepting connections
    // Create driver and PhaseLoop, then run
    let driver = AgUiClientDriver::new(endpoint, headers);
    let phase_loop = PhaseLoop::new(driver, ...);
    phase_loop.run().await
}
```

### 6.4 Single-Actor Documents

When a document has one or fewer actors after SDK normalization (single-phase or multi-phase form, or a single-element `actors[]`), the orchestrator is bypassed. The CLI runs the single actor directly via `run_actor()` without `JoinSet`, `ReadinessGate`, or grace period coordination. This avoids unnecessary overhead for the common case.

The orchestrator activates only when `document.attack.execution.actors.len() > 1`, regardless of whether the document was authored in single-phase, multi-phase, or multi-actor form.

---

## 7. Actor-to-Mode Router

### 7.1 Mode Dispatch

The `run_actor()` function dispatches to a mode-specific handler via pattern match. There is no separate factory — `run_actor()` creates the transport, driver, and `PhaseLoop` in a single function:

```rust
pub async fn run_actor(
    actor_index: usize,
    document: oatf::Document,
    config: &ActorConfig,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    ready_tx: Option<oneshot::Sender<()>>,
    gate_rx: Option<broadcast::Receiver<()>>,
    events: &EventEmitter,
) -> Result<ActorResult, EngineError> {
    let actor = &document.attack.execution.actors[actor_index];
    match actor.mode.as_str() {
        "mcp_server" => { /* create transport + McpServerDriver + PhaseLoop */ }
        "ag_ui_client" => { /* create AgUiClientDriver + PhaseLoop */ }
        "a2a_server" => { /* create transport + A2aServerDriver + PhaseLoop */ }
        "a2a_client" => { /* create A2aClientDriver + PhaseLoop */ }
        "mcp_client" => { /* create McpClientDriver + PhaseLoop */ }
        other => Err(EngineError::Driver(
            format!("unsupported actor mode: {other}")
        )),
    }
}
```

Unsupported modes return `Err(EngineError::Driver(...))`, which the orchestrator wraps in `ActorOutcome::Error`. This is a hard error for that actor, not a skip-with-warning — the orchestrator's `unpack_join_result()` handles the error and other actors continue.

### 7.2 Supported Mode Matrix

| Mode | Driver | Role | Spec | Introduced |
|------|--------|------|------|------------|
| `mcp_server` | `McpServerDriver` | Server | TJ-SPEC-013 | v0.5 |
| `ag_ui_client` | `AgUiClientDriver` | Client | TJ-SPEC-016 | v0.7 |
| `a2a_server` | `A2aServerDriver` | Server | TJ-SPEC-017 | v0.8 |
| `a2a_client` | `A2aClientDriver` | Client | TJ-SPEC-017 | v0.8 |
| `mcp_client` | `McpClientDriver` | Client | TJ-SPEC-018 | v0.9 |

When `run_actor()` encounters an unsupported mode, it returns `Err(EngineError::Driver(...))`. The orchestrator records this as `ActorOutcome::Error` and other actors continue executing independently.

---

## 8. Error Handling

### 8.1 Actor Failure Modes

| Failure | Behavior | Impact on Other Actors |
|---------|----------|----------------------|
| Transport bind failure (server) | `run_actor()` returns `Err(EngineError)`. Recorded as `ActorOutcome::Error`. | Other actors continue. Readiness gate may timeout if server never signals. |
| Transport connect failure (client) | `run_actor()` returns `Err(EngineError)`. Recorded as `ActorOutcome::Error`. | Other actors continue. Counted as "client done" for grace period. |
| Readiness timeout | Orchestrator cancels all actors, aborts `JoinSet`. | All actors abort. Exit with error. |
| Runtime panic in actor task | `JoinSet` returns `JoinError`. Recorded as `ActorOutcome::Panic`. | Other actors continue. Verdict may be `error`. |
| Transport closed mid-execution | Actor returns `Ok(ActorResult { termination: TransportClosed, .. })`. | Other actors continue. May trigger grace period. |
| Actor error during phase execution | `run_actor()` returns `Err(EngineError)`. Recorded as `ActorOutcome::Error`. | Other actors continue. Verdict includes error. |

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
thoughtjack run --scenario cross-protocol.yaml --a2a-server 0.0.0.0:9090

# MCP server (stdio) + AG-UI client
thoughtjack run --scenario attack.yaml --agui-client-endpoint http://localhost:8000/agent

# Full spectrum
thoughtjack run --scenario full-spectrum.yaml \
  --a2a-server 0.0.0.0:9090 \
  --agui-client-endpoint http://localhost:8000/agent \
  --output verdict.json
```

### 9.2 Flag Routing

All CLI flags are collected into a single `ActorConfig` struct and shared by all actors. Each actor's `run_actor()` branch reads only the fields relevant to its mode:

| Flag | Used By | Multiple Actors |
|------|---------|-----------------|
| `--mcp-server <addr:port>` | `mcp_server` actors | Same bind address for all |
| `--mcp-client-command <cmd>` | `mcp_client` actors (stdio) | Same command for all |
| `--mcp-client-endpoint <url>` | `mcp_client` actors (http) | Same endpoint for all |
| `--agui-client-endpoint <url>` | `ag_ui_client` actors | Same agent for all |
| `--a2a-server <addr:port>` | `a2a_server` actors | Same bind address for all |
| `--a2a-client-endpoint <url>` | `a2a_client` actors | Same agent for all |
| `--header <key:value>` | All HTTP client actors | Same headers for all |
| `--raw-synthesize` | All actors | Global |
| `--grace-period <dur>` | Orchestrator | Global |
| `--max-session <dur>` | Orchestrator | Global |
| `--readiness-timeout <dur>` | Orchestrator | Global |
| `--output <path>` | Verdict pipeline (TJ-SPEC-014) | Global |
| `--no-semantic` | Verdict pipeline (TJ-SPEC-014) | Global |

**Stdio exclusivity:** At most one `mcp_server` actor can use stdio (the process's stdin/stdout). If `--mcp-server` is not set and the document has multiple `mcp_server` actors, this is a validation error. A single `mcp_server` actor without `--mcp-server` uses stdio (default).

**Port sharing:** Multiple server actors of the same protocol mode sharing the same bind address is not currently supported. Each server mode should use a distinct address, or only one server of each type should be defined. Future versions may add port auto-increment.

### 9.3 Single-Actor Fallback

When the document has one or fewer actors after SDK normalization, `thoughtjack run` skips the orchestrator and calls `run_actor()` directly. No readiness gate, no `JoinSet`, no grace period coordination. This is a performance optimization, not a behavioral difference.

```bash
# Single-actor mcp_server — runs directly, no orchestrator overhead
thoughtjack run --scenario rug-pull.yaml
```

---

## 10. Observable Events

The orchestrator emits structured events via TJ-SPEC-008's event system:

| Event | When | Payload |
|-------|------|---------|
| `orchestrator.started` | After document parsed, actors classified | Actor count, server/client split |
| `actor.spawned` | Actor task spawned into JoinSet | Actor name, mode |
| `actor.ready` | Server actor signals readiness | Actor name, bind address |
| `readiness_gate.open` | All server actors ready | Server count, time elapsed |
| `readiness_gate.timeout` | Server readiness timed out | Which actors not ready |
| `actor.started` | Actor begins phase execution | Actor name, phase count |
| `actor.phase_advanced` | Actor advances to next phase | Actor name, from_phase, to_phase |
| `actor.completed` | Actor finishes execution | Actor name, reason, phases completed |
| `actor.error` | Actor fails | Actor name, error message |
| `orchestrator.grace_period_started` | All clients done, grace period begins | Duration (seconds) |
| `orchestrator.grace_period_expired` | Grace period ended, cancelling remaining actors | Messages captured during grace |
| `orchestrator.shutdown` | Shutdown initiated | Reason (max_session_expired, etc.) |
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
**Expected:** Detected at startup during document loading, not at runtime. After extracting `await_extractors` entries, `detect_await_cycles()` builds an actor dependency graph and performs DFS cycle detection. If a cycle is found, loading fails with `LoaderError::CyclicDependency`. No actors are spawned. Exit code 10 (RUNTIME_ERROR).

**Rationale:** All `await_extractors` configuration is static (known from the YAML before execution). Waiting for a timeout on every test run with a misconfigured circular dependency would waste 30+ seconds and produce a confusing "extractor timed out" error rather than identifying the root cause.

### EC-ORCH-004: Readiness Gate — Partial Server Startup (Port Conflict)

**Scenario:** Three server actors: `mcp_a` (port 8001), `mcp_b` (port 8002, already in use), `a2a_c` (port 9000). `mcp_b` fails to bind. Its `run_actor()` returns `Err` and the oneshot sender is dropped.
**Expected:** `ReadinessGate::wait_all_ready()` detects the dropped sender and returns `GateError::ServerFailed { actor: "mcp_b" }` (or times out if the error races with the timeout). Orchestrator cancels all actors, aborts the `JoinSet`. Exit with error. No client actors start. `--readiness-timeout` (default 30s) controls the maximum wait.

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

**Scenario:** OATF document defines `actors: [{ name: "solo", mode: "mcp_server", ... }]` — one actor in the multi-actor document structure.
**Expected:** Orchestrator is bypassed (`actors.len() <= 1`). The single actor runs directly via `run_actor()` without `JoinSet` or `ReadinessGate`. `execution_summary.actors` has a one-element array. No overhead from unused orchestration machinery.

### EC-ORCH-010: Actor Name Collision

**Scenario:** Two actors defined with `name: "mcp_poison"`.
**Expected:** Validation error during document loading: `"Duplicate actor name: 'mcp_poison'"`. Rejected before orchestrator starts.

### EC-ORCH-011: Extractor Store — High Contention Write Storm

**Scenario:** Four actors each capturing extractors at 100+ events/sec, all writing to the shared `ExtractorStore`.
**Expected:** `DashMap` shards writes across multiple locks. Read and write latency remains bounded (each write is a single shard-locked insert, ~100ns). No data loss, no deadlock. In practice, protocol I/O latency (ms) dominates extractor store contention (ns).

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
- `mcp_server` → `McpServerDriver` (TJ-SPEC-013)
- `ag_ui_client` → `AgUiClientDriver` (TJ-SPEC-016)
- `a2a_server` → `A2aServerDriver` (TJ-SPEC-017)
- `a2a_client` → `A2aClientDriver` (TJ-SPEC-017)
- `mcp_client` → `McpClientDriver` (TJ-SPEC-018)
- Unrecognized mode values: `run_actor()` returns `Err(EngineError::Driver(...))`, recorded as `ActorOutcome::Error`
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
The system SHALL route single-actor documents directly to `run_actor()` without orchestrator overhead.

**Acceptance Criteria:**
- Documents with one or fewer actors after SDK normalization bypass the orchestrator (`actors.len() <= 1`)
- The bypass applies regardless of document form (single-phase, multi-phase, or single-element `actors[]`)
- No `JoinSet`, no `ReadinessGate`, no grace period coordination for single-actor mode
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
- [ ] Actor-to-mode routing dispatches to correct protocol driver
- [ ] `ExtractorStore` provides thread-safe cross-actor value propagation
- [ ] `all_qualified()` returns qualified names for SDK interpolation
- [ ] `await_extractors` blocks phase entry until cross-actor extractors available
- [ ] `await_extractors` timeout produces warning after configurable limit
- [ ] `await_extractors` circular dependencies detected at load time (EC-ORCH-003)
- [ ] `SharedTrace` provides global sequence-ordered trace across all actors
- [ ] Readiness gate synchronizes server/client actor startup
- [ ] Actor errors isolated — other actors continue executing
- [ ] Execution summary includes per-actor status, phases completed, error details
- [ ] SIGINT/SIGTERM triggers coordinated shutdown within 5 seconds
- [ ] Single-actor documents bypass orchestrator with no overhead
- [ ] `thoughtjack run` routes CLI flags via `ActorConfig` to `run_actor()`
- [ ] All 16 edge cases (EC-ORCH-001 through EC-ORCH-016) have tests
- [ ] Actor startup < 500ms (NFR-001)
- [ ] Extractor store set < 10μs, all_qualified < 1ms (NFR-002)
- [ ] `cargo clippy --tests -- -D warnings` passes
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
- [Tokio Sync Primitives](https://docs.rs/tokio/latest/tokio/sync/index.html) — Oneshot, broadcast, watch channels, cancellation
- [DashMap](https://docs.rs/dashmap/latest/dashmap/) — Concurrent HashMap for ExtractorStore
