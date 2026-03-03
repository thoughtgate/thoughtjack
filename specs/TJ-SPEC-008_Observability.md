# TJ-SPEC-008: Observability

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-008` |
| **Title** | Observability |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **Medium** |
| **Version** | v2.0.0 |
| **Tags** | `#logging` `#metrics` `#events` `#debugging` `#reporting` |
| **Supersedes** | TJ-SPEC-008 v1.0.0 |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's observability system — the logging, metrics, and events that enable security researchers to understand attack execution, validate results, and diagnose issues.

### 1.1 Motivation

ThoughtJack is a testing tool. Observability is critical for:

| Use Case | Requirement |
|----------|-------------|
| **Test validation** | Did the attack execute as expected? |
| **Result capture** | What did the agent do? What payloads were delivered? |
| **Debugging** | Why didn't the phase transition fire? Why did extractors not capture? |
| **CI integration** | Machine-readable output for automated pipelines |
| **Multi-actor coordination** | Which actor advanced when? What cross-actor extractors propagated? |

### 1.2 Observability Pillars

| Pillar | Purpose | Implementation |
|--------|---------|----------------|
| **Logging** | Human-readable event stream | Structured logs via `tracing` |
| **Metrics** | Quantitative measurements | Counters, histograms, gauges |
| **Events** | Discrete state changes | Phase transitions, attack triggers, verdict |

### 1.3 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Structured by default** | Machine-parseable logs for tooling integration |
| **Verbosity levels** | From quiet (errors only) to trace (everything) |
| **Zero-cost when disabled** | No overhead when not observing |
| **Actor-scoped context** | Multi-actor logging with actor name in every span |
| **Privacy-aware** | Option to redact sensitive payloads |

### 1.4 Scope Boundaries

**In scope:**
- Logging format and levels
- Structured log fields
- Metrics definitions
- Event types and payloads (engine, orchestration, verdict, protocol)
- Output destinations (stderr, file, JSON)
- Debug mode features

**Out of scope:**
- Metrics aggregation/storage (external systems)
- Log shipping (external systems)
- Verdict output format (TJ-SPEC-014)
- CLI flag handling (TJ-SPEC-007)

---

## 2. Logging

### F-001: Logging Framework

The system SHALL use structured logging with configurable verbosity.

**Acceptance Criteria:**
- Uses `tracing` crate for structured logging
- Supports log levels: trace, debug, info, warn, error
- Log messages include timestamp, level, target, message
- Structured fields attached to log events
- Configurable via `-v` flags and `THOUGHTJACK_LOG_LEVEL`

**Log Levels:**

| Level | Use For | Example |
|-------|---------|---------|
| `error` | Failures requiring attention | Document validation failed, transport error |
| `warn` | Unexpected but recoverable | `--raw-synthesize` active, extractor pattern didn't match |
| `info` | Normal operation milestones | Phase transition, actor started, verdict computed |
| `debug` | Detailed operation flow | Request received, response dispatched, extractor captured |
| `trace` | Everything, including data | Full message payloads, interpolation results |

**Implementation:**
```rust
use tracing::{info, debug, trace, warn, error};
use tracing_subscriber::{fmt, EnvFilter};

pub const fn verbosity_to_directive(verbosity: u8) -> &'static str {
    match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    }
}

pub fn init_logging(format: LogFormat, verbosity: u8, color: ColorChoice) {
    let default_directive = verbosity_to_directive(verbosity);
    let filter = EnvFilter::try_from_env("THOUGHTJACK_LOG_LEVEL")
        .unwrap_or_else(|_| EnvFilter::new(default_directive));

    let use_ansi = match color {
        ColorChoice::Auto => std::io::stderr().is_terminal(),
        ColorChoice::Always => true,
        ColorChoice::Never => false,
    };

    match format {
        LogFormat::Human => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(use_ansi)
                .with_target(verbosity >= 2)
                .with_writer(std::io::stderr)
                .try_init()
                .ok();
        }
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_target(verbosity >= 2)
                .with_writer(std::io::stderr)
                .try_init()
                .ok();
        }
    }
}
```

### F-002: Log Format (Human-Readable)

Default format for terminal output. Colorized when stderr is TTY.

**Format:**
```
2025-02-25T10:15:30.123Z INFO  thoughtjack::engine > [mcp_server] Phase transition from="trust_building" to="trigger_swap"
2025-02-25T10:15:31.456Z DEBUG thoughtjack::engine > [mcp_server] Extractor captured name="secret_key" value="abc..."
2025-02-25T10:15:32.789Z INFO  thoughtjack::verdict > Verdict: exploited (2/3 indicators matched)
```

### F-003: Log Format (JSON)

One JSON object per line (JSONL/NDJSON) for machine consumption.

**Format:**
```json
{"timestamp":"2025-02-25T10:15:30.123Z","level":"INFO","target":"thoughtjack::engine","fields":{"actor":"mcp_server","from":"trust_building","to":"trigger_swap"},"message":"Phase transition"}
```

### F-004: Contextual Logging

The system SHALL attach context to log spans using tracing spans.

**Acceptance Criteria:**
- Actor name attached to all engine logs
- Phase name attached during phase execution
- Protocol type included in driver logs
- Request ID for request-scoped operations (where applicable)

**Implementation:**
```rust
// Actor-scoped span — all logs within an actor carry its name
let _span = tracing::info_span!("actor", name = %actor_name, protocol = %protocol).entered();

// Phase-scoped span — nested within actor span
let _phase_span = tracing::info_span!("phase", name = %phase_name, index = phase_index).entered();

// Request-scoped span — nested within phase span (server-mode drivers)
let _req_span = tracing::debug_span!("request", method = %method).entered();
```

---

## 3. Events

Events are discrete, typed state changes emitted during execution. They serve two purposes: observability (understanding what happened) and test assertions (verifying expected behavior).

### F-005: Event Emitter

The system SHALL emit structured events with timestamps and sequence numbers.

**Acceptance Criteria:**
- Events are structured, typed objects
- Events include timestamp and sequence number
- Events can be streamed to file (JSONL)
- Events enable test assertions

**Implementation:**
```rust
pub struct EventEmitter {
    file: Option<BufWriter<File>>,
    sequence: AtomicU64,
}

impl EventEmitter {
    pub fn emit(&self, event: ThoughtJackEvent) {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let envelope = EventEnvelope { sequence: seq, event };

        if let Some(file) = &self.file {
            let line = serde_json::to_string(&envelope).unwrap();
            writeln!(file, "{}", line).ok();
            file.flush().ok();
        }

        tracing::info!(event = ?envelope.event, "Event emitted");
    }
}
```

### F-006: Engine Events

Events emitted by the PhaseLoop and PhaseEngine (TJ-SPEC-013).

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `phase.entered` | INFO | actor, phase_name, phase_index | Phase activated |
| `phase.advanced` | INFO | actor, from_phase, to_phase, trigger | Trigger fired, phase advanced |
| `phase.terminal` | INFO | actor, phase_name | Terminal phase reached |
| `extractor.captured` | DEBUG | actor, extractor_name, value_preview | Extractor matched and captured value |
| `extractor.published` | DEBUG | actor, extractor_name | Value published to ExtractorStore |
| `trigger.evaluated` | TRACE | actor, event_type, count, threshold, fired | Trigger condition checked |
| `synthesize.generated` | DEBUG | actor, protocol, prompt_preview | LLM generated response content |
| `synthesize.validation_bypassed` | WARN | actor | `--raw-synthesize` active, output sent without validation |
| `entry_action.executed` | DEBUG | actor, action_type | on_enter action completed |

### F-007: Orchestration Events

Events emitted by the Orchestrator and ActorRunners (TJ-SPEC-015).

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `orchestrator.started` | INFO | actor_count, server_count, client_count | After document parsed, actors classified |
| `actor.init` | INFO | actor_name, mode | Actor runner initialized |
| `actor.ready` | INFO | actor_name, bind_address | Server actor signals readiness |
| `readiness_gate.open` | INFO | server_count, elapsed_ms | All server actors ready |
| `readiness_gate.timeout` | WARN | not_ready_actors | Server readiness timed out |
| `actor.started` | INFO | actor_name, phase_count | Actor begins phase execution |
| `actor.completed` | INFO | actor_name, reason, phases_completed | Actor finishes execution |
| `actor.error` | ERROR | actor_name, error_message | Actor fails |
| `await_extractors.waiting` | DEBUG | actor, phase_index, awaiting | Waiting for cross-actor extractors |
| `await_extractors.resolved` | DEBUG | actor, phase_index, values | All awaited extractors available |
| `await_extractors.timeout` | WARN | actor, phase_index, missing | Timeout waiting for extractors |
| `orchestrator.shutdown` | INFO | reason | Shutdown initiated (grace expired, timeout, signal) |
| `orchestrator.completed` | INFO | actor_results_summary | All actors collected, ready for verdict |

### F-008: Verdict Events

Events emitted by the verdict pipeline (TJ-SPEC-014).

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `grace_period.started` | INFO | duration | Grace period timer begins |
| `grace_period.expired` | INFO | messages_captured | Grace period timer fires |
| `grace_period.early_termination` | INFO | reason | Grace period ends early |
| `indicator.evaluated` | INFO | indicator_id, method, result, duration_ms | Indicator evaluation complete |
| `indicator.skipped` | WARN | indicator_id, reason | Indicator skipped (no LLM, etc.) |
| `semantic.llm_call` | DEBUG | model, indicator_id, latency_ms | LLM invoked for semantic eval |
| `semantic.calibration_warning` | WARN | indicator_id, example, score, threshold | Example calibration failed |
| `verdict.computed` | INFO | result, matched_count, total_count | Final verdict computed |

### F-009: Protocol Events

Events emitted by protocol drivers (TJ-SPEC-013, 016, 017, 018). These are the protocol-level messages exchanged with the agent.

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `protocol.request_received` | DEBUG | actor, method, protocol | Incoming request from agent |
| `protocol.request_payload` | TRACE | actor, full_params | Full request content |
| `protocol.response_sent` | DEBUG | actor, method, protocol, duration_ms | Response dispatched to agent |
| `protocol.response_payload` | TRACE | actor, full_result | Full response content |
| `protocol.request_sent` | DEBUG | actor, method, protocol | Outgoing request to agent (client mode) |
| `protocol.response_received` | DEBUG | actor, method, protocol, duration_ms | Response received from agent (client mode) |
| `protocol.notification` | DEBUG | actor, method, direction | Notification sent or received |
| `protocol.transport_error` | WARN | actor, error | Transport-level failure |
| `protocol.interleave` | DEBUG | actor, server_method | Server-initiated request during dispatch (elicitation/sampling) |

### F-010: Legacy Server Events

Events from v0.2 modules that remain operational (transport, behavior, generators). Retained for backward compatibility.

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `server.started` | INFO | name, transport | Server started (v0.2 mode) |
| `server.stopped` | INFO | reason, uptime | Server stopped (v0.2 mode) |
| `transport.listening` | INFO | transport_type, address | Transport bound |
| `transport.connected` | INFO | connection_id | Client connected |
| `transport.disconnected` | INFO | connection_id, reason | Client disconnected |
| `behavior.delivery_start` | DEBUG | behavior_type, message_size | Delivery behavior activates |
| `behavior.delivery_complete` | DEBUG | behavior_type, duration_ms | Delivery completes |
| `behavior.side_effect_start` | DEBUG | effect_type | Side effect execution |
| `behavior.side_effect_complete` | DEBUG | effect_type, messages_sent | Side effect completes |

---

## 4. Metrics

### F-011: Metrics Collection

The system SHALL collect quantitative metrics. Metrics are exposed via Prometheus format.

**Label cardinality MUST be bounded to prevent memory exhaustion** (see §4.1).

#### Core Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `tj_scenarios_total` | Counter | result | Scenarios executed, labeled by verdict result |
| `tj_scenario_duration_seconds` | Histogram | — | Total scenario execution time |
| `tj_actors_total` | Counter | mode, status | Actors executed, labeled by mode and completion status |

#### Engine Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `tj_phase_transitions_total` | Counter | actor, from, to | Phase transitions |
| `tj_extractors_captured_total` | Counter | actor | Extractor values captured |
| `tj_synthesize_calls_total` | Counter | actor, protocol | LLM generation calls |
| `tj_synthesize_duration_seconds` | Histogram | protocol | LLM generation latency |

#### Protocol Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `tj_protocol_messages_total` | Counter | actor, direction, method | Protocol messages (in/out) |
| `tj_protocol_message_duration_seconds` | Histogram | actor, direction, method | Message handling latency |
| `tj_transport_errors_total` | Counter | actor, protocol | Transport-level failures |

#### Verdict Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `tj_verdicts_total` | Counter | result | Verdicts produced by result type |
| `tj_indicators_evaluated_total` | Counter | method, result | Indicator evaluations by method (cel/pattern/semantic) and result |
| `tj_indicator_evaluation_duration_seconds` | Histogram | method | Indicator evaluation latency |
| `tj_semantic_llm_calls_total` | Counter | — | LLM calls for semantic evaluation |
| `tj_semantic_llm_latency_seconds` | Histogram | — | Semantic LLM call latency |
| `tj_grace_period_messages_captured` | Histogram | — | Messages captured during grace period |

#### Legacy Metrics (v0.2)

These metrics are retained for backward compatibility when running in v0.2 server mode.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `thoughtjack_requests_total` | Counter | method | Requests received |
| `thoughtjack_responses_total` | Counter | method, status | Responses sent |
| `thoughtjack_request_duration_ms` | Histogram | method | Request latency |
| `thoughtjack_delivery_bytes_total` | Counter | — | Bytes delivered |
| `thoughtjack_side_effects_total` | Counter | effect_type | Side effects run |
| `thoughtjack_current_phase` | Gauge | phase_name | Current phase |
| `thoughtjack_connections_active` | Gauge | — | Open connections |

### 4.1 Label Cardinality Protection

To prevent memory exhaustion from attacker-controlled label values, all metrics with `method` labels MUST normalize unknown methods:

```rust
const KNOWN_MCP_METHODS: &[&str] = &[
    "initialize", "ping",
    "tools/list", "tools/call",
    "resources/list", "resources/read", "resources/subscribe",
    "prompts/list", "prompts/get",
    "elicitation/create", "sampling/createMessage",
    "logging/setLevel", "completion/complete",
];

const KNOWN_A2A_METHODS: &[&str] = &[
    "message/send", "message/stream",
    "tasks/get", "tasks/cancel",
    "agent_card/get",
];

const KNOWN_AGUI_EVENTS: &[&str] = &[
    "RUN_STARTED", "RUN_FINISHED", "RUN_ERROR",
    "TEXT_MESSAGE_START", "TEXT_MESSAGE_CONTENT", "TEXT_MESSAGE_END",
    "TOOL_CALL_START", "TOOL_CALL_END",
    "AGENT_ERROR",
];

fn sanitize_method_label(method: &str) -> &str {
    if KNOWN_MCP_METHODS.contains(&method)
        || KNOWN_A2A_METHODS.contains(&method)
        || KNOWN_AGUI_EVENTS.contains(&method)
    {
        method
    } else {
        "__unknown__"
    }
}
```

Phase name labels are sanitized: truncated to 64 characters, non-`[a-zA-Z0-9_-]` replaced with `_`.

### F-012: Metrics Export

The system SHALL support Prometheus exposition format via `metrics-exporter-prometheus`.

**Acceptance Criteria:**
- Metrics endpoint available when enabled
- Disabled by default (zero overhead)
- Configurable via `--metrics-port`

```bash
# Enable metrics endpoint
thoughtjack run --config attack.yaml --metrics-port 9090
# Metrics at http://localhost:9090/metrics
```

---

## 5. Debug Mode

### F-013: Verbose Logging Levels

The system SHALL support multiple verbosity levels via `-v` flags.

| Flag | Level | Content |
|------|-------|---------|
| (none) | warn | Warnings and errors only |
| `-v` | info | Lifecycle events, phase transitions, verdict |
| `-vv` | debug | Full request/response payloads, extractor captures |
| `-vvv` | trace | Per-operation timing, internal state, interpolation results |

### F-014: Event File Output

The system SHALL support writing events to a JSONL file.

**Acceptance Criteria:**
- Events written as JSONL (one JSON object per line)
- Atomic writes (no partial events)
- File rotation not required (test tool, bounded execution)

**Warning:** When running multiple instances in parallel, each instance SHOULD write to a unique event file.

---

## 6. Edge Cases

### EC-OBS-001: High-Volume Logging

**Trigger:** Trace-level logging with high request rate.
**Expected:** Async buffered writes. No request blocking. Backpressure drops log entries rather than blocking.

### EC-OBS-002: Log File Full

**Trigger:** Disk full during event file write.
**Expected:** Log warning, continue execution without event file. Do not crash.

### EC-OBS-003: Invalid Log Level

**Trigger:** `THOUGHTJACK_LOG_LEVEL=invalid`.
**Expected:** Fall back to verbosity-derived level. Log warning about invalid env var.

### EC-OBS-004: Metrics Endpoint Conflict

**Trigger:** `--metrics-port` specifies a port already in use.
**Expected:** Exit with error. Do not start execution.

### EC-OBS-005: Event File Not Writable

**Trigger:** Event file path not writable (permissions, directory doesn't exist).
**Expected:** Exit with error at startup. Do not start execution.

### EC-OBS-006: Unicode in Log Messages

**Trigger:** Protocol messages contain multi-byte UTF-8 (emoji, CJK, etc.).
**Expected:** Log correctly in both human and JSON format. No truncation or corruption.

### EC-OBS-007: Concurrent Log Writes

**Trigger:** Multiple actors logging simultaneously in multi-actor execution.
**Expected:** No interleaving within a single log line. Actor name in context disambiguates.

### EC-OBS-008: Metrics Overflow

**Trigger:** Counter exceeds u64 max.
**Expected:** Counter wraps. No crash. (Practically unreachable.)

### EC-OBS-009: Multi-Actor Event Ordering

**Trigger:** Events from different actors arrive near-simultaneously.
**Expected:** Sequence numbers are globally ordered (AtomicU64). Timestamps reflect wall clock, not causal order.

### EC-OBS-010: Quiet Mode With Verdict

**Trigger:** `--quiet` flag with `--output verdict.json`.
**Expected:** No stderr output. JSON verdict still written to file. Exit code still reflects verdict.

### EC-OBS-011: Unknown Method Metric Bucketing

**Trigger:** Agent sends request with non-standard MCP method (e.g., `custom/foo`).
**Expected:** Method label normalized to `__unknown__`. Single metric bucket, not per-unknown-method.

### EC-OBS-012: High Cardinality Label Attack

**Trigger:** Agent sends thousands of requests with unique method names.
**Expected:** All bucketed as `__unknown__`. Metric memory bounded.

---

## 7. Non-Functional Requirements

### NFR-001: Logging Overhead

Logging at info level SHALL add less than 1% overhead to request processing latency.

### NFR-002: Memory Usage

Event buffer and metrics storage SHALL not exceed 10 MB for scenarios with fewer than 10,000 protocol messages.

### NFR-003: Non-Blocking

Async logging and event emission SHALL NOT block protocol message handling.

### NFR-004: Atomic Writes

Individual log lines and event file entries SHALL be written atomically (no partial lines in output).

---

## 8. Implementation

### 8.1 Module Structure

```
src/observability/
├── mod.rs          # Re-exports, init functions
├── logging.rs      # init_logging, format configuration
├── metrics.rs      # init_metrics, describe_metrics, sanitize_method_label
└── events.rs       # EventEmitter, ThoughtJackEvent, EventEnvelope
```

### 8.2 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Blocking on log writes | Stalls request handling | Use async/buffered writes |
| String formatting in hot path | Allocation overhead | Use tracing's lazy formatting |
| Global mutable state for metrics | Thread safety issues | Use atomic counters |
| Logging passwords/secrets | Security risk | Redact sensitive fields |
| Timestamps in local time | Confusing across timezones | Always use UTC |
| Unbounded log buffers | Memory exhaustion | Bounded buffers with backpressure |
| Too many unique label values | Cardinality explosion | Sanitize method labels (§4.1) |

### 8.3 Testing Strategy

**Unit Tests:**
- Log level filtering
- JSON format correctness
- Metrics increment accuracy
- Event serialization
- Method label sanitization

**Integration Tests:**
- End-to-end with log capture
- Metrics endpoint scraping
- Event file verification
- Multi-actor event ordering

---

## 9. Definition of Done

- [ ] Tracing-based logging framework initialized
- [ ] Human-readable and JSON log formats implemented
- [ ] Log levels (trace through error) work correctly
- [ ] Contextual logging with actor/phase spans
- [ ] Engine events emitted (F-006)
- [ ] Orchestration events emitted (F-007)
- [ ] Verdict events emitted (F-008)
- [ ] Protocol events emitted (F-009)
- [ ] Legacy server events retained (F-010)
- [ ] Metrics collection with cardinality protection (F-011, §4.1)
- [ ] Prometheus metrics export (F-012)
- [ ] Verbosity levels via `-v` flags (F-013)
- [ ] Event file output as JSONL (F-014)
- [ ] All 12 edge cases (EC-OBS-001 through EC-OBS-012) have tests
- [ ] Logging overhead < 1% at info level (NFR-001)
- [ ] Memory usage within limits (NFR-002)
- [ ] Async logging doesn't block requests (NFR-003)
- [ ] Atomic log writes verified (NFR-004)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 10. References

- [TJ-SPEC-013: OATF Integration](./TJ-SPEC-013_OATF_Integration.md) — Engine events
- [TJ-SPEC-014: Verdict Evaluation Output](./TJ-SPEC-014_Verdict_Evaluation_Output.md) — Verdict events, §8
- [TJ-SPEC-015: Multi-Actor Orchestration](./TJ-SPEC-015_Multi_Actor_Orchestration.md) — Orchestration events
- [TJ-SPEC-007: CLI Interface](./TJ-SPEC-007_CLI_Interface.md) — Verbosity flags
- [Tracing Crate](https://docs.rs/tracing/latest/tracing/)
- [Metrics Crate](https://docs.rs/metrics/latest/metrics/)
- [Prometheus Exposition Format](https://prometheus.io/docs/instrumenting/exposition_formats/)

---

## Appendix A: Full Event Type Enum

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ThoughtJackEvent {
    // --- Engine (TJ-SPEC-013) ---
    PhaseEntered { actor: String, phase_name: String, phase_index: usize },
    PhaseAdvanced { actor: String, from: String, to: String, trigger: String },
    PhaseTerminal { actor: String, phase_name: String },
    ExtractorCaptured { actor: String, name: String, value_preview: String },
    SynthesizeGenerated { actor: String, protocol: String },
    SynthesizeValidationBypassed { actor: String },
    EntryActionExecuted { actor: String, action_type: String },

    // --- Orchestration (TJ-SPEC-015) ---
    OrchestratorStarted { actor_count: usize, server_count: usize, client_count: usize },
    ActorInit { actor_name: String, mode: String },
    ActorReady { actor_name: String, bind_address: String },
    ReadinessGateOpen { server_count: usize, elapsed_ms: u64 },
    ReadinessGateTimeout { not_ready: Vec<String> },
    ActorStarted { actor_name: String, phase_count: usize },
    ActorCompleted { actor_name: String, reason: String, phases_completed: usize },
    ActorError { actor_name: String, error: String },
    AwaitExtractorsWaiting { actor: String, phase_index: usize, awaiting: Vec<String> },
    AwaitExtractorsResolved { actor: String, phase_index: usize },
    AwaitExtractorsTimeout { actor: String, phase_index: usize, missing: Vec<String> },
    OrchestratorShutdown { reason: String },
    OrchestratorCompleted { summary: String },

    // --- Verdict (TJ-SPEC-014) ---
    GracePeriodStarted { duration_seconds: u64 },
    GracePeriodExpired { messages_captured: usize },
    GracePeriodEarlyTermination { reason: String },
    IndicatorEvaluated { indicator_id: String, method: String, result: String, duration_ms: u64 },
    IndicatorSkipped { indicator_id: String, reason: String },
    SemanticLlmCall { model: String, indicator_id: String, latency_ms: u64 },
    VerdictComputed { result: String, matched: usize, total: usize },

    // --- Protocol (TJ-SPEC-013, 016, 017, 018) ---
    ProtocolMessageReceived { actor: String, method: String, protocol: String },
    ProtocolMessageSent { actor: String, method: String, protocol: String, duration_ms: u64 },
    ProtocolNotification { actor: String, method: String, direction: String },
    ProtocolTransportError { actor: String, error: String },
    ProtocolInterleave { actor: String, server_method: String },

    // --- Legacy (v0.2) ---
    ServerStarted { server_name: String, transport: String },
    ServerStopped { reason: String, uptime_seconds: u64 },
    TransportConnected { connection_id: String },
    TransportDisconnected { connection_id: String, reason: String },

    // --- General ---
    Error { error_type: String, message: String, context: String },
}
```
