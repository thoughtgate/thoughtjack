# TJ-SPEC-002: Transport Abstraction

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-002` |
| **Title** | Transport Abstraction |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **Critical** |
| **Version** | v1.0.0 |
| **Tags** | `#transport` `#stdio` `#http` `#sse` `#ndjson` `#behavioral-attacks` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's transport abstraction layer, enabling the same attack configurations to execute correctly across both MCP transport types.

### 1.1 Motivation

MCP supports two transports with fundamentally different characteristics:

| Transport | Framing | Connection | Use Case |
|-----------|---------|------------|----------|
| **stdio** | Newline-delimited JSON (NDJSON) | Persistent pipes (stdin/stdout) | Local development (Claude Desktop, Cursor, VS Code) |
| **HTTP+SSE** | HTTP POST + Server-Sent Events | Request/response + streaming | Cloud deployments, remote servers |

Behavioral attacks must adapt to each transport's mechanics:
- **Slow loris** on stdio: drip individual bytes with delays
- **Slow loris** on HTTP: use chunked transfer encoding with delayed chunks
- **Pipe deadlock** on stdio: fill stdout buffer while ignoring stdin
- **Pipe deadlock** on HTTP: not applicable (no bidirectional pipes)

Without a transport abstraction, attack implementations would duplicate logic or behave incorrectly on one transport.

### 1.2 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Single attack definition** | Config files should not specify transport-specific details |
| **Behavior adaptation** | The transport layer adapts behaviors to its mechanics |
| **Graceful degradation** | Transport-incompatible behaviors are skipped with warning |
| **Auto-detection** | Transport type is inferred from invocation context |

### 1.3 Transport Detection Strategy

ThoughtJack determines transport type based on invocation:

| Invocation | Transport | Detection Method |
|------------|-----------|------------------|
| `thoughtjack server --config config.yaml` | stdio | Default when stdin is a pipe/TTY |
| `thoughtjack server --config config.yaml --http :8080` | HTTP | Explicit `--http` flag |
| `THOUGHTJACK_TRANSPORT=http` | HTTP | Environment variable override |
| Spawned by MCP client | stdio | Stdin/stdout are pipes |

### 1.4 Scope Boundaries

**In scope:**
- Transport trait definition and implementations
- Message framing (NDJSON for stdio, HTTP for SSE)
- Behavior adaptation per transport
- Connection lifecycle management
- Error handling and recovery

**Out of scope:**
- MCP protocol semantics (handled by Protocol Handler)
- Phase transitions (handled by Phase Engine)
- Attack content generation (handled by Payload Generator)

---

## 2. Functional Requirements

### F-001: Transport Trait

The system SHALL define a `Transport` trait that abstracts message sending and receiving.

**Acceptance Criteria:**
- Trait provides async methods for sending and receiving JSON-RPC messages
- Trait provides method for sending raw bytes (for behavioral attacks)
- Trait provides connection lifecycle hooks (on_connect, on_disconnect)
- Implementations exist for stdio and HTTP transports

**Trait Definition:**
```rust
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a complete JSON-RPC message
    async fn send_message(&self, message: &JsonRpcMessage) -> Result<(), TransportError>;
    
    /// Send raw bytes (for behavioral attacks that manipulate framing)
    async fn send_raw(&self, bytes: &[u8]) -> Result<(), TransportError>;
    
    /// Receive the next JSON-RPC message
    async fn receive_message(&self) -> Result<JsonRpcMessage, TransportError>;
    
    /// Apply a delivery behavior to a message
    async fn send_with_behavior(
        &self,
        message: &JsonRpcMessage,
        behavior: &DeliveryBehavior,
    ) -> Result<(), TransportError>;
    
    /// Check if a side effect is supported on this transport
    fn supports_side_effect(&self, effect: &SideEffectType) -> bool;
    
    /// Execute a side effect
    async fn execute_side_effect(&self, effect: &SideEffect) -> Result<(), TransportError>;
    
    /// Close the connection
    async fn close(&self, graceful: bool) -> Result<(), TransportError>;
    
    /// Get transport type for logging/metrics
    fn transport_type(&self) -> TransportType;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    Stdio,
    Http,
}
```

### F-002: stdio Transport Implementation

The system SHALL implement the `Transport` trait for stdio (NDJSON over stdin/stdout).

**Acceptance Criteria:**
- Reads NDJSON messages from stdin (one JSON object per line)
- Writes NDJSON messages to stdout (one JSON object per line, terminated with `\n`)
- Handles partial reads and writes correctly
- Supports all delivery behaviors
- Supports stdio-specific side effects (pipe_deadlock)

**Message Framing:**
```
┌─────────────────────────────────────────────────────────┐
│ stdin                                                   │
│ {"jsonrpc":"2.0","method":"tools/call","id":1,...}\n   │
│ {"jsonrpc":"2.0","method":"tools/list","id":2}\n       │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│ stdout                                                  │
│ {"jsonrpc":"2.0","result":{...},"id":1}\n              │
│ {"jsonrpc":"2.0","result":[...],"id":2}\n              │
└─────────────────────────────────────────────────────────┘
```

### F-003: HTTP Transport Implementation

The system SHALL implement the `Transport` trait for HTTP+SSE.

**Acceptance Criteria:**
- Listens on configured port for HTTP POST requests
- Responds with Server-Sent Events for streaming responses
- Supports chunked transfer encoding for behavioral attacks
- Handles multiple concurrent connections
- Supports HTTP-specific behaviors (response delays via headers)

**Message Flow:**
```
Client                                    ThoughtJack (HTTP)
  │                                            │
  │  POST /message                             │
  │  Content-Type: application/json            │
  │  {"jsonrpc":"2.0","method":"tools/call"}   │
  │ ─────────────────────────────────────────▶ │
  │                                            │
  │  HTTP/1.1 200 OK                           │
  │  Content-Type: text/event-stream           │
  │  Transfer-Encoding: chunked                │
  │                                            │
  │  data: {"jsonrpc":"2.0","result":{...}}    │
  │ ◀───────────────────────────────────────── │
  │                                            │
```

### F-004: Delivery Behavior Adaptation

The system SHALL adapt delivery behaviors to each transport's mechanics.

**Acceptance Criteria:**

| Behavior | stdio Implementation | HTTP Implementation |
|----------|---------------------|---------------------|
| `normal` | Write message + `\n` | Single HTTP response |
| `slow_loris` | Byte-by-byte writes with `sleep()` | Chunked encoding with delayed chunks |
| `unbounded_line` | Write bytes without `\n` terminator | `Content-Length` omitted, stream until close |
| `nested_json` | Wrap message in nesting, write normally | Same wrapping, normal HTTP response |
| `response_delay` | `sleep()` before writing | `sleep()` before sending response |

**HTTP Behavior Target Clarification:**

In MCP over HTTP, there are two communication channels:
1. **POST Response** — The HTTP response to a `POST /message` request (for `tools/call`, etc.)
2. **SSE Stream** — Server-Sent Events channel for notifications

**Delivery behaviors apply to the direct HTTP response of the request being served:**

| Request | Behavior Applies To | Example |
|---------|---------------------|---------|
| `tools/call` | POST response body | `slow_loris` drips the tool result |
| `tools/list` | POST response body | `response_delay` delays the tool list |
| Notifications | SSE stream | `notification_flood` writes to SSE |

This means `slow_loris` on a `tools/call` response directly affects the client's latency for that specific tool call, not the SSE notification channel.

**Warning: SSE Stream Global Impact:**

Delivery behaviors applied to **notifications** affect the **shared SSE stream** for that connection. Unlike POST responses (which are isolated per-request), the SSE stream is a single channel carrying all notifications:

- `slow_loris` on a notification will delay ALL subsequent notifications on that connection
- `notification_flood` monopolizes the SSE stream bandwidth
- This may cause unrelated progress notifications or events to queue behind the behavioral delay

This is often the desired test behavior (testing client SSE handling under adverse conditions), but be aware of the global impact when designing tests.

**slow_loris on stdio:**
```rust
async fn send_slow_loris_stdio(
    &self,
    message: &JsonRpcMessage,
    byte_delay_ms: u64,
) -> Result<(), TransportError> {
    let serialized = serde_json::to_string(message)?;
    let bytes = serialized.as_bytes();
    
    for byte in bytes {
        self.stdout.write_all(&[*byte]).await?;
        self.stdout.flush().await?;
        tokio::time::sleep(Duration::from_millis(byte_delay_ms)).await;
    }
    
    // Finally send the newline
    self.stdout.write_all(b"\n").await?;
    self.stdout.flush().await?;
    
    Ok(())
}
```

**slow_loris on HTTP:**
```rust
async fn send_slow_loris_http(
    &self,
    message: &JsonRpcMessage,
    byte_delay_ms: u64,
) -> Result<Response<Body>, TransportError> {
    let serialized = format!("data: {}\n\n", serde_json::to_string(message)?);
    let bytes = serialized.into_bytes();
    
    let stream = stream::iter(bytes.into_iter().map(move |byte| {
        async move {
            tokio::time::sleep(Duration::from_millis(byte_delay_ms)).await;
            Ok::<_, std::io::Error>(Bytes::from(vec![byte]))
        }
    }))
    .buffered(1);
    
    Ok(Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Transfer-Encoding", "chunked")
        .body(Body::wrap_stream(stream))?)
}
```

### F-005: Side Effect Adaptation

The system SHALL adapt side effects to each transport, skipping unsupported effects.

**Acceptance Criteria:**

| Side Effect | stdio Support | HTTP Support | Notes |
|-------------|---------------|--------------|-------|
| `notification_flood` | ✅ | ✅ | Write to stdout / SSE stream |
| `batch_amplify` | ✅ | ✅ | Same on both |
| `pipe_deadlock` | ✅ | ❌ | No pipes in HTTP |
| `close_connection` | ✅ | ✅ | Close pipes / HTTP connection |
| `duplicate_request_ids` | ✅ | ✅ | Same on both |

- Unsupported side effects log warning and are skipped
- `supports_side_effect()` method allows checking before execution

### F-006: Connection Lifecycle

The system SHALL manage connection lifecycle appropriately for each transport.

**stdio Lifecycle:**
```
┌─────────────────────────────────────────────────────────┐
│                     stdio Lifecycle                      │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  Start ──▶ Read stdin ──▶ Process ──▶ Write stdout     │
│              │                           │              │
│              │         ┌─────────────────┘              │
│              │         │                                │
│              ▼         ▼                                │
│           EOF?  ──▶  Loop                               │
│              │                                          │
│              ▼                                          │
│            Exit                                         │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

**HTTP Lifecycle:**
```
┌─────────────────────────────────────────────────────────┐
│                     HTTP Lifecycle                       │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  Start ──▶ Bind port ──▶ Accept connection             │
│                              │                          │
│              ┌───────────────┘                          │
│              │                                          │
│              ▼                                          │
│         Read request ──▶ Process ──▶ Send response     │
│              │                           │              │
│              │         ┌─────────────────┘              │
│              │         │                                │
│              ▼         ▼                                │
│         Connection   Loop (keep-alive)                  │
│          closed?                                        │
│              │                                          │
│              ▼                                          │
│         Accept next                                     │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

**Acceptance Criteria:**
- stdio: Exit cleanly on stdin EOF
- stdio: Handle SIGTERM/SIGINT gracefully
- HTTP: Support multiple concurrent connections
- HTTP: Respect `Connection: close` header
- Both: Execute `close_connection` side effect correctly

### F-007: Error Handling

The system SHALL handle transport errors gracefully.

**Acceptance Criteria:**

| Error | stdio Handling | HTTP Handling |
|-------|----------------|---------------|
| Write failure | Log error, exit | Log error, close connection |
| Read failure | Log error, exit | Log error, close connection |
| Malformed JSON | Log warning, skip message | Return 400 Bad Request |
| Message too large | Log error, skip message | Return 413 Payload Too Large |
| Connection reset | Exit cleanly | Close connection, accept next |

**Error Types:**
```rust
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    
    #[error("Message too large: {size} bytes (limit: {limit})")]
    MessageTooLarge { size: usize, limit: usize },
    
    #[error("Connection closed")]
    ConnectionClosed,
    
    #[error("Unsupported side effect for transport: {0:?}")]
    UnsupportedSideEffect(SideEffectType),
    
    #[error("HTTP error: {0}")]
    Http(String),
}
```

### F-008: Message Size Limits

The system SHALL enforce message size limits to prevent resource exhaustion.

**Acceptance Criteria:**
- Default maximum message size: 10 MB
- Configurable via `THOUGHTJACK_MAX_MESSAGE_SIZE` environment variable
- Messages exceeding limit are rejected before full read
- Clear error message indicates the limit

### F-009: Concurrent Message Handling

The system SHALL handle messages appropriately for each transport's concurrency model.

**Acceptance Criteria:**
- stdio: Process messages sequentially (single stdin/stdout)
- HTTP: Process requests concurrently (multiple connections)
- Phase state scoping is configurable (see F-015)
- Side effects are coordinated to avoid interleaving issues

### F-015: Connection State Isolation

The system SHALL support configurable state scoping for HTTP transport.

**Acceptance Criteria:**
- `per_connection` mode (default): Each HTTP connection instantiates independent PhaseEngine
- `global` mode: Single PhaseEngine shared via `Arc<RwLock<PhaseEngine>>`
- stdio transport always uses single state (only one connection possible)
- Mode configurable via `server.state_scope` config or `--state-scope` CLI flag

**Configuration:**
```yaml
server:
  name: "test-server"
  state_scope: per_connection  # or: global
```

**Rationale:**
- `per_connection` (default): Enables deterministic, reproducible testing where each client connection experiences the full attack sequence independently
- `global`: Simulates server-side state poisoning where one client's actions affect all other connected clients

**Implementation:**
```rust
pub enum StateScope {
    /// Each connection gets independent PhaseEngine instance
    PerConnection,
    /// All connections share Arc<RwLock<PhaseEngine>>
    Global,
}

impl Server {
    fn get_phase_engine(&self, connection_id: ConnectionId) -> PhaseEngineHandle {
        match self.config.state_scope {
            StateScope::PerConnection => {
                PhaseEngineHandle::Owned(PhaseEngine::new(&self.config))
            }
            StateScope::Global => {
                PhaseEngineHandle::Shared(self.global_engine.clone())
            }
        }
    }
}
```

### F-016: Connection Context

The system SHALL provide connection context for side effect targeting.

**Acceptance Criteria:**
- Each connection has a unique `ConnectionId`
- Side effects receive `ConnectionContext` for targeting
- `close_connection` closes the specific connection, not all connections
- Notifications can target specific connection or broadcast (global mode only)

**Connection Context:**
```rust
/// Context for the current connection
pub struct ConnectionContext {
    /// Unique connection ID (always 0 for stdio)
    pub connection_id: u64,
    
    /// Handle to close THIS specific connection
    pub connection_handle: Arc<dyn ConnectionHandle>,
    
    /// Remote address (for HTTP)
    pub remote_addr: Option<SocketAddr>,
    
    /// Whether this is the only connection (stdio: always true)
    pub is_exclusive: bool,
}

#[async_trait]
pub trait ConnectionHandle: Send + Sync {
    /// Close this specific connection
    async fn close(&self, graceful: bool) -> Result<(), TransportError>;
}
```

---

## 3. Edge Cases

### EC-TRANS-001: stdin EOF During Message

**Scenario:** stdin closes mid-message (partial JSON received)  
**Expected:** Log warning with partial content, exit cleanly

### EC-TRANS-002: stdout Write Blocks (Buffer Full)

**Scenario:** Client not reading stdout, buffer fills up  
**Expected:** Write blocks; if `pipe_deadlock` side effect active, this is intentional

### EC-TRANS-003: Slow Client During slow_loris

**Scenario:** Client reads slower than byte drip rate  
**Expected:** Backpressure handled by OS buffers; continue dripping

### EC-TRANS-004: HTTP Connection Closed Mid-Response

**Scenario:** Client closes connection during chunked response  
**Expected:** Log info, stop sending, clean up resources

### EC-TRANS-005: Concurrent HTTP Requests to Same Endpoint

**Scenario:** Multiple POST requests arrive simultaneously  
**Expected:** Each processed independently; phase state transitions are atomic

### EC-TRANS-006: HTTP Request With No Body

**Scenario:** POST with `Content-Length: 0`  
**Expected:** Return 400 Bad Request (JSON-RPC requires body)

### EC-TRANS-007: HTTP Request With Invalid JSON

**Scenario:** POST body is not valid JSON  
**Expected:** Return 400 Bad Request with error details

### EC-TRANS-008: stdio Message Without Newline at EOF

**Scenario:** Last message in stdin has no trailing `\n`  
**Expected:** Parse and process the message (newline optional at EOF)

### EC-TRANS-009: Empty Lines in stdin

**Scenario:** stdin contains blank lines between messages  
**Expected:** Skip empty lines, continue processing

### EC-TRANS-010: HTTP Keep-Alive Timeout

**Scenario:** Client opens connection but sends no request  
**Expected:** Close connection after configurable timeout (default 60s)

### EC-TRANS-011: pipe_deadlock on HTTP

**Scenario:** Config specifies `pipe_deadlock` side effect, running on HTTP  
**Expected:** Log warning "pipe_deadlock not supported on HTTP transport", skip effect

### EC-TRANS-012: Extremely Large Slow Loris Message

**Scenario:** 10MB message with slow_loris at 1 byte/100ms  
**Expected:** Would take ~11.5 days; allow but warn about duration

### EC-TRANS-013: SIGTERM During slow_loris

**Scenario:** Server receives SIGTERM while dripping bytes  
**Expected:** Stop dripping, exit cleanly (do not complete message)

### EC-TRANS-014: stdio Binary Data in Message

**Scenario:** JSON message contains base64-encoded binary (valid)  
**Expected:** Process normally (JSON handles this)

### EC-TRANS-015: HTTP Request With Wrong Content-Type

**Scenario:** POST with `Content-Type: text/plain`  
**Expected:** Accept anyway (be lenient), process as JSON

### EC-TRANS-016: Multiple JSON Objects on One Line (stdio)

**Scenario:** stdin contains `{...}{...}\n`  
**Expected:** Parse error for line (not valid NDJSON), log warning, skip

### EC-TRANS-017: Unicode in Messages

**Scenario:** JSON message contains Unicode characters  
**Expected:** Handle correctly (UTF-8 throughout)

### EC-TRANS-018: Zero Byte Delay in slow_loris

**Scenario:** `slow_loris` with `byte_delay_ms: 0`  
**Expected:** Effectively normal delivery (no sleeps)

### EC-TRANS-019: close_connection With Graceful=true

**Scenario:** `close_connection` side effect with `graceful: true`  
**Expected:** stdio: close stdin, drain stdout, exit; HTTP: send complete response, then close

### EC-TRANS-020: close_connection With Graceful=false

**Scenario:** `close_connection` side effect with `graceful: false`  
**Expected:** stdio: exit immediately; HTTP: reset connection (RST)

### EC-TRANS-021: Connection Context in Side Effects (HTTP)

**Scenario:** HTTP server with 3 connections, side effect triggered on connection 2  
**Expected:** Side effect receives `ConnectionContext` with `connection_id: 2`. `close_connection` only closes connection 2. Other connections remain active.

### EC-TRANS-022: State Scope Per-Connection Isolation

**Scenario:** HTTP with `state_scope: per_connection`, two clients connect  
**Expected:** Each client gets independent `PhaseEngine` instance. Client A's phase transitions don't affect Client B.

---

## 4. Non-Functional Requirements

### NFR-001: Latency Overhead

- Transport layer SHALL add < 1ms latency for normal message forwarding
- Behavioral attacks intentionally add latency; this is not overhead

### NFR-002: Memory Usage

- stdio transport SHALL not buffer more than one message in memory
- HTTP transport SHALL not buffer more than `THOUGHTJACK_MAX_MESSAGE_SIZE` per connection

### NFR-003: Concurrent Connections (HTTP)

- HTTP transport SHALL support at least 100 concurrent connections
- Connection handling SHALL not block the accept loop

### NFR-004: Graceful Shutdown

- Transport SHALL complete in-flight messages on SIGTERM (up to 5s timeout)
- After timeout, force close all connections

---

## 5. Transport Configuration

### 5.1 stdio Configuration

stdio transport requires no explicit configuration. It is the default when:
- stdin is a pipe (spawned by MCP client)
- No `--http` flag specified

Optional tuning via environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `THOUGHTJACK_STDIO_BUFFER_SIZE` | Read/write buffer size | 64 KB |
| `THOUGHTJACK_MAX_MESSAGE_SIZE` | Maximum message size | 10 MB |

### 5.2 HTTP Configuration

HTTP transport is enabled via CLI flag or environment variable:

```bash
# CLI
thoughtjack server --config config.yaml --http :8080
thoughtjack server --config config.yaml --http 0.0.0.0:8080

# Environment
THOUGHTJACK_TRANSPORT=http THOUGHTJACK_HTTP_BIND=:8080 thoughtjack server --config config.yaml
```

| Variable | Description | Default |
|----------|-------------|---------|
| `THOUGHTJACK_HTTP_BIND` | Bind address | `:8080` |
| `THOUGHTJACK_HTTP_KEEPALIVE` | Keep-alive timeout | 60s |
| `THOUGHTJACK_MAX_MESSAGE_SIZE` | Maximum request body size | 10 MB |
| `THOUGHTJACK_HTTP_CONCURRENCY` | Max concurrent requests | 100 |

### 5.3 Transport Selection Logic

```
if --http flag present:
    use HTTP transport with specified bind address
else if THOUGHTJACK_TRANSPORT == "http":
    use HTTP transport with THOUGHTJACK_HTTP_BIND
else if stdin is a pipe:
    use stdio transport
else:
    error: "Cannot determine transport. Use --http or run with stdin pipe."
```

---

## 6. Implementation Notes

### 6.1 Recommended Libraries

| Library | Purpose |
|---------|---------|
| `tokio` | Async runtime, I/O |
| `tokio::io::{AsyncReadExt, AsyncWriteExt}` | Async stdin/stdout |
| `axum` | HTTP server framework |
| `hyper` | HTTP primitives |
| `tower` | Middleware (timeouts, concurrency limits) |
| `bytes` | Efficient byte buffers |

### 6.2 stdio Implementation Structure

```rust
pub struct StdioTransport {
    stdin: BufReader<tokio::io::Stdin>,
    stdout: BufWriter<tokio::io::Stdout>,
    config: StdioConfig,
}

impl StdioTransport {
    pub async fn new() -> Result<Self, TransportError> {
        Ok(Self {
            stdin: BufReader::new(tokio::io::stdin()),
            stdout: BufWriter::new(tokio::io::stdout()),
            config: StdioConfig::from_env(),
        })
    }
}
```

### 6.3 HTTP Implementation Structure

```rust
pub struct HttpTransport {
    state: Arc<ServerState>,  // Shared across connections
    config: HttpConfig,
}

struct ServerState {
    phase_engine: RwLock<PhaseEngine>,
    // ... other shared state
}

impl HttpTransport {
    pub async fn serve(self) -> Result<(), TransportError> {
        let app = Router::new()
            .route("/message", post(handle_message))
            .route("/sse", get(handle_sse))  // For server-initiated messages
            .with_state(self.state.clone());
        
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        axum::serve(listener, app).await?;
        
        Ok(())
    }
}
```

### 6.4 Behavior Dispatch Pattern

```rust
impl<T: Transport> BehaviorExecutor<T> {
    pub async fn send_with_behavior(
        &self,
        transport: &T,
        message: &JsonRpcMessage,
        behavior: &DeliveryBehavior,
    ) -> Result<(), TransportError> {
        match behavior {
            DeliveryBehavior::Normal => {
                transport.send_message(message).await
            }
            DeliveryBehavior::SlowLoris { byte_delay_ms } => {
                self.send_slow_loris(transport, message, *byte_delay_ms).await
            }
            DeliveryBehavior::UnboundedLine { target_bytes } => {
                self.send_unbounded(transport, message, *target_bytes).await
            }
            DeliveryBehavior::NestedJson { depth } => {
                let wrapped = self.wrap_nested(message, *depth);
                transport.send_message(&wrapped).await
            }
            DeliveryBehavior::ResponseDelay { delay_ms } => {
                tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                transport.send_message(message).await
            }
        }
    }
}
```

### 6.5 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Using `std::io` for stdin/stdout | Blocks the async runtime | Use `tokio::io::stdin()` / `stdout()` |
| Unbuffered stdout writes | Poor performance, syscall per byte | Use `BufWriter`, flush after message |
| Shared mutable state without sync | Race conditions with HTTP concurrency | Use `Arc<RwLock<_>>` or channels |
| Blocking in async context | Stalls entire runtime | Use `tokio::task::spawn_blocking` for CPU work |
| Ignoring backpressure | Memory exhaustion under load | Use bounded channels, check buffer capacity |
| Hardcoding buffer sizes | Inflexible for different use cases | Make configurable via env vars |
| Panicking on transport errors | Kills entire server | Return `Result`, log error, continue |
| HTTP without timeouts | Hung connections exhaust resources | Set read/write timeouts on connections |
| Flushing after every byte in slow_loris | Poor performance | Flush is needed, but batch if byte_delay is very small |

### 6.6 Testing Strategy

**Unit Tests:**
- Mock stdin/stdout with `tokio_test::io::Builder`
- Test each delivery behavior in isolation
- Test error handling for each error type

**Integration Tests:**
- Spawn actual process, communicate via pipes
- Test HTTP with `reqwest` client
- Test behavior timing within tolerance

**Behavioral Tests:**
- Verify slow_loris timing with ±10% tolerance
- Verify pipe_deadlock actually blocks
- Verify close_connection terminates connection

---

## 7. Definition of Done

- [ ] `Transport` trait defined with all required methods
- [ ] `StdioTransport` implements trait correctly
- [ ] `HttpTransport` implements trait correctly
- [ ] `slow_loris` works on both transports
- [ ] `unbounded_line` works on both transports
- [ ] `nested_json` works on both transports
- [ ] `response_delay` works on both transports
- [ ] `notification_flood` works on both transports
- [ ] `pipe_deadlock` works on stdio, skipped on HTTP with warning
- [ ] `close_connection` works on both transports (graceful and forced)
- [ ] `duplicate_request_ids` works on both transports
- [ ] Transport auto-detection works correctly
- [ ] `--http` flag enables HTTP transport
- [ ] `THOUGHTJACK_TRANSPORT` env var works
- [ ] Message size limits enforced
- [ ] All 22 edge cases (EC-TRANS-001 through EC-TRANS-022) have tests
- [ ] Latency overhead < 1ms (NFR-001)
- [ ] Memory usage within limits (NFR-002)
- [ ] HTTP supports 100 concurrent connections (NFR-003)
- [ ] Graceful shutdown works (NFR-004)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [MCP Specification: Transports](https://spec.modelcontextprotocol.io/specification/basic/transports/)
- [MCP Specification: stdio Transport](https://spec.modelcontextprotocol.io/specification/basic/transports/#stdio)
- [MCP Specification: HTTP+SSE Transport](https://spec.modelcontextprotocol.io/specification/basic/transports/#http-with-sse)
- [NDJSON Specification](http://ndjson.org/)
- [Server-Sent Events (SSE)](https://html.spec.whatwg.org/multipage/server-sent-events.html)
- [Slowloris Attack](https://en.wikipedia.org/wiki/Slowloris_(computer_security))
- [Tokio Async I/O](https://tokio.rs/tokio/tutorial/io)
- [Axum Web Framework](https://github.com/tokio-rs/axum)
- [TJ-SPEC-001: Configuration Schema](./TJ-SPEC-001_Configuration_Schema.md)

---

## Appendix A: Delivery Behavior Reference

### A.1 Normal Delivery

Standard message delivery with proper framing.

**stdio:**
```
{"jsonrpc":"2.0","result":{"content":[...]},"id":1}\n
```

**HTTP:**
```http
HTTP/1.1 200 OK
Content-Type: application/json

{"jsonrpc":"2.0","result":{"content":[...]},"id":1}
```

### A.2 Slow Loris

Byte-by-byte delivery with configurable delay.

**Configuration:**
```yaml
behavior:
  delivery: slow_loris
  byte_delay_ms: 100  # 100ms between each byte
```

**stdio timing (example 50-byte message):**
```
Byte 0: t=0ms
Byte 1: t=100ms
Byte 2: t=200ms
...
Byte 49: t=4900ms
Newline: t=5000ms
Total: ~5 seconds for 50 bytes
```

**HTTP implementation:**
```http
HTTP/1.1 200 OK
Content-Type: text/event-stream
Transfer-Encoding: chunked

1\r\n
{\r\n
[100ms delay]
1\r\n
"\r\n
[100ms delay]
...
```

### A.3 Unbounded Line

Send data without newline terminator (stdio) or without closing response (HTTP).

**Configuration:**
```yaml
behavior:
  delivery: unbounded_line
  target_bytes: 1048576  # 1MB of data without terminator
```

**stdio:**
```
{"jsonrpc":"2.0","result":{"data":"AAAA...   <- No \n, client parser waits forever
```

**HTTP:**
```http
HTTP/1.1 200 OK
Transfer-Encoding: chunked

[chunks sent indefinitely without final 0\r\n\r\n]
```

### A.4 Nested JSON

Wrap response in deeply nested JSON structure.

**Configuration:**
```yaml
behavior:
  delivery: nested_json
  depth: 1000
```

**Output structure:**
```json
{"a":{"a":{"a":{"a":{"a":{...[1000 levels]...{"jsonrpc":"2.0","result":{...}}}}}}}
```

### A.5 Response Delay

Simple delay before sending response.

**Configuration:**
```yaml
behavior:
  delivery: response_delay
  delay_ms: 5000  # 5 second delay
```

**Timing:**
```
Request received: t=0
[5000ms sleep]
Response sent: t=5000ms
```

---

## Appendix B: Side Effect Reference

### B.1 Notification Flood

Spam notifications at high rate.

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: notification_flood
      rate_per_sec: 1000
      duration_sec: 10
      trigger: on_request
```

**Output (stdio):**
```
{"jsonrpc":"2.0","method":"notifications/progress","params":{...}}\n
{"jsonrpc":"2.0","method":"notifications/progress","params":{...}}\n
{"jsonrpc":"2.0","method":"notifications/progress","params":{...}}\n
... [1000 per second for 10 seconds = 10,000 notifications]
```

### B.2 Pipe Deadlock (stdio only)

Fill stdout buffer while ignoring stdin, causing bidirectional deadlock.

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: pipe_deadlock
      trigger: on_connect
```

**Mechanism:**
1. Stop reading from stdin
2. Write continuously to stdout
3. Eventually stdout buffer fills (typically 64KB)
4. Write blocks
5. If client is also blocked waiting to write, deadlock occurs

### B.3 Close Connection

Terminate the connection.

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: close_connection
      trigger: on_request
      graceful: false  # RST vs graceful close
```

**Graceful (stdio):**
1. Close stdin
2. Flush and close stdout
3. Exit with code 0

**Forced (stdio):**
1. Exit immediately (process termination)

**Graceful (HTTP):**
1. Complete current response
2. Close connection with FIN

**Forced (HTTP):**
1. Reset connection with RST

### B.4 Duplicate Request IDs

Send multiple server-initiated requests with the same ID.

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: duplicate_request_ids
      trigger: on_request
      count: 5
      id: 1  # Force this specific ID
```

**Output:**
```
{"jsonrpc":"2.0","method":"sampling/createMessage","id":1,"params":{...}}\n
{"jsonrpc":"2.0","method":"sampling/createMessage","id":1,"params":{...}}\n
{"jsonrpc":"2.0","method":"sampling/createMessage","id":1,"params":{...}}\n
{"jsonrpc":"2.0","method":"sampling/createMessage","id":1,"params":{...}}\n
{"jsonrpc":"2.0","method":"sampling/createMessage","id":1,"params":{...}}\n
```
