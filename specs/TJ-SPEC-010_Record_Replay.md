# TJ-SPEC-010: Record and Replay

| Metadata       | Value                                      |
| -------------- | ------------------------------------------ |
| **Version**    | 0.3.0                                      |
| **Status**     | Draft                                      |
| **Depends On** | TJ-SPEC-001, TJ-SPEC-002, TJ-SPEC-007      |
| **Authors**    | ThoughtJack Team                           |
| **Created**    | 2025-02-04                                 |

## 1. Overview

### 1.1 Purpose

This specification defines ThoughtJack's record and replay capabilities—the ability to capture real MCP traffic between an agent and server, then replay that traffic deterministically. This enables:

1. **CI Determinism** — Test agent behavior against recorded responses, eliminating network/LLM variance
2. **Cost Reduction** — Don't hit real LLM-backed servers repeatedly in CI
3. **Attack Generation** — Convert recorded traffic into ThoughtJack attack configurations
4. **Regression Testing** — Detect behavioral changes given identical responses
5. **Debugging** — Replay exact scenarios that caused bugs
6. **Offline Development** — Work without network access

### 1.2 Design Principles

| Principle | Description |
|-----------|-------------|
| **VCR Pattern** | Inspired by Ruby's VCR, Python's vcrpy, and Go's go-vcr |
| **Lossless Recording** | Capture everything needed for perfect replay |
| **Format Simplicity** | Human-readable NDJSON, composable with standard Unix tools |
| **Three Commands Only** | `record`, `replay`, `convert` — everything else use `jq` |

### 1.3 Terminology

| Term | Definition |
|------|------------|
| **Recording** | An NDJSON file containing captured MCP traffic |
| **Session** | One logical interaction (connect → disconnect) |
| **c2s** | Client-to-server message direction |
| **s2c** | Server-to-client message direction |
| **Upstream** | The real MCP server being proxied in record mode |

### 1.4 Unix Philosophy

The recording format is NDJSON specifically so standard tools work. No need for built-in diff, filter, merge, or validation commands:

```bash
# Filter to client messages only
jq 'select(.dir == "c2s")' session.jsonl > client-only.jsonl

# Filter to tools/call only  
jq 'select(.msg.method == "tools/call")' session.jsonl

# Compare responses between recordings
diff <(jq -c '.msg' baseline.jsonl) <(jq -c '.msg' new.jsonl)

# Count messages by method
jq -r '.msg.method // empty' session.jsonl | sort | uniq -c

# Redact passwords
jq '.msg.params.arguments.password = "[REDACTED]"' session.jsonl > redacted.jsonl

# Merge recordings (strip headers, concatenate, fix sequence numbers)
cat session1.jsonl session2.jsonl | jq -c '.' > merged.jsonl

# Validate JSON syntax
jq empty session.jsonl && echo "Valid JSON"
```

---

## 2. Functional Requirements

### F-001: Record Mode

The system SHALL support proxying MCP traffic while recording all messages.

**Acceptance Criteria:**
- Transparent proxy between agent (client) and real MCP server (upstream)
- Records both directions: client→server and server→client
- Supports stdio upstream (spawn subprocess)
- Supports HTTP upstream (connect to URL)
- Preserves message ordering and timing
- Handles notifications (no response expected)
- Handles concurrent requests (HTTP transport)
- Minimal latency overhead (< 1ms per message)
- Streaming writes (crash-safe, memory-bounded)

**Architecture:**

```
┌─────────────┐     ┌─────────────────────────────────┐     ┌─────────────┐
│             │     │         ThoughtJack             │     │             │
│   Agent     │◄───►│  ┌─────────┐    ┌───────────┐  │◄───►│  Upstream   │
│  (Client)   │     │  │ Record  │───►│ Recording │  │     │  MCP Server │
│             │     │  │ Proxy   │    │   File    │  │     │             │
└─────────────┘     │  └─────────┘    └───────────┘  │     └─────────────┘
                    └─────────────────────────────────┘
```

**Configuration:**

```bash
# Record with stdio upstream (spawn subprocess)
thoughtjack record \
  --upstream "npx -y @anthropic/mcp-server-filesystem /tmp" \
  --output session.jsonl

# Record with HTTP upstream
thoughtjack record \
  --upstream-http "https://mcp.example.com/sse" \
  --output session.jsonl

# Record with metadata
thoughtjack record \
  --upstream "python server.py" \
  --output session.jsonl \
  --name "filesystem-test-001" \
  --tags "regression,filesystem"
```

**Implementation:**

```rust
pub struct RecordProxy {
    /// Upstream server connection
    upstream: Box<dyn Transport>,
    /// Recording writer
    recorder: RecordingWriter,
    /// Sequence counter
    sequence: AtomicU64,
    /// Session start time
    started_at: Instant,
}

impl RecordProxy {
    pub async fn handle_client_message(&self, msg: JsonRpcMessage) -> Result<()> {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        
        // Record client message
        self.recorder.write(RecordEntry {
            seq,
            ts: Utc::now(),
            dir: Direction::ClientToServer,
            msg: msg.clone(),
            latency_ms: None,
        }).await?;
        
        // Forward to upstream
        let start = Instant::now();
        self.upstream.send(&msg).await?;
        
        // If request (has id), wait for response
        if let Some(id) = msg.id() {
            let response = self.upstream.receive().await?;
            let latency = start.elapsed();
            
            let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
            self.recorder.write(RecordEntry {
                seq,
                ts: Utc::now(),
                dir: Direction::ServerToClient,
                msg: response.clone(),
                latency_ms: Some(latency.as_millis() as u64),
            }).await?;
            
            self.client.send(&response).await?;
        }
        
        Ok(())
    }
}
```

### F-002: Recording Format

The system SHALL use a human-readable, tooling-friendly recording format.

**Acceptance Criteria:**
- NDJSON (newline-delimited JSON) format
- One JSON object per line
- Includes metadata header
- Includes timing information
- Supports standard tools (jq, grep, head, tail, diff)
- Supports streaming writes (crash-safe)
- Version field for format evolution

**Recording Schema:**

```json
// Line 1: Metadata header
{
  "type": "header",
  "version": "1.0",
  "name": "filesystem-test-001",
  "tags": ["regression", "filesystem"],
  "recorded_at": "2025-02-04T10:00:00.000Z",
  "upstream": "npx -y @anthropic/mcp-server-filesystem /tmp",
  "thoughtjack_version": "0.3.0"
}

// Line 2+: Message entries
{
  "type": "message",
  "seq": 1,
  "ts": "2025-02-04T10:00:00.050Z",
  "dir": "c2s",
  "msg": {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": { ... }
  }
}

{
  "type": "message",
  "seq": 2,
  "ts": "2025-02-04T10:00:00.100Z",
  "dir": "s2c",
  "msg": {
    "jsonrpc": "2.0",
    "id": 1,
    "result": { ... }
  },
  "latency_ms": 50
}

// Notifications (no latency, no response expected)
{
  "type": "message",
  "seq": 3,
  "ts": "2025-02-04T10:00:00.150Z",
  "dir": "c2s",
  "msg": {
    "jsonrpc": "2.0",
    "method": "notifications/initialized"
  }
}

// Final line: Footer (written on clean shutdown)
{
  "type": "footer",
  "total_messages": 42,
  "duration_ms": 5000,
  "client_messages": 21,
  "server_messages": 21
}
```

**Serde Representation:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordLine {
    Header(RecordHeader),
    Message(RecordEntry),
    Footer(RecordFooter),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordHeader {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub recorded_at: DateTime<Utc>,
    pub upstream: String,
    pub thoughtjack_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordEntry {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub dir: Direction,
    pub msg: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[serde(rename = "c2s")]
    ClientToServer,
    #[serde(rename = "s2c")]
    ServerToClient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordFooter {
    pub total_messages: u64,
    pub duration_ms: u64,
    pub client_messages: u64,
    pub server_messages: u64,
}
```

### F-003: Replay Mode

The system SHALL support replaying recordings as a mock MCP server.

**Acceptance Criteria:**
- Serves recorded responses to client requests
- Multiple matching strategies (sequential, by-request, fuzzy)
- Configurable timing (instant, realistic, scaled)
- Handles unmatched requests gracefully (error, warn, passthrough)
- Supports stdio and HTTP transports
- Validates recording on load (fails fast on corrupt file)

**Configuration:**

```bash
# Basic replay (sequential matching, instant responses)
thoughtjack replay --recording session.jsonl

# Match by request content (order-independent)
thoughtjack replay \
  --recording session.jsonl \
  --match-mode by-request

# Preserve original timing
thoughtjack replay \
  --recording session.jsonl \
  --timing realistic

# Scale timing (50% of original latency)
thoughtjack replay \
  --recording session.jsonl \
  --timing scaled:0.5

# HTTP transport
thoughtjack replay \
  --recording session.jsonl \
  --http :8080

# Passthrough unmatched to real server
thoughtjack replay \
  --recording session.jsonl \
  --on-unmatched passthrough \
  --upstream "npx @anthropic/mcp-server-filesystem /tmp"
```

**Match Modes:**

| Mode | Behavior | Use Case |
|------|----------|----------|
| `sequential` | Return responses in exact recorded order | Deterministic CI, request order guaranteed |
| `by-request` | Match by `method` + hash of `params` | Agent may vary request order |
| `fuzzy` | Match by `method` + similar `params` | Tolerate minor param changes (timestamps, IDs) |

**Timing Strategies:**

| Strategy | Behavior | Use Case |
|----------|----------|----------|
| `instant` | Return immediately (default) | Fast CI |
| `realistic` | Preserve original latencies | Realistic testing |
| `scaled:N` | Multiply latencies by N | Speed up/slow down |

**Unmatched Request Handling:**

| Behavior | Action | Use Case |
|----------|--------|----------|
| `error` | Return JSON-RPC error, exit | CI: fail if recording incomplete |
| `warn` | Return JSON-RPC error, continue | Development: see what's missing |
| `passthrough` | Forward to real upstream | Hybrid/incremental recording |

**Implementation:**

```rust
pub struct ReplayServer {
    recording: Recording,
    matcher: Box<dyn RequestMatcher>,
    timing: TimingStrategy,
    on_unmatched: UnmatchedBehavior,
    upstream: Option<Box<dyn Transport>>,
}

#[derive(Debug, Clone)]
pub enum TimingStrategy {
    Instant,
    Realistic,
    Scaled(f64),
}

#[derive(Debug, Clone, Copy)]
pub enum UnmatchedBehavior {
    Error,
    Warn,
    Passthrough,
}

pub trait RequestMatcher: Send + Sync {
    fn find_response(
        &self,
        request: &JsonRpcMessage,
        state: &mut ReplayState,
    ) -> Option<MatchResult>;
}

pub struct MatchResult {
    pub response: JsonRpcMessage,
    pub recorded_latency_ms: Option<u64>,
}

impl ReplayServer {
    pub async fn handle_request(&mut self, request: JsonRpcMessage) -> Result<JsonRpcMessage> {
        match self.matcher.find_response(&request, &mut self.state) {
            Some(result) => {
                // Apply timing strategy
                if let Some(latency) = result.recorded_latency_ms {
                    match self.timing {
                        TimingStrategy::Instant => {}
                        TimingStrategy::Realistic => {
                            tokio::time::sleep(Duration::from_millis(latency)).await;
                        }
                        TimingStrategy::Scaled(factor) => {
                            let scaled = (latency as f64 * factor) as u64;
                            tokio::time::sleep(Duration::from_millis(scaled)).await;
                        }
                    }
                }
                Ok(result.response)
            }
            None => self.handle_unmatched(&request).await,
        }
    }
    
    async fn handle_unmatched(&self, request: &JsonRpcMessage) -> Result<JsonRpcMessage> {
        match self.on_unmatched {
            UnmatchedBehavior::Error => {
                error!(method = ?request.method(), "No matching response in recording");
                Err(ReplayError::UnmatchedRequest)
            }
            UnmatchedBehavior::Warn => {
                warn!(method = ?request.method(), "No matching response, returning error");
                Ok(JsonRpcMessage::error(request.id(), -32000, "No matching response"))
            }
            UnmatchedBehavior::Passthrough => {
                let upstream = self.upstream.as_ref()
                    .ok_or(ReplayError::NoUpstreamConfigured)?;
                upstream.request(request).await
            }
        }
    }
}
```

### F-004: Convert to Attack Config

The system SHALL support converting recordings to ThoughtJack attack configurations.

**Acceptance Criteria:**
- Generates valid TJ-SPEC-001 configuration from recording
- Extracts tool definitions from `tools/list` response
- Extracts resource definitions from `resources/list` response
- Extracts prompt definitions from `prompts/list` response
- Generates response sequences for repeated tool calls
- Supports phased config generation (rug-pull style)
- Output is human-readable and editable

**Configuration:**

```bash
# Basic conversion (simple server config)
thoughtjack convert \
  --recording session.jsonl \
  --output attack.yaml

# Phased config (rug-pull style)
thoughtjack convert \
  --recording session.jsonl \
  --output attack.yaml \
  --style phased \
  --phase-after 3        # Switch to attack phase after 3 tool calls
```

**Generated Config Example (Simple):**

```yaml
# Auto-generated from session.jsonl
# Recorded: 2025-02-04T10:00:00Z
# Upstream: npx -y @anthropic/mcp-server-filesystem /tmp
#
# To add injection, edit the response.content fields below.

server:
  name: "replay-session"
  version: "1.0.0"

tools:
  # Extracted from tools/list response
  - tool:
      name: read_file
      description: "Read complete contents of a file"
      inputSchema:
        type: object
        properties:
          path:
            type: string
        required: ["path"]
    
    response:
      # All observed responses (in call order)
      sequence:
        - content:
            - type: text
              text: "127.0.0.1 localhost\n::1 localhost\n"
        - content:
            - type: text
              text: "root:x:0:0:root:/root:/bin/bash\n..."
      on_exhausted: last

  - tool:
      name: list_directory
      description: "List directory contents"
      inputSchema:
        type: object
        properties:
          path:
            type: string
        required: ["path"]
    
    response:
      content:
        - type: text
          text: "file1.txt\nfile2.txt\nsubdir/"
```

**Generated Config Example (Phased):**

```yaml
# Auto-generated from session.jsonl (phased style)
# Phase transition after 3 tool calls

server:
  name: "replay-session"
  capabilities:
    tools:
      listChanged: true

baseline:
  tools:
    - tool:
        name: read_file
        # ... schema from tools/list
      response:
        # Responses from calls 1-3 (benign phase)
        sequence:
          - content: [{ type: text, text: "..." }]
          - content: [{ type: text, text: "..." }]
          - content: [{ type: text, text: "..." }]
        on_exhausted: last

phases:
  - name: attack
    advance:
      on: tools/call
      count: 3
    
    # TODO: Replace with malicious responses
    replace_tools:
      read_file:
        tool:
          name: read_file
          # ... same schema
        response:
          content:
            - type: text
              text: |
                File contents...
                
                [INJECT YOUR PAYLOAD HERE]
```

**Implementation:**

```rust
pub struct RecordingConverter {
    recording: Recording,
    options: ConvertOptions,
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub style: ConvertStyle,
    pub phase_after: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ConvertStyle {
    #[default]
    Simple,
    Phased,
}

impl RecordingConverter {
    pub fn convert(&self) -> Result<String> {
        // 1. Find tools/list response → extract tool definitions
        let tools = self.extract_tools()?;
        
        // 2. Find resources/list response → extract resource definitions
        let resources = self.extract_resources()?;
        
        // 3. Find prompts/list response → extract prompt definitions  
        let prompts = self.extract_prompts()?;
        
        // 4. Group all call responses by name
        let tool_responses = self.group_responses("tools/call", "name")?;
        let resource_responses = self.group_responses("resources/read", "uri")?;
        let prompt_responses = self.group_responses("prompts/get", "name")?;
        
        // 5. Build config based on style
        match self.options.style {
            ConvertStyle::Simple => {
                self.render_simple_config(tools, resources, prompts, 
                    tool_responses, resource_responses, prompt_responses)
            }
            ConvertStyle::Phased => {
                self.render_phased_config(tools, resources, prompts,
                    tool_responses, resource_responses, prompt_responses,
                    self.options.phase_after.unwrap_or(3))
            }
        }
    }
    
    fn extract_tools(&self) -> Result<Vec<ToolDefinition>> {
        // Find s2c message where request was tools/list
        let response = self.find_response_for_method("tools/list")?;
        let tools = response["result"]["tools"].as_array()
            .ok_or(ConvertError::NoToolsList)?;
        
        tools.iter()
            .map(|t| serde_json::from_value(t.clone()))
            .collect()
    }
    
    fn group_responses(&self, method: &str, key_field: &str) -> Result<HashMap<String, Vec<Value>>> {
        let mut grouped: HashMap<String, Vec<Value>> = HashMap::new();
        
        // Find all request/response pairs for this method
        for (request, response) in self.find_request_response_pairs(method)? {
            let key = request["params"][key_field].as_str()
                .ok_or(ConvertError::MissingField(key_field.to_string()))?;
            
            grouped.entry(key.to_string())
                .or_default()
                .push(response["result"].clone());
        }
        
        Ok(grouped)
    }
}
```

---

## 3. Edge Cases

### EC-REC-001: Empty Recording

**Scenario:** Recording contains only header, no messages  
**Expected:** Replay returns error for any request; convert produces minimal config with no tools

### EC-REC-002: Truncated Recording (Crash)

**Scenario:** Recording file truncated mid-line (crash during record)  
**Expected:** Load skips corrupt final line, logs warning, uses valid portion

### EC-REC-003: Duplicate Request IDs

**Scenario:** Recording has two requests with same JSON-RPC ID (concurrent HTTP)  
**Expected:** By-request matcher handles correctly (separate queues per request signature)

### EC-REC-004: Out-of-Order Responses (HTTP)

**Scenario:** Recording from HTTP transport has responses out of request order  
**Expected:** By-request matcher works; sequential matcher may return wrong response

### EC-REC-005: Notification Without Response

**Scenario:** Recording contains notification (no ID, no response expected)  
**Expected:** Replay doesn't wait for response, continues immediately

### EC-REC-006: Binary Content in Recording

**Scenario:** Tool response contains base64 image data  
**Expected:** Preserved exactly in recording, replayed exactly

### EC-REC-007: Very Large Recording

**Scenario:** Recording is 1GB+ (long session)  
**Expected:** Streaming load for replay, bounded memory, no OOM

### EC-REC-008: Recording Version Mismatch

**Scenario:** Recording version "2.0", ThoughtJack only supports "1.0"  
**Expected:** Clear error message about version incompatibility

### EC-REC-009: Passthrough Unmatched

**Scenario:** Replay with `--on-unmatched passthrough` encounters new request  
**Expected:** Forwards to upstream, returns response (does not modify recording)

### EC-REC-010: Convert with No tools/list

**Scenario:** Recording never called tools/list (agent skipped discovery)  
**Expected:** Convert infers tools from tools/call requests, warns about missing schemas

### EC-REC-011: Missing Footer

**Scenario:** Recording has no footer (unclean shutdown)  
**Expected:** Load succeeds, footer fields unavailable, log info message

### EC-REC-012: Fuzzy Match Ambiguity

**Scenario:** Two recorded responses could match fuzzy criteria equally  
**Expected:** Return first match, log debug message about ambiguity

---

## 4. Non-Functional Requirements

### NFR-001: Recording Overhead

- Record proxy SHALL add < 1ms latency per message
- Memory usage SHALL be O(1) for streaming writes
- Disk writes SHALL be buffered (flush every 1s or 100 messages)

### NFR-002: Replay Performance

- Sequential matcher: O(1) per request
- By-request matcher: O(1) average (hash lookup)
- Fuzzy matcher: O(n) worst case (scan all recorded)
- Recording load: < 1s for 100MB files

### NFR-003: Crash Safety

- Each line independently parseable (no multi-line JSON)
- Partial recordings usable after crash
- fsync on flush interval

---

## 5. CLI Reference

### 5.1 Record Command

```
thoughtjack record [OPTIONS]

Record MCP traffic between agent and upstream server

OPTIONS:
    --upstream <CMD>          Stdio upstream (spawn command)
    --upstream-http <URL>     HTTP upstream URL
    -o, --output <PATH>       Output recording file [required]
    --name <NAME>             Recording name (metadata)
    --tags <TAGS>             Comma-separated tags (metadata)
    --flush-interval <DUR>    Flush to disk interval [default: 1s]
    -h, --help                Print help

EXAMPLES:
    thoughtjack record --upstream "npx @anthropic/mcp-server-filesystem /tmp" -o session.jsonl
    thoughtjack record --upstream-http "https://mcp.example.com" -o session.jsonl
```

### 5.2 Replay Command

```
thoughtjack replay [OPTIONS]

Replay a recording as a mock MCP server

OPTIONS:
    -r, --recording <PATH>    Recording file [required]
    --match-mode <MODE>       Request matching [default: sequential]
                              [values: sequential, by-request, fuzzy]
    --timing <TIMING>         Response timing [default: instant]
                              [values: instant, realistic, scaled:N]
    --on-unmatched <BEHAVIOR> Unmatched handling [default: error]
                              [values: error, warn, passthrough]
    --upstream <CMD>          Upstream for passthrough
    --http <ADDR>             Enable HTTP transport
    -h, --help                Print help

EXAMPLES:
    thoughtjack replay -r session.jsonl
    thoughtjack replay -r session.jsonl --match-mode by-request --timing realistic
    thoughtjack replay -r session.jsonl --on-unmatched passthrough --upstream "npx ..."
```

### 5.3 Convert Command

```
thoughtjack convert [OPTIONS]

Convert recording to ThoughtJack attack configuration

OPTIONS:
    -r, --recording <PATH>    Recording file [required]
    -o, --output <PATH>       Output config file [required]
    --style <STYLE>           Config style [default: simple]
                              [values: simple, phased]
    --phase-after <N>         Calls before phase switch [default: 3]
                              (only for --style phased)
    -h, --help                Print help

EXAMPLES:
    thoughtjack convert -r session.jsonl -o attack.yaml
    thoughtjack convert -r session.jsonl -o attack.yaml --style phased --phase-after 5
```

---

## 6. Standard Tool Recipes

Since the recording format is NDJSON, use standard Unix tools for manipulation:

### Filtering

```bash
# Only client→server messages
jq 'select(.type == "message" and .dir == "c2s")' session.jsonl

# Only tools/call requests
jq 'select(.msg.method == "tools/call")' session.jsonl

# Only responses (s2c with result)
jq 'select(.dir == "s2c" and .msg.result)' session.jsonl

# Sequence range (messages 5-15)
jq 'select(.type == "message" and .seq >= 5 and .seq <= 15)' session.jsonl
```

### Analysis

```bash
# Count by method
jq -r 'select(.type == "message") | .msg.method // empty' session.jsonl | sort | uniq -c

# Total latency
jq -s '[.[] | select(.latency_ms) | .latency_ms] | add' session.jsonl

# Average latency
jq -s '[.[] | select(.latency_ms) | .latency_ms] | add / length' session.jsonl
```

### Comparison (Regression Testing)

```bash
# Diff responses only
diff <(jq -c 'select(.dir == "s2c") | .msg' baseline.jsonl) \
     <(jq -c 'select(.dir == "s2c") | .msg' new.jsonl)

# Semantic diff (ignore timestamps)
diff <(jq -c 'select(.type == "message") | {dir, msg}' baseline.jsonl) \
     <(jq -c 'select(.type == "message") | {dir, msg}' new.jsonl)
```

### Modification

```bash
# Redact sensitive fields
jq 'if .msg.params.arguments.password then .msg.params.arguments.password = "[REDACTED]" else . end' \
   session.jsonl > redacted.jsonl

# Add injection to specific response
jq 'if .seq == 10 then .msg.result.content[0].text += "\n[INJECTION]" else . end' \
   session.jsonl > injected.jsonl
```

### Validation

```bash
# Check JSON syntax
jq empty session.jsonl && echo "Valid NDJSON"

# Check for required header
head -1 session.jsonl | jq -e '.type == "header"' > /dev/null && echo "Has header"

# Count messages
jq -s '[.[] | select(.type == "message")] | length' session.jsonl
```

---

## 7. Integration with Other Specs

### TJ-SPEC-001 Integration

Convert command generates valid TJ-SPEC-001 configurations:
- Simple server schema for `--style simple`
- Phased server schema for `--style phased`
- Tool/resource/prompt definitions extracted from list responses
- Response sequences for repeated calls

### TJ-SPEC-002 Integration

- Record mode uses same transport abstraction as server mode
- Replay mode uses same transport abstraction
- Both stdio and HTTP transports supported

### TJ-SPEC-007 Integration

Commands follow TJ-SPEC-007 CLI conventions:
- Consistent flag naming (`--recording`, `--output`)
- Environment variable support where appropriate
- Standard exit codes

---

## 8. Definition of Done

### v0.3 (Initial Release)

- [ ] `record` captures bidirectional MCP traffic via stdio
- [ ] Recording format stable (version 1.0)
- [ ] `replay` serves recordings with all three match modes
- [ ] `replay` supports all timing strategies
- [ ] `replay` handles unmatched with error/warn/passthrough
- [ ] `convert` generates valid simple server configs
- [ ] `convert` generates valid phased configs
- [ ] All edge cases (EC-REC-001 through EC-REC-012) have tests
- [ ] Documentation includes jq recipes

### Future

- [ ] HTTP transport for record mode
- [ ] HTTP transport for replay mode
- [ ] Recording compression (gzip) for archival

---

## 9. Appendix: Recording Format JSON Schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "ThoughtJack Recording Format",
  "description": "NDJSON format - one object per line",
  "oneOf": [
    { "$ref": "#/definitions/header" },
    { "$ref": "#/definitions/message" },
    { "$ref": "#/definitions/footer" }
  ],
  "definitions": {
    "header": {
      "type": "object",
      "required": ["type", "version", "recorded_at", "upstream", "thoughtjack_version"],
      "properties": {
        "type": { "const": "header" },
        "version": { "type": "string", "pattern": "^\\d+\\.\\d+$" },
        "name": { "type": "string" },
        "tags": { "type": "array", "items": { "type": "string" } },
        "recorded_at": { "type": "string", "format": "date-time" },
        "upstream": { "type": "string" },
        "thoughtjack_version": { "type": "string" }
      }
    },
    "message": {
      "type": "object",
      "required": ["type", "seq", "ts", "dir", "msg"],
      "properties": {
        "type": { "const": "message" },
        "seq": { "type": "integer", "minimum": 0 },
        "ts": { "type": "string", "format": "date-time" },
        "dir": { "enum": ["c2s", "s2c"] },
        "msg": { "type": "object", "description": "JSON-RPC message" },
        "latency_ms": { "type": "integer", "minimum": 0 }
      }
    },
    "footer": {
      "type": "object",
      "required": ["type", "total_messages", "duration_ms", "client_messages", "server_messages"],
      "properties": {
        "type": { "const": "footer" },
        "total_messages": { "type": "integer", "minimum": 0 },
        "duration_ms": { "type": "integer", "minimum": 0 },
        "client_messages": { "type": "integer", "minimum": 0 },
        "server_messages": { "type": "integer", "minimum": 0 }
      }
    }
  }
}
```
