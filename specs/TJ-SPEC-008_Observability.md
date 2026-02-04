# TJ-SPEC-008: Observability

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-008` |
| **Title** | Observability |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **Medium** |
| **Version** | v1.0.0 |
| **Tags** | `#logging` `#metrics` `#tracing` `#events` `#debugging` `#reporting` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's observability system — the logging, metrics, events, and debugging capabilities that enable security researchers to understand attack execution, validate test results, and diagnose issues.

### 1.1 Motivation

ThoughtJack is a testing tool. Observability is critical for:

| Use Case | Requirement |
|----------|-------------|
| **Test validation** | Did the attack execute as expected? |
| **Result capture** | What did the client do? What payloads were delivered? |
| **Debugging** | Why didn't the phase transition fire? |
| **Reporting** | Generate test reports for security assessments |
| **Integration** | Feed data into security tooling pipelines |

### 1.2 Observability Pillars

ThoughtJack implements three observability pillars:

| Pillar | Purpose | Implementation |
|--------|---------|----------------|
| **Logging** | Human-readable event stream | Structured logs via `tracing` |
| **Metrics** | Quantitative measurements | Counters, histograms, gauges |
| **Events** | Discrete state changes | Phase transitions, attack triggers |

### 1.3 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Structured by default** | Machine-parseable logs for tooling integration |
| **Verbosity levels** | From quiet (errors only) to trace (everything) |
| **Zero-cost when disabled** | No overhead when not observing |
| **Test-friendly output** | Easy to assert on in integration tests |
| **Privacy-aware** | Option to redact sensitive payloads |

### 1.4 Scope Boundaries

**In scope:**
- Logging format and levels
- Structured log fields
- Metrics definitions
- Event types and payloads
- Output destinations (stderr, file, JSON)
- Test result reporting
- Debug mode features

**Out of scope:**
- Metrics aggregation/storage (external systems)
- Log shipping (external systems)
- Alerting (external systems)
- CLI flag handling (TJ-SPEC-007)

---

## 2. Functional Requirements

### F-001: Logging Framework

The system SHALL use structured logging with configurable verbosity.

**Acceptance Criteria:**
- Uses `tracing` crate for structured logging
- Supports log levels: trace, debug, info, warn, error
- Log messages include timestamp, level, target, message
- Structured fields attached to log events
- Configurable via `--log-level` and `THOUGHTJACK_LOG_LEVEL`

**Log Levels:**

| Level | Use For | Example |
|-------|---------|---------|
| `error` | Failures requiring attention | Config validation failed |
| `warn` | Unexpected but recoverable | Unknown field in config |
| `info` | Normal operation milestones | Phase transition, server started |
| `debug` | Detailed operation flow | Request received, response sent |
| `trace` | Everything, including data | Full message payloads |

**Implementation:**
```rust
use tracing::{info, debug, trace, warn, error, instrument, Span};
use tracing_subscriber::{fmt, EnvFilter};

pub fn init_logging(level: LogLevel, format: LogFormat) {
    let filter = EnvFilter::new(level.to_filter_string());
    
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);
    
    match format {
        LogFormat::Human => {
            subscriber
                .with_ansi(atty::is(atty::Stream::Stderr))
                .init();
        }
        LogFormat::Json => {
            subscriber
                .json()
                .init();
        }
    }
}
```

### F-002: Log Format (Human-Readable)

The system SHALL support human-readable log output.

**Acceptance Criteria:**
- Default format for terminal output
- Colorized when stderr is TTY
- Timestamps in local timezone
- Concise, scannable format

**Format:**
```
2025-02-04T10:15:30.123Z INFO  thoughtjack::server > Server started
2025-02-04T10:15:30.125Z INFO  thoughtjack::server > name="rug-pull-test" transport="stdio"
2025-02-04T10:15:31.456Z DEBUG thoughtjack::phase  > Event recorded event="tools/call" count=1
2025-02-04T10:15:31.789Z INFO  thoughtjack::phase  > Phase transition from="trust_building" to="trigger_swap"
```

**With colors (TTY):**
```
10:15:30 INFO  server     Server started
10:15:30 INFO  server     name="rug-pull-test" transport="stdio"
10:15:31 DEBUG phase      Event recorded event="tools/call" count=1
10:15:31 INFO  phase      Phase transition from="trust_building" to="trigger_swap"
```

### F-003: Log Format (JSON)

The system SHALL support JSON log output for machine consumption.

**Acceptance Criteria:**
- One JSON object per line (JSONL/NDJSON)
- Includes all structured fields
- Timestamp in ISO 8601 format
- Suitable for log aggregation systems

**Format:**
```json
{"timestamp":"2025-02-04T10:15:30.123456Z","level":"INFO","target":"thoughtjack::server","message":"Server started"}
{"timestamp":"2025-02-04T10:15:30.125Z","level":"INFO","target":"thoughtjack::server","message":"Server ready","fields":{"name":"rug-pull-test","transport":"stdio"}}
{"timestamp":"2025-02-04T10:15:31.456Z","level":"DEBUG","target":"thoughtjack::phase","message":"Event recorded","fields":{"event":"tools/call","count":1}}
{"timestamp":"2025-02-04T10:15:31.789Z","level":"INFO","target":"thoughtjack::phase","message":"Phase transition","fields":{"from":"trust_building","to":"trigger_swap","trigger":"event_count"}}
```

### F-004: Contextual Logging

The system SHALL attach context to log spans.

**Acceptance Criteria:**
- Request ID attached to all logs for a request
- Phase name attached during phase execution
- Tool name attached during tool call handling
- Connection ID for HTTP transport

**Implementation:**
```rust
#[instrument(skip(self, transport), fields(request_id = %request.id))]
async fn handle_request(&self, request: JsonRpcRequest, transport: &dyn Transport) {
    debug!("Request received");
    
    // All logs within this function include request_id
    let result = self.process_request(request).await;
    
    debug!(response_size = result.len(), "Response prepared");
}

#[instrument(skip(self), fields(phase = %self.current_phase_name()))]
async fn evaluate_trigger(&self, event: &Event) {
    debug!(event_type = %event.event_type, "Evaluating trigger");
    
    if self.should_advance(event) {
        info!("Trigger fired, advancing phase");
    }
}
```

### F-005: Server Lifecycle Events

The system SHALL log server lifecycle events.

**Acceptance Criteria:**
- Server start with configuration summary
- Transport binding
- Client connection/disconnection
- Graceful shutdown initiation
- Server stop with summary

**Events:**

| Event | Level | Fields |
|-------|-------|--------|
| `server.started` | INFO | name, version, transport |
| `server.config_loaded` | INFO | phases_count, tools_count |
| `transport.listening` | INFO | transport_type, address |
| `transport.connected` | INFO | client_info, connection_id |
| `transport.disconnected` | INFO | connection_id, reason |
| `server.shutdown_started` | INFO | reason |
| `server.stopped` | INFO | uptime, requests_handled |

**Example:**
```rust
info!(
    name = %config.server.name,
    version = %config.server.version.as_deref().unwrap_or("unset"),
    transport = %transport_type,
    "Server started"
);

info!(
    phases = config.phases.len(),
    tools = config.baseline.tools.len(),
    resources = config.baseline.resources.len(),
    "Configuration loaded"
);
```

### F-006: Phase Transition Events

The system SHALL log phase transitions with full context.

**Acceptance Criteria:**
- Log when entering each phase
- Log trigger that caused transition
- Log entry actions executed
- Log effective state changes

**Events:**

| Event | Level | Fields |
|-------|-------|--------|
| `phase.entered` | INFO | phase_name, phase_index, trigger_type |
| `phase.trigger_evaluated` | DEBUG | event, count, threshold, fired |
| `phase.entry_action` | DEBUG | action_type, action_details |
| `phase.state_changed` | DEBUG | tools_added, tools_removed, tools_replaced |
| `phase.terminal` | INFO | phase_name |

**Example:**
```rust
info!(
    phase = %phase.name,
    index = phase_index,
    trigger = %trigger_description,
    "Phase transition"
);

debug!(
    added = ?diff.add_tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
    removed = ?diff.remove_tools,
    replaced = ?diff.replace_tools.keys().collect::<Vec<_>>(),
    "Effective state updated"
);
```

### F-007: Request/Response Logging

The system SHALL log MCP request/response activity.

**Acceptance Criteria:**
- Log method and ID for each request
- Log response status (success/error)
- Log timing information
- Optionally log full payloads (trace level)

**Events:**

| Event | Level | Fields |
|-------|-------|--------|
| `request.received` | DEBUG | method, id, params_size |
| `request.payload` | TRACE | full_params |
| `response.sent` | DEBUG | id, success, duration_ms |
| `response.payload` | TRACE | full_result |
| `response.error` | WARN | id, error_code, error_message |

**Example:**
```rust
debug!(
    method = %request.method,
    id = ?request.id,
    params_bytes = request.params.as_ref().map(|p| p.to_string().len()).unwrap_or(0),
    "Request received"
);

trace!(params = ?request.params, "Request payload");

debug!(
    id = ?request.id,
    success = result.is_ok(),
    duration_ms = start.elapsed().as_millis(),
    "Response sent"
);
```

### F-008: Behavioral Mode Logging

The system SHALL log behavioral mode activity.

**Acceptance Criteria:**
- Log when delivery behavior activates
- Log progress for slow behaviors
- Log side effect execution
- Log behavior completion

**Events:**

| Event | Level | Fields |
|-------|-------|--------|
| `behavior.delivery_start` | DEBUG | behavior_type, message_size |
| `behavior.delivery_progress` | TRACE | bytes_sent, total_bytes |
| `behavior.delivery_complete` | DEBUG | behavior_type, duration_ms |
| `behavior.side_effect_start` | DEBUG | effect_type, trigger |
| `behavior.side_effect_complete` | DEBUG | effect_type, messages_sent |

**Example:**
```rust
debug!(
    behavior = "slow_loris",
    message_bytes = message.len(),
    chunk_size = self.chunk_size,
    delay_ms = self.byte_delay.as_millis(),
    "Starting slow delivery"
);

trace!(
    bytes_sent = bytes_sent,
    total = total_bytes,
    progress_pct = (bytes_sent * 100) / total_bytes,
    "Delivery progress"
);
```

### F-009: Metrics Collection

The system SHALL collect quantitative metrics.

**Acceptance Criteria:**
- Counter metrics for events
- Histogram metrics for durations
- Gauge metrics for current state
- Metrics exposed via configurable sink
- **Label cardinality MUST be bounded to prevent memory exhaustion**

**Metrics:**

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `thoughtjack_requests_total` | Counter | method | Total requests received |
| `thoughtjack_responses_total` | Counter | method, status | Total responses sent |
| `thoughtjack_request_duration_ms` | Histogram | method | Request processing time |
| `thoughtjack_phase_transitions_total` | Counter | from, to | Phase transitions |
| `thoughtjack_delivery_bytes_total` | Counter | behavior | Bytes delivered |
| `thoughtjack_delivery_duration_ms` | Histogram | behavior | Delivery duration |
| `thoughtjack_side_effects_total` | Counter | effect_type | Side effects executed |
| `thoughtjack_current_phase` | Gauge | phase_name | Current phase (1 = active) |
| `thoughtjack_event_counts` | Gauge | event_type | Current event counts |

**Label Cardinality Protection:**

To prevent memory exhaustion from attacker-controlled label values, all metrics with `method` labels MUST normalize unknown methods:

```rust
const KNOWN_METHODS: &[&str] = &[
    "initialize", "ping",
    "tools/list", "tools/call",
    "resources/list", "resources/read", "resources/subscribe", "resources/unsubscribe",
    "prompts/list", "prompts/get",
    "logging/setLevel",
    "completion/complete",
];

fn sanitize_method_label(method: &str) -> &str {
    if KNOWN_METHODS.contains(&method) {
        method
    } else {
        "__unknown__"
    }
}
```

Unknown methods are bucketed as `method="__unknown__"` to limit cardinality.

**Warning: Config-Derived Cardinality:**
Labels derived from configuration (e.g., `phase_name`, `from`, `to`) are NOT sanitized because they are user-controlled, not attacker-controlled. However, configurations with an excessive number of phases (100+) will increase metric memory usage proportionally. This is a self-DoS scenario via config, not an attack vector.

**Recommendation:** Keep phase counts reasonable (<50) or disable phase-level metrics for large configurations.

**Implementation:**
```rust
use metrics::{counter, histogram, gauge};

// Request handling - use sanitized method label
let method_label = sanitize_method_label(&method);
counter!("thoughtjack_requests_total", "method" => method_label).increment(1);
let start = Instant::now();

// ... process request ...

histogram!("thoughtjack_request_duration_ms", "method" => method_label)
    .record(start.elapsed().as_millis() as f64);
counter!("thoughtjack_responses_total", "method" => method_label, "status" => "success")
    .increment(1);

// Phase transition
counter!("thoughtjack_phase_transitions_total", 
    "from" => from_phase.to_string(), 
    "to" => to_phase.to_string()
).increment(1);

gauge!("thoughtjack_current_phase", "phase_name" => phase_name).set(1.0);
```

### F-010: Metrics Export

The system SHALL support metrics export to external systems.

**Acceptance Criteria:**
- Prometheus exposition format (pull)
- Optional metrics endpoint for HTTP transport
- StatsD push support (optional feature)
- Metrics disabled by default (zero overhead)

**Configuration:**
```yaml
# In server configuration
observability:
  metrics:
    enabled: true
    endpoint: /metrics        # For HTTP transport
    format: prometheus        # prometheus | statsd
    statsd_host: localhost:8125  # For statsd format
```

**CLI:**
```bash
# Enable metrics endpoint on HTTP transport
thoughtjack server --config attack.yaml --http :8080 --metrics

# Metrics available at http://localhost:8080/metrics
```

**Prometheus Output:**
```
# HELP thoughtjack_requests_total Total requests received
# TYPE thoughtjack_requests_total counter
thoughtjack_requests_total{method="tools/call"} 15
thoughtjack_requests_total{method="tools/list"} 3

# HELP thoughtjack_request_duration_ms Request processing time
# TYPE thoughtjack_request_duration_ms histogram
thoughtjack_request_duration_ms_bucket{method="tools/call",le="1"} 10
thoughtjack_request_duration_ms_bucket{method="tools/call",le="5"} 14
thoughtjack_request_duration_ms_bucket{method="tools/call",le="10"} 15
thoughtjack_request_duration_ms_bucket{method="tools/call",le="+Inf"} 15
thoughtjack_request_duration_ms_sum{method="tools/call"} 23.5
thoughtjack_request_duration_ms_count{method="tools/call"} 15
```

### F-011: Event Stream

The system SHALL emit discrete events for significant state changes.

**Acceptance Criteria:**
- Events are structured, typed objects
- Events include timestamp and sequence number
- Events can be streamed to file
- Events enable test assertions

**Event Types:**
```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ThoughtJackEvent {
    ServerStarted {
        timestamp: DateTime<Utc>,
        server_name: String,
        transport: String,
    },
    
    PhaseEntered {
        timestamp: DateTime<Utc>,
        phase_name: String,
        phase_index: usize,
        trigger: TriggerInfo,
    },
    
    RequestReceived {
        timestamp: DateTime<Utc>,
        request_id: JsonRpcId,
        method: String,
    },
    
    ResponseSent {
        timestamp: DateTime<Utc>,
        request_id: JsonRpcId,
        success: bool,
        duration_ms: u64,
    },
    
    AttackTriggered {
        timestamp: DateTime<Utc>,
        attack_type: String,
        details: serde_json::Value,
    },
    
    ServerStopped {
        timestamp: DateTime<Utc>,
        reason: StopReason,
        summary: RunSummary,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub uptime_seconds: u64,
    pub requests_received: u64,
    pub responses_sent: u64,
    pub phases_entered: Vec<String>,
    pub final_phase: String,
    pub attacks_triggered: Vec<String>,
}
```

### F-012: Event File Output

The system SHALL support writing events to a file.

**Acceptance Criteria:**
- Events written as JSONL
- File path configurable
- Atomic writes (no partial events)
- File rotation not required (test tool)

**Warning: Parallel Test Runners:**
When running multiple ThoughtJack instances in parallel (e.g., in a test suite), each instance SHOULD write to a unique event file. While individual writes are atomic, concurrent appends from multiple processes may interleave on some operating systems without explicit file locking.

**Recommendation:** Use unique file names per instance:
```bash
thoughtjack server --config a.yaml --events-file events-$$-a.jsonl &
thoughtjack server --config b.yaml --events-file events-$$-b.jsonl &
```

**Configuration:**
```bash
# Write events to file
thoughtjack server --config attack.yaml --events-file events.jsonl

# Or via environment
THOUGHTJACK_EVENTS_FILE=events.jsonl thoughtjack server --config attack.yaml
```

**Output (events.jsonl):**
```json
{"type":"ServerStarted","timestamp":"2025-02-04T10:15:30Z","server_name":"rug-pull","transport":"stdio"}
{"type":"RequestReceived","timestamp":"2025-02-04T10:15:31Z","request_id":1,"method":"initialize"}
{"type":"ResponseSent","timestamp":"2025-02-04T10:15:31Z","request_id":1,"success":true,"duration_ms":2}
{"type":"PhaseEntered","timestamp":"2025-02-04T10:15:35Z","phase_name":"trigger_swap","phase_index":1,"trigger":{"type":"event_count","event":"tools/call","count":3}}
{"type":"AttackTriggered","timestamp":"2025-02-04T10:15:35Z","attack_type":"tool_swap","details":{"tool":"calculator","old":"benign","new":"injection"}}
{"type":"ServerStopped","timestamp":"2025-02-04T10:15:40Z","reason":"client_disconnected","summary":{"uptime_seconds":10,"requests_received":8,"responses_sent":8,"phases_entered":["trust_building","trigger_swap","exploit"],"final_phase":"exploit","attacks_triggered":["tool_swap"]}}
```

### F-013: Debug Mode

The system SHALL support a debug mode with enhanced output.

**Acceptance Criteria:**
- `--debug` flag enables all debug features
- Full request/response payloads logged
- State dumps on phase transition
- Timing information on all operations

**Debug Features:**

| Feature | Normal | Debug |
|---------|--------|-------|
| Request payloads | Not logged | Full JSON |
| Response payloads | Not logged | Full JSON |
| Phase state | Transition only | Full state dump |
| Timing | Response only | Per-operation |
| Memory usage | Not tracked | Periodic reports |

**Implementation:**
```rust
#[instrument(skip_all, fields(request_id = %request.id))]
async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
    if self.debug_mode {
        debug!(payload = ?request, "Full request payload");
    }
    
    let response = self.process(request).await;
    
    if self.debug_mode {
        debug!(payload = ?response, "Full response payload");
        debug!(
            phase_state = ?self.phase_engine.debug_state(),
            "Current phase state"
        );
    }
    
    response
}
```

### F-014: Test Report Generation

The system SHALL generate test reports on completion.

**Acceptance Criteria:**
- Summary printed to stderr on exit
- Detailed report to file (optional)
- Includes attack success/failure
- Includes timing and phase progression

**Summary Output:**
```
═══════════════════════════════════════════════════════════════════════
                         ThoughtJack Test Summary
═══════════════════════════════════════════════════════════════════════

Server: rug-pull-test
Duration: 45.3s
Transport: stdio

PHASE PROGRESSION
  [✓] trust_building     (0:00 - 0:15)  3 requests
  [✓] trigger_swap       (0:15 - 0:18)  1 request
  [✓] exploit            (0:18 - 0:45)  4 requests  [TERMINAL]

ATTACKS TRIGGERED
  [✓] Tool swap: calculator (benign → injection)
  [✓] Notification sent: tools/list_changed

REQUEST SUMMARY
  Total: 8 requests
  tools/call: 5
  tools/list: 2
  initialize: 1

BEHAVIORAL MODES
  Delivery: slow_loris (3 responses)
  Side effects: notification_flood (1 execution, 1000 notifications)

═══════════════════════════════════════════════════════════════════════
```

**Detailed Report (JSON):**
```json
{
  "server": {
    "name": "rug-pull-test",
    "version": "1.0.0"
  },
  "run": {
    "start_time": "2025-02-04T10:15:30Z",
    "end_time": "2025-02-04T10:16:15Z",
    "duration_seconds": 45.3,
    "transport": "stdio",
    "stop_reason": "client_disconnected"
  },
  "phases": [
    {
      "name": "trust_building",
      "index": 0,
      "entered_at": "2025-02-04T10:15:30Z",
      "exited_at": "2025-02-04T10:15:45Z",
      "duration_seconds": 15.0,
      "requests_handled": 3,
      "trigger": {"type": "event_count", "event": "tools/call", "count": 3}
    }
  ],
  "requests": {
    "total": 8,
    "by_method": {
      "initialize": 1,
      "tools/list": 2,
      "tools/call": 5
    },
    "errors": 0
  },
  "attacks": [
    {
      "type": "tool_swap",
      "triggered_at": "2025-02-04T10:15:45Z",
      "phase": "trigger_swap",
      "details": {
        "tool": "calculator",
        "from": "benign",
        "to": "injection"
      }
    }
  ],
  "behaviors": {
    "deliveries": {
      "slow_loris": {"count": 3, "total_bytes": 15000, "total_duration_ms": 45000}
    },
    "side_effects": {
      "notification_flood": {"executions": 1, "messages_sent": 1000}
    }
  }
}
```

### F-015: Payload Redaction

The system SHALL support redacting sensitive data from logs.

**Acceptance Criteria:**
- Redaction enabled by default at info level and below
- Full payloads only at trace level
- Configurable redaction patterns
- Redaction applies to logs and events

**Redaction Levels:**

| Log Level | Payload Visibility |
|-----------|-------------------|
| error | Method only |
| warn | Method only |
| info | Method + size |
| debug | Method + truncated (100 chars) |
| trace | Full payload |

**Implementation:**
```rust
fn format_payload(payload: &serde_json::Value, level: Level) -> String {
    match level {
        Level::TRACE => serde_json::to_string(payload).unwrap(),
        Level::DEBUG => {
            let s = serde_json::to_string(payload).unwrap();
            if s.len() > 100 {
                format!("{}... ({} bytes)", &s[..100], s.len())
            } else {
                s
            }
        }
        _ => format!("({} bytes)", serde_json::to_string(payload).unwrap().len()),
    }
}
```

### F-016: Log Targets

The system SHALL support multiple log output targets.

**Acceptance Criteria:**
- Default: stderr
- File output via `--log-file`
- Both stderr and file simultaneously
- JSON format independent per target

**Configuration:**
```bash
# Log to stderr (default)
thoughtjack server --config attack.yaml

# Log to file only
thoughtjack server --config attack.yaml --log-file server.log --quiet

# Log to both
thoughtjack server --config attack.yaml --log-file server.log

# JSON to file, human to stderr
thoughtjack server --config attack.yaml --log-file server.json --log-file-format json
```

### F-016: Request/Response Capture

The system SHALL support capturing full request/response pairs for debugging and replay.

**Acceptance Criteria:**
- `--capture-dir` enables capture to specified directory
- Creates one NDJSON file per session: `capture-{timestamp}-{pid}.ndjson`
- Each line contains request OR response with timing and phase context
- `--capture-redact` removes sensitive fields (arguments, response content)
- Distinct from event logging (which captures lifecycle events, not full payloads)

**Relationship to Other Features:**

| Feature | Purpose | Content |
|---------|---------|---------|
| Event Logging (`--events-file`) | Lifecycle events | Phase transitions, attack triggers |
| Debug Mode (`--debug`) | Live debugging | Full payloads to tracing log |
| **Capture (`--capture-dir`)** | **Replay/Analysis** | **Full request/response pairs to file** |

**Use Cases:**
1. Reproduce client bugs by replaying captured requests
2. Analyze agent behavior across sessions
3. Build test fixtures from real interactions
4. Share attack sequences with team members

**Configuration:**
```bash
# Capture all traffic
thoughtjack server --config attack.yaml --capture-dir ./captures

# Capture with redaction (for sharing)
thoughtjack server --config attack.yaml --capture-dir ./captures --capture-redact
```

**Output Format:** See TJ-SPEC-007 F-013 for detailed capture format specification.

---

## 3. Edge Cases

### EC-OBS-001: High-Volume Logging

**Scenario:** 10,000 requests/second with debug logging  
**Expected:** Logging does not significantly impact throughput; consider async logging

### EC-OBS-002: Log File Full

**Scenario:** Disk full while writing to log file  
**Expected:** Warning to stderr, continue without file logging

### EC-OBS-003: Invalid Log Level

**Scenario:** `--log-level invalid`  
**Expected:** Error message listing valid levels, exit 64

### EC-OBS-004: Metrics Endpoint Conflict

**Scenario:** `--metrics` but not using HTTP transport  
**Expected:** Warning: "Metrics endpoint requires HTTP transport"

### EC-OBS-005: Event File Not Writable

**Scenario:** `--events-file /root/events.json` without permission  
**Expected:** Error: "Cannot write to events file", exit 3

### EC-OBS-006: Very Long Log Message

**Scenario:** Log message with 1MB payload at trace level  
**Expected:** Logged correctly (no truncation at trace)

### EC-OBS-007: Unicode in Log Messages

**Scenario:** Tool name contains emoji or CJK characters  
**Expected:** Logged correctly in both human and JSON format

### EC-OBS-008: Concurrent Log Writes

**Scenario:** Multiple async tasks logging simultaneously  
**Expected:** No interleaved lines; each log entry is atomic

### EC-OBS-009: Metrics Overflow

**Scenario:** Counter exceeds u64::MAX  
**Expected:** Saturate at max value, no wrap-around

### EC-OBS-010: Phase Transition During Debug Dump

**Scenario:** Phase transitions while debug state dump is being generated  
**Expected:** Consistent snapshot (locked during dump)

### EC-OBS-011: Empty Event Stream

**Scenario:** Server starts and stops immediately  
**Expected:** ServerStarted and ServerStopped events still emitted

### EC-OBS-012: Report Generation Timeout

**Scenario:** Report generation takes > 5s  
**Expected:** Timeout and emit partial report with warning

### EC-OBS-013: JSON Log With Binary Data

**Scenario:** Payload contains non-UTF8 bytes  
**Expected:** Escaped or base64 encoded in JSON output

### EC-OBS-014: Logging Before Init

**Scenario:** Error occurs before logging is initialized  
**Expected:** Falls back to eprintln!

### EC-OBS-015: Multiple Verbosity Flags

**Scenario:** `-v -v -v` (triple verbose)  
**Expected:** Each -v increases level (info → debug → trace)

### EC-OBS-016: Quiet and Verbose Together

**Scenario:** `--quiet -v`  
**Expected:** Error: "Cannot use --quiet with --verbose", exit 64

### EC-OBS-017: Log Rotation

**Scenario:** Log file grows to 10GB  
**Expected:** No rotation (out of scope), document in help

### EC-OBS-018: Timestamp Timezone

**Scenario:** Server runs across timezone change (DST)  
**Expected:** UTC timestamps throughout (no local time confusion)

### EC-OBS-019: Metrics With No Requests

**Scenario:** Metrics endpoint queried before any requests  
**Expected:** All counters at 0, gauges at initial state

### EC-OBS-020: Report After Crash

**Scenario:** Server crashes (panic) mid-operation  
**Expected:** Panic handler emits partial summary to stderr

### EC-OBS-021: Unknown Method Metric Bucketing

**Scenario:** Client sends requests with unknown method names like "random_xyz_123"  
**Expected:** Metric recorded as `thoughtjack_requests_total{method="__unknown__"}`, not `method="random_xyz_123"`. This prevents cardinality explosion from attacker-controlled method names.

### EC-OBS-022: High Cardinality Label Attack

**Scenario:** Attacker sends 10,000 requests with unique random method names  
**Expected:** All bucketed to single `method="__unknown__"` label. Metrics memory usage remains bounded.

---

## 4. Non-Functional Requirements

### NFR-001: Logging Overhead

- Logging at info level SHALL add < 1% overhead
- Logging at trace level SHALL add < 10% overhead
- Disabled logging SHALL have zero overhead (compile-time)

### NFR-002: Memory Usage

- Log buffer SHALL not exceed 10MB
- Event buffer SHALL not exceed 1MB
- Metrics collection SHALL use < 1MB

### NFR-003: Latency Impact

- Synchronous log writes SHALL not block request handling
- Async log flushing within 100ms

### NFR-004: Reliability

- Log file writes SHALL be atomic (no partial lines)
- Metrics SHALL be accurate (no lost increments)
- Events SHALL be ordered (sequence numbers)

---

## 5. Observability Configuration

### 5.1 CLI Flags

| Flag | Environment | Default | Description |
|------|-------------|---------|-------------|
| `--log-level` | `THOUGHTJACK_LOG_LEVEL` | `info` | Log verbosity |
| `--log-format` | `THOUGHTJACK_LOG_FORMAT` | `human` | Log format (human/json) |
| `--log-file` | `THOUGHTJACK_LOG_FILE` | — | Log file path |
| `--log-file-format` | — | (same as --log-format) | Log file format |
| `--quiet` | — | false | Suppress non-error output |
| `--debug` | `THOUGHTJACK_DEBUG` | false | Enable debug mode |
| `--metrics` | — | false | Enable metrics endpoint |
| `--events-file` | `THOUGHTJACK_EVENTS_FILE` | — | Events output file |
| `--report` | — | false | Print summary on exit |
| `--report-file` | `THOUGHTJACK_REPORT_FILE` | — | Detailed report file |

### 5.2 Configuration File

```yaml
observability:
  logging:
    level: info                    # trace | debug | info | warn | error
    format: human                  # human | json
    file: /var/log/thoughtjack.log # Optional file output
    redact_payloads: true         # Redact at info and below
    
  metrics:
    enabled: false
    endpoint: /metrics            # Path for HTTP transport
    format: prometheus            # prometheus | statsd
    
  events:
    enabled: false
    file: events.jsonl
    
  reports:
    summary_on_exit: true
    detailed_file: report.json
    
  debug:
    enabled: false
    state_dumps: true
    full_payloads: true
    timing: true
```

---

## 6. Implementation Notes

### 6.1 Recommended Libraries

| Library | Purpose |
|---------|---------|
| `tracing` | Structured logging framework |
| `tracing-subscriber` | Log formatting and filtering |
| `tracing-appender` | File output with rotation |
| `metrics` | Metrics facade |
| `metrics-exporter-prometheus` | Prometheus exposition |
| `serde_json` | JSON serialization |
| `chrono` | Timestamps |

### 6.2 Logging Setup

```rust
use tracing_subscriber::{
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
    Layer,
};

pub fn init_observability(config: &ObservabilityConfig) -> Result<(), Error> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.logging.level));
    
    // Stderr layer
    let stderr_layer = fmt::layer()
        .with_target(true)
        .with_ansi(atty::is(atty::Stream::Stderr));
    
    let stderr_layer = if config.logging.format == LogFormat::Json {
        stderr_layer.json().boxed()
    } else {
        stderr_layer.boxed()
    };
    
    // File layer (optional)
    let file_layer = config.logging.file.as_ref().map(|path| {
        let file = std::fs::File::create(path)?;
        let layer = fmt::layer()
            .with_writer(file)
            .with_ansi(false);
        
        if config.logging.format == LogFormat::Json {
            Ok(layer.json().boxed())
        } else {
            Ok(layer.boxed())
        }
    }).transpose()?;
    
    // Combine layers
    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    
    Ok(())
}
```

### 6.3 Metrics Setup

```rust
use metrics_exporter_prometheus::PrometheusBuilder;

pub fn init_metrics(config: &MetricsConfig) -> Result<PrometheusHandle, Error> {
    let builder = PrometheusBuilder::new();
    
    let handle = builder
        .with_http_listener(([127, 0, 0, 1], 9090))
        .install_recorder()?;
    
    // Register metrics with descriptions
    metrics::describe_counter!(
        "thoughtjack_requests_total",
        "Total number of requests received"
    );
    metrics::describe_histogram!(
        "thoughtjack_request_duration_ms",
        "Request processing duration in milliseconds"
    );
    
    Ok(handle)
}
```

### 6.4 Event Emitter

```rust
pub struct EventEmitter {
    file: Option<BufWriter<File>>,
    sequence: AtomicU64,
}

impl EventEmitter {
    pub fn emit(&self, event: ThoughtJackEvent) {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        
        let envelope = EventEnvelope {
            sequence: seq,
            event,
        };
        
        if let Some(file) = &self.file {
            let line = serde_json::to_string(&envelope).unwrap();
            writeln!(file, "{}", line).ok();
            file.flush().ok();
        }
        
        // Also emit as tracing event for log correlation
        tracing::info!(event = ?envelope.event, "Event emitted");
    }
}
```

### 6.5 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Blocking on log writes | Stalls request handling | Use async/buffered writes |
| String formatting in hot path | Allocation overhead | Use tracing's lazy formatting |
| Global mutable state for metrics | Thread safety issues | Use atomic counters |
| Logging passwords/secrets | Security risk | Redact sensitive fields |
| Timestamps in local time | Confusing across timezones | Always use UTC |
| Unbounded log buffers | Memory exhaustion | Bounded buffers with backpressure |
| Metrics without labels | Can't filter/aggregate | Use appropriate labels |
| Too many unique label values | Cardinality explosion | Limit label cardinality |

### 6.6 Testing Strategy

**Unit Tests:**
- Log level filtering
- JSON format correctness
- Metrics increment accuracy
- Event serialization

**Integration Tests:**
- End-to-end with log capture
- Metrics endpoint scraping
- Event file verification
- Report generation

**Performance Tests:**
- Logging overhead at each level
- Metrics collection overhead
- High-throughput scenarios

---

## 7. Definition of Done

- [ ] Tracing-based logging framework initialized
- [ ] Human-readable log format implemented
- [ ] JSON log format implemented
- [ ] Log levels (trace through error) work correctly
- [ ] Contextual logging with spans/fields
- [ ] Server lifecycle events logged
- [ ] Phase transition events logged
- [ ] Request/response logging at appropriate levels
- [ ] Behavioral mode logging implemented
- [ ] Metrics collection implemented
- [ ] Prometheus metrics export works
- [ ] Event stream to file works
- [ ] Debug mode with enhanced output
- [ ] Test report generation on exit
- [ ] Payload redaction at non-trace levels
- [ ] Multiple log targets (stderr + file)
- [ ] All 22 edge cases (EC-OBS-001 through EC-OBS-022) have tests
- [ ] Logging overhead < 1% at info level (NFR-001)
- [ ] Memory usage within limits (NFR-002)
- [ ] Async logging doesn't block requests (NFR-003)
- [ ] Atomic log writes verified (NFR-004)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [TJ-SPEC-003: Phase Engine](./TJ-SPEC-003_Phase_Engine.md)
- [TJ-SPEC-004: Behavioral Modes](./TJ-SPEC-004_Behavioral_Modes.md)
- [TJ-SPEC-007: CLI Interface](./TJ-SPEC-007_CLI_Interface.md)
- [Tracing Crate](https://docs.rs/tracing/latest/tracing/)
- [Metrics Crate](https://docs.rs/metrics/latest/metrics/)
- [Prometheus Exposition Format](https://prometheus.io/docs/instrumenting/exposition_formats/)
- [NDJSON Specification](http://ndjson.org/)

---

## Appendix A: Log Message Catalog

### A.1 Server Lifecycle

| Message | Level | When |
|---------|-------|------|
| "Server started" | INFO | After config loaded, before accepting connections |
| "Configuration loaded" | INFO | After successful config parsing |
| "Transport ready" | INFO | Transport bound and listening |
| "Client connected" | INFO | New connection established |
| "Client disconnected" | INFO | Connection closed |
| "Shutdown initiated" | INFO | Signal received or shutdown requested |
| "Server stopped" | INFO | Clean shutdown complete |

### A.2 Phase Engine

| Message | Level | When |
|---------|-------|------|
| "Phase entered" | INFO | New phase activated |
| "Event recorded" | DEBUG | Event counter incremented |
| "Trigger evaluated" | DEBUG | Trigger condition checked |
| "Trigger fired" | DEBUG | Trigger condition met |
| "Entry action executed" | DEBUG | Phase entry action completed |
| "Terminal phase reached" | INFO | No more transitions possible |

### A.3 Request Handling

| Message | Level | When |
|---------|-------|------|
| "Request received" | DEBUG | MCP request parsed |
| "Request payload" | TRACE | Full request JSON |
| "Processing request" | DEBUG | Handler invoked |
| "Response prepared" | DEBUG | Response ready to send |
| "Response payload" | TRACE | Full response JSON |
| "Response sent" | DEBUG | Response transmitted |
| "Request error" | WARN | Error processing request |

### A.4 Behavioral Modes

| Message | Level | When |
|---------|-------|------|
| "Delivery started" | DEBUG | Behavioral delivery beginning |
| "Delivery progress" | TRACE | Periodic progress update |
| "Delivery complete" | DEBUG | Behavioral delivery finished |
| "Side effect triggered" | DEBUG | Side effect execution starting |
| "Side effect complete" | DEBUG | Side effect execution finished |

---

## Appendix B: Metrics Reference

### B.1 Counters

| Name | Labels | Description |
|------|--------|-------------|
| `thoughtjack_requests_total` | method | Requests received |
| `thoughtjack_responses_total` | method, status | Responses sent |
| `thoughtjack_phase_transitions_total` | from, to | Phase changes |
| `thoughtjack_delivery_bytes_total` | behavior | Bytes delivered |
| `thoughtjack_side_effects_total` | effect_type | Side effects run |
| `thoughtjack_errors_total` | error_type | Errors occurred |

### B.2 Histograms

| Name | Labels | Buckets | Description |
|------|--------|---------|-------------|
| `thoughtjack_request_duration_ms` | method | 1,5,10,50,100,500,1000 | Request latency |
| `thoughtjack_delivery_duration_ms` | behavior | 10,100,1000,10000,60000 | Delivery time |
| `thoughtjack_payload_size_bytes` | direction | 100,1K,10K,100K,1M | Payload sizes |

### B.3 Gauges

| Name | Labels | Description |
|------|--------|-------------|
| `thoughtjack_current_phase` | phase_name | Active phase (1=active) |
| `thoughtjack_event_count` | event_type | Current event counts |
| `thoughtjack_connections_active` | — | Open connections |
| `thoughtjack_uptime_seconds` | — | Server uptime |

---

## Appendix C: Event Schema

### C.1 Event Envelope

```json
{
  "sequence": 42,
  "timestamp": "2025-02-04T10:15:30.123456Z",
  "type": "PhaseEntered",
  "...event-specific fields..."
}
```

### C.2 Event Types

| Type | Fields |
|------|--------|
| `ServerStarted` | server_name, transport, config_path |
| `ServerStopped` | reason, summary |
| `PhaseEntered` | phase_name, phase_index, trigger |
| `RequestReceived` | request_id, method |
| `ResponseSent` | request_id, success, duration_ms |
| `AttackTriggered` | attack_type, phase, details |
| `SideEffectExecuted` | effect_type, trigger, result |
| `Error` | error_type, message, context |
