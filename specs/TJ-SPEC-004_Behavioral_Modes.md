# TJ-SPEC-004: Behavioral Modes

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-004` |
| **Title** | Behavioral Modes |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **High** |
| **Version** | v1.0.0 |
| **Tags** | `#behaviors` `#slow-loris` `#dos` `#flooding` `#deadlock` `#delivery` `#side-effects` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's behavioral modes — the mechanisms that manipulate how responses are delivered and what side effects occur during server operation.

### 1.1 Motivation

Content-based attacks (prompt injection, tool shadowing) test what the server returns. Behavioral attacks test how the server behaves at the protocol and transport layers:

| Attack Category | What It Tests |
|-----------------|---------------|
| **Delivery Behaviors** | Client resilience to malformed/slow/malicious response delivery |
| **Side Effects** | Client handling of unexpected server-initiated traffic |

These attacks expose vulnerabilities that content inspection cannot detect:
- Parser stack overflow from deeply nested JSON
- Memory exhaustion from unbounded reads
- Connection pool exhaustion from slow responses
- Deadlocks from bidirectional pipe blocking
- State corruption from duplicate request IDs

### 1.2 Behavior Categories

ThoughtJack distinguishes two behavior categories:

| Category | Description | Examples |
|----------|-------------|----------|
| **Delivery Behaviors** | Modify how a response is transmitted | slow_loris, unbounded_line, nested_json, response_delay |
| **Side Effects** | Generate additional protocol traffic | notification_flood, batch_amplify, pipe_deadlock, close_connection, duplicate_request_ids |

**Key difference:** Delivery behaviors modify the response to a request. Side effects occur independently of request/response flow.

### 1.3 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Transport-aware** | Behaviors adapt to stdio vs HTTP mechanics |
| **Composable** | Multiple side effects can be active simultaneously |
| **Triggered** | Side effects fire on configurable triggers (on_connect, on_request, on_subscribe, on_unsubscribe, continuous) |
| **Scoped** | Behaviors can be set at server, phase, or tool level |
| **Measurable** | All behaviors emit metrics for test validation |

### 1.4 Scope Boundaries

**In scope:**
- Delivery behavior implementations for both transports
- Side effect implementations for both transports
- Trigger system for side effects
- Behavior scoping and override rules
- Parameter validation
- Transport compatibility detection

**Out of scope:**
- Transport mechanics (TJ-SPEC-002)
- Phase transitions (TJ-SPEC-003)
- Configuration parsing (TJ-SPEC-006)
- Payload content generation (TJ-SPEC-005)

---

## 2. Functional Requirements

### F-001: Delivery Behavior Interface

The system SHALL define a common interface for delivery behaviors.

**Acceptance Criteria:**
- All delivery behaviors implement a common trait
- Behaviors receive the message to send and transport reference
- Behaviors are responsible for complete message delivery
- Behaviors report success/failure and bytes sent

**Interface:**
```rust
#[async_trait]
pub trait DeliveryBehavior: Send + Sync {
    /// Deliver a JSON-RPC message using this behavior.
    ///
    /// The `cancel` token allows cooperative cancellation during delivery
    /// (e.g., aborting a slow loris drip on server shutdown).
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError>;

    /// Get behavior name for logging/metrics.
    fn name(&self) -> &'static str;
}

pub struct DeliveryResult {
    pub bytes_sent: usize,
    pub duration: Duration,
    pub completed: bool,  // false if interrupted
}
```

### F-002: Normal Delivery

The system SHALL support normal (baseline) message delivery.

**Acceptance Criteria:**
- Messages are serialized to JSON and sent immediately
- stdio: Message followed by newline (`\n`)
- HTTP: Standard HTTP response with `Content-Type: application/json`
- No artificial delays or modifications

**Configuration:**
```yaml
behavior:
  delivery: normal
```

**Implementation:**
```rust
pub struct NormalDelivery;

#[async_trait]
impl DeliveryBehavior for NormalDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = Instant::now();
        let bytes = transport.send_message(message).await?;

        Ok(DeliveryResult {
            bytes_sent: bytes,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn name(&self) -> &'static str {
        "normal"
    }
}
```

### F-003: Slow Loris Delivery

The system SHALL support slow loris delivery that drips bytes with delays.

**Acceptance Criteria:**
- Message is sent byte-by-byte (or chunk-by-chunk)
- Configurable delay between bytes/chunks
- stdio: Each byte written and flushed separately
- HTTP: Chunked transfer encoding with delayed chunks
- Total delivery time = message_size × byte_delay

**Warning: Intermediate Timeout Risk:**
When using `slow_loris` over HTTP, be aware that proxies, load balancers, and CDNs often have default timeouts (typically 30-120 seconds). If `message_size × byte_delay_ms` exceeds these timeouts, the intermediary will close the connection before the client receives the full response.

For example, a 1KB message with `byte_delay_ms: 100` takes ~100 seconds to deliver. This exceeds many default proxy timeouts (60s), causing connection closure at the proxy, not the target client.

**Warning: Proxy Buffering Defeats Slow Loris:**
Many reverse proxies (Nginx, AWS ALB, Cloudflare) buffer chunked responses before forwarding to the client. This means the proxy receives the slow drip, buffers it entirely, then sends it to the client in one fast burst — completely defeating the slow loris attack.

**Mitigations:**
1. **Bypass proxies**: Test against the target directly without intermediaries
2. **Disable buffering**: If using Nginx, add `proxy_buffering off;` or `X-Accel-Buffering: no` header
3. **Use stdio transport**: No intermediaries, bytes delivered directly to client process
4. **AWS ALB**: Has no disable option; cannot test slow loris through ALB
5. **Test the proxy itself**: Sometimes the goal is to test proxy timeout handling, not client handling

**Recommendation:** For testing client timeout handling (not proxy behavior), keep total delivery time under 30 seconds or use stdio transport which has no intermediaries.

**Configuration:**
```yaml
behavior:
  delivery: slow_loris
  byte_delay_ms: 100      # Delay between bytes (default: 100)
  chunk_size: 1           # Bytes per chunk (default: 1)
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `byte_delay_ms` | u64 | 100 | Milliseconds between chunks |
| `chunk_size` | usize | 1 | Bytes per chunk (1 = true byte-by-byte) |

**Implementation:**
```rust
pub struct SlowLorisDelivery {
    pub byte_delay: Duration,
    pub chunk_size: usize,
}

#[async_trait]
impl DeliveryBehavior for SlowLorisDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let mut bytes_sent = 0;

        for chunk in serialized.chunks(self.chunk_size) {
            if cancel.is_cancelled() {
                return Ok(DeliveryResult {
                    bytes_sent,
                    duration: start.elapsed(),
                    completed: false,
                });
            }
            transport.send_raw(chunk).await?;
            bytes_sent += chunk.len();
            tokio::time::sleep(self.byte_delay).await;
        }

        // Send terminator (newline for stdio)
        if transport.transport_type() == TransportType::Stdio {
            transport.send_raw(b"\n").await?;
            bytes_sent += 1;
        }

        Ok(DeliveryResult {
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn name(&self) -> &'static str {
        "slow_loris"
    }
}
```

### F-004: Unbounded Line Delivery

The system SHALL support unbounded line delivery that omits message terminators.

**Acceptance Criteria:**
- stdio: Message sent without trailing newline
- HTTP: Response sent without `Content-Length`, connection kept open
- Client parser waits indefinitely for "end of message"
- Optional `target_bytes` to send garbage padding
- Tests client timeout and resource handling

**Configuration:**
```yaml
behavior:
  delivery: unbounded_line
  target_bytes: 1048576   # Optional: pad to this size (default: 0)
  padding_char: "A"       # Character to pad with (default: "A")
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `target_bytes` | usize | 0 | Pad message to this size (0 = no padding) |
| `padding_char` | char | 'A' | Character used for padding |

**Implementation:**
```rust
pub struct UnboundedLineDelivery {
    pub target_bytes: usize,
    pub padding_char: char,
}

#[async_trait]
impl DeliveryBehavior for UnboundedLineDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let mut bytes_sent = serialized.len();

        // Send the message
        transport.send_raw(&serialized).await?;

        // Send padding if target_bytes specified
        if self.target_bytes > serialized.len() {
            let padding_needed = self.target_bytes - serialized.len();
            let padding = vec![self.padding_char as u8; padding_needed];
            transport.send_raw(&padding).await?;
            bytes_sent += padding_needed;
        }

        // Deliberately DO NOT send newline (stdio) or close response (HTTP)
        // This is the attack!

        Ok(DeliveryResult {
            bytes_sent,
            duration: start.elapsed(),
            completed: true,  // We completed our part; client is stuck
        })
    }

    fn name(&self) -> &'static str {
        "unbounded_line"
    }
}
```

### F-005: Nested JSON Delivery

The system SHALL support wrapping responses in deeply nested JSON structures.

**Acceptance Criteria:**
- Response is wrapped in N levels of JSON object nesting
- Structure: `{"a":{"a":{"a":...{ACTUAL_RESPONSE}...}}}`
- Tests parser stack depth limits
- Configurable nesting depth
- Configurable key name

**Configuration:**
```yaml
behavior:
  delivery: nested_json
  depth: 10000            # Nesting levels (default: 10000)
  key: "a"                # Key name at each level (default: "a")
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `depth` | usize | 10000 | Number of nesting levels |
| `key` | String | "a" | Key name at each level |

**Implementation:**
```rust
pub struct NestedJsonDelivery {
    pub depth: usize,
    pub key: String,
}

#[async_trait]
impl DeliveryBehavior for NestedJsonDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = Instant::now();

        // Build nested structure
        let inner = serde_json::to_value(message)?;
        let mut wrapped = inner;

        for _ in 0..self.depth {
            wrapped = serde_json::json!({ &self.key: wrapped });
        }

        let serialized = serde_json::to_vec(&wrapped)?;
        let bytes_sent = transport.send_message_raw(&serialized).await?;

        Ok(DeliveryResult {
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn name(&self) -> &'static str {
        "nested_json"
    }
}
```

### F-006: Response Delay Delivery

The system SHALL support delaying response delivery.

**Acceptance Criteria:**
- Configurable delay before sending response
- Tests client timeout handling
- Delay applies before any bytes are sent
- Normal delivery after delay completes

**Configuration:**
```yaml
behavior:
  delivery: response_delay
  delay_ms: 5000          # Delay in milliseconds (default: 5000)
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `delay_ms` | u64 | 5000 | Milliseconds to delay before responding |

**Implementation:**
```rust
pub struct ResponseDelayDelivery {
    pub delay: Duration,
}

#[async_trait]
impl DeliveryBehavior for ResponseDelayDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = Instant::now();

        // Wait before sending
        tokio::time::sleep(self.delay).await;

        // Then send normally
        let bytes_sent = transport.send_message(message).await?;

        Ok(DeliveryResult {
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn name(&self) -> &'static str {
        "response_delay"
    }
}
```

### F-007: Side Effect Interface

The system SHALL define a common interface for side effects.

**Acceptance Criteria:**
- All side effects implement a common trait
- Side effects can be triggered on various events
- Side effects run asynchronously (don't block request processing)
- Side effects can be cancelled (for shutdown)
- Side effects receive connection context for targeting (e.g., `close_connection`)

**Interface:**
```rust
#[async_trait]
pub trait SideEffect: Send + Sync {
    /// Execute the side effect
    async fn execute(
        &self,
        transport: &dyn Transport,
        connection: &ConnectionContext,  // Connection targeting for HTTP
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError>;
    
    /// Check if this side effect is supported on the given transport
    fn supports_transport(&self, transport_type: TransportType) -> bool;
    
    /// Get the trigger type for this side effect
    fn trigger(&self) -> SideEffectTrigger;
    
    /// Get side effect name for logging/metrics
    fn name(&self) -> &'static str;
}

/// Context for the current connection (see TJ-SPEC-002 F-016)
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

pub struct SideEffectResult {
    pub messages_sent: usize,
    pub bytes_sent: usize,
    pub duration: Duration,
    pub completed: bool,              // false if cancelled
    pub outcome: SideEffectOutcome,
}

/// Outcome of side effect execution.
#[derive(Debug, Clone)]
pub enum SideEffectOutcome {
    /// Side effect completed normally.
    Completed,
    /// Side effect requests connection closure.
    CloseConnection { graceful: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectTrigger {
    OnConnect,      // When client connects
    OnRequest,      // When any request is received
    OnSubscribe,    // When client subscribes to a resource
    OnUnsubscribe,  // When client unsubscribes from a resource
    Continuous,     // Runs continuously in background
}
```

**Subscription Triggers:**

The `OnSubscribe` and `OnUnsubscribe` triggers enable resource-specific attack patterns:

```yaml
# Flood notifications when client subscribes to a resource
resources:
  - resource:
      uri: "events://system/logs"
      name: "System Logs"
    behavior:
      side_effects:
        - type: notification_flood
          trigger: on_subscribe          # Fire when subscription starts
          method: notifications/resources/updated
          rate_per_sec: 100
          duration_sec: 30

# Close connection when client unsubscribes (trap)
resources:
  - resource:
      uri: "critical://health/status"
      name: "Health Status"
    behavior:
      side_effects:
        - type: close_connection
          trigger: on_unsubscribe        # Trap: can't safely unsubscribe
          graceful: false
```

**Connection Targeting (HTTP):**

For HTTP transport with multiple connections, side effects use `ConnectionContext` to target specific connections:

| Side Effect | stdio | HTTP Behavior |
|-------------|-------|---------------|
| `close_connection` | Closes server | Closes THIS connection only |
| `notification_flood` | Writes to stdout | Writes to THIS connection's stream |
| `pipe_deadlock` | Supported | Not supported (skipped) |

### F-008: Notification Flood Side Effect

The system SHALL support flooding the client with notifications.

**Acceptance Criteria:**
- Sends notifications at configurable rate
- Runs for configurable duration
- Configurable notification method and params
- Tests client notification queue handling
- Can overwhelm client processing

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: notification_flood
      trigger: on_request        # When to start flooding
      rate_per_sec: 1000         # Notifications per second
      duration_sec: 10           # How long to flood
      method: "notifications/progress"
      params:                    # Optional params for notifications
        progressToken: "flood"
        progress: 50
        total: 100
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `trigger` | enum | on_request | When to start (on_connect, on_request, on_subscribe, on_unsubscribe, continuous) |
| `rate_per_sec` | u64 | 1000 | Notifications per second (clamped to `[1, 10_000]` at runtime) |
| `duration_sec` | u64 | 10 | Duration in seconds |
| `method` | String | "notifications/message" | Notification method |
| `params` | Value | null | Notification params |

**Implementation:**
```rust
pub struct NotificationFlood {
    pub trigger: SideEffectTrigger,
    pub rate_per_sec: u64,
    pub duration: Duration,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[async_trait]
impl SideEffect for NotificationFlood {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = Instant::now();
        // Clamp rate to [1, 10_000] to prevent busy loops from tiny intervals.
        let effective_rate = self.rate_per_sec.clamp(1, 10_000);
        let interval = Duration::from_nanos(1_000_000_000 / effective_rate);
        let mut messages_sent = 0u64;
        let mut bytes_sent = 0usize;
        
        let notification = JsonRpcMessage::notification(&self.method, self.params.clone());
        
        while start.elapsed() < self.duration {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent,
                        bytes_sent,
                        duration: start.elapsed(),
                        completed: false,
                    });
                }
                _ = tokio::time::sleep(interval) => {
                    match transport.send_message(&notification).await {
                        Ok(bytes) => {
                            bytes_sent += bytes;
                            messages_sent += 1;
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "Notification flood send failed");
                            break;
                        }
                    }
                }
            }
        }
        
        Ok(SideEffectResult {
            messages_sent,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }
    
    fn supports_transport(&self, _: TransportType) -> bool {
        true
    }
    
    fn trigger(&self) -> SideEffectTrigger {
        self.trigger
    }
    
    fn name(&self) -> &'static str {
        "notification_flood"
    }
}
```

### F-009: Batch Amplify Side Effect

The system SHALL support sending large JSON-RPC batch arrays.

**Acceptance Criteria:**
- Sends array of N notifications/requests as single message
- Tests client batch handling limits
- Configurable batch size and content
- Single large message vs many small messages

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: batch_amplify
      trigger: on_request
      batch_size: 10000          # Number of messages in batch
      method: "notifications/progress"
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `trigger` | enum | on_request | When to send batch |
| `batch_size` | usize | 10000 | Number of messages in batch |
| `method` | String | "notifications/message" | Method for batch items |

**Implementation:**
```rust
pub struct BatchAmplify {
    pub trigger: SideEffectTrigger,
    pub batch_size: usize,
    pub method: String,
}

#[async_trait]
impl SideEffect for BatchAmplify {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        _cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = Instant::now();
        
        // Build batch array
        let notification = JsonRpcMessage::notification(&self.method, None);
        let batch: Vec<_> = (0..self.batch_size)
            .map(|_| notification.clone())
            .collect();
        
        let serialized = serde_json::to_vec(&batch)?;
        let bytes_sent = transport.send_raw(&serialized).await?;
        
        // Send newline for stdio
        if transport.transport_type() == TransportType::Stdio {
            transport.send_raw(b"\n").await?;
        }
        
        Ok(SideEffectResult {
            messages_sent: self.batch_size as u64,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }
    
    fn supports_transport(&self, _: TransportType) -> bool {
        true
    }
    
    fn trigger(&self) -> SideEffectTrigger {
        self.trigger
    }
    
    fn name(&self) -> &'static str {
        "batch_amplify"
    }
}
```

### F-010: Pipe Deadlock Side Effect (stdio only)

The system SHALL support inducing pipe deadlock on stdio transport.

**Acceptance Criteria:**
- Stops reading from stdin
- Writes continuously to stdout until buffer fills
- Creates bidirectional deadlock if client also blocks
- Only supported on stdio transport
- Logged warning and skipped on HTTP
- **MUST use `tokio::io::stdout()`, NOT `std::io::stdout()`** to avoid blocking the async runtime
- **MUST acquire exclusive write lock** to prevent interleaving with concurrent responses

**Write Lock Requirement:**

The pipe deadlock attack specifically tests buffer filling, not JSON stream corruption. To ensure the garbage bytes don't interleave with valid JSON responses (e.g., heartbeats or concurrent requests), the implementation MUST acquire an exclusive lock on the transport's write handle before starting the fill loop.

If intentional corruption is desired, use a separate behavioral mode.

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: pipe_deadlock
      trigger: on_request
      fill_bytes: 1048576        # Bytes to write (default: 1MB)
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `trigger` | enum | on_request | When to trigger deadlock |
| `fill_bytes` | usize | 1048576 | Bytes to write to stdout |

**Implementation:**
```rust
pub struct PipeDeadlock {
    pub trigger: SideEffectTrigger,
    pub fill_bytes: usize,
}

#[async_trait]
impl SideEffect for PipeDeadlock {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = Instant::now();

        // Write garbage via send_raw() until blocked or cancelled.
        // The transport's internal write serialization (Mutex<BufWriter>)
        // prevents interleaving with concurrent response writes.
        let chunk = vec![b'X'; 4096];
        let mut bytes_sent = 0usize;

        while bytes_sent < self.fill_bytes {
            // Pre-check: handle pre-cancelled tokens deterministically.
            // tokio::select! randomly picks which branch to poll first,
            // so a pre-cancelled token may not win the race against
            // an instantly-completing send.
            if cancel.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                result = transport.send_raw(&chunk) => {
                    match result {
                        Ok(()) => bytes_sent += chunk.len(),
                        Err(e) => {
                            tracing::info!(
                                bytes_written = bytes_sent,
                                error = %e,
                                "Pipe deadlock write stopped"
                            );
                            break;
                        }
                    }
                }
            }
        }

        Ok(SideEffectResult {
            messages_sent: 0,
            bytes_sent,
            duration: start.elapsed(),
            completed: !cancel.is_cancelled(),
            outcome: SideEffectOutcome::Completed,
        })
    }

    fn supports_transport(&self, transport_type: TransportType) -> bool {
        transport_type == TransportType::Stdio
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger
    }

    fn name(&self) -> &'static str {
        "pipe_deadlock"
    }
}
```

**Note on Write Serialization:**

The `pipe_deadlock` side effect writes via `transport.send_raw()`. The stdio transport's internal `Mutex<BufWriter>` serializes all writes, preventing interleaving with concurrent response writes. No explicit lock acquisition is needed.

This is the intended behavior for testing deadlock resilience. Implementers should:
- Ensure the cancellation token is respected (checked in the write loop)
- Log when other tasks are blocked waiting for the write lock

> ⚠️ **Terminal State Behavior**
>
> A successful `pipe_deadlock` is effectively a **terminal state** for that connection's writer task. Once the pipe fills and blocks, the write lock is held indefinitely until either:
> 1. The client reads from stdout (draining the buffer)
> 2. The connection is forcibly closed
> 3. The server is shut down
> 4. The cancellation token is triggered
>
> In `per_connection` mode, this only affects the single client. In `global` mode (if stdio were supported, which it isn't), this would kill all server I/O. Since `pipe_deadlock` is stdio-only and `state_scope: global` is HTTP-only, this cross-contamination cannot occur by design.

### F-011: Close Connection Side Effect

The system SHALL support forcibly closing the connection.

**Acceptance Criteria:**
- Terminates connection immediately or gracefully
- Graceful: Complete pending writes, then close
- Forceful: Immediate close (RST on TCP)
- Tests client reconnection handling
- Works on both transports
- Returns `SideEffectOutcome::CloseConnection { graceful: bool }` instead of directly calling `transport.close()`
- Server loop handles the actual close based on the outcome (better separation of concerns)

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: close_connection
      trigger: on_request
      graceful: false            # true = clean close, false = RST/abort
      delay_ms: 0                # Optional delay before closing
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `trigger` | enum | on_request | When to close |
| `graceful` | bool | true | Graceful vs forceful close |
| `delay_ms` | u64 | 0 | Delay before closing |

**Implementation:**
```rust
pub struct CloseConnection {
    pub trigger: SideEffectTrigger,
    pub graceful: bool,
    pub delay: Duration,
}

#[async_trait]
impl SideEffect for CloseConnection {
    async fn execute(
        &self,
        _transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = Instant::now();

        if !self.delay.is_zero() {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent: 0,
                        bytes_sent: 0,
                        duration: start.elapsed(),
                        completed: false,
                        outcome: SideEffectOutcome::Completed,
                    });
                }
                _ = tokio::time::sleep(self.delay) => {}
            }
        }

        Ok(SideEffectResult {
            messages_sent: 0,
            bytes_sent: 0,
            duration: start.elapsed(),
            completed: true,
            outcome: SideEffectOutcome::CloseConnection {
                graceful: self.graceful,
            },
        })
    }

    fn supports_transport(&self, _: TransportType) -> bool {
        true
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger
    }

    fn name(&self) -> &'static str {
        "close_connection"
    }
}
```

### F-012: Duplicate Request IDs Side Effect

The system SHALL support sending server-initiated requests with duplicate IDs.

**Acceptance Criteria:**
- Sends multiple requests with same ID
- Tests client request/response correlation
- Can use explicit ID or ID from recent client request
- Can cause response routing confusion

**Configuration:**
```yaml
behavior:
  side_effects:
    - type: duplicate_request_ids
      trigger: on_request
      count: 5                   # Number of requests to send
      id: 1                      # Explicit ID (optional)
      method: "sampling/createMessage"
      params:
        messages:
          - role: user
            content: "Summarize"
```

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `trigger` | enum | on_request | When to send |
| `count` | usize | 3 | Number of duplicate requests |
| `id` | JsonRpcId | `1` | Explicit ID (defaults to `json!(1)` when omitted) |
| `method` | String | required | Request method |
| `params` | Value | null | Request params |

**Implementation:**
```rust
pub struct DuplicateRequestIds {
    pub trigger: SideEffectTrigger,
    pub count: usize,
    pub explicit_id: Option<JsonRpcId>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[async_trait]
impl SideEffect for DuplicateRequestIds {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        _cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = Instant::now();
        
        let id = self.explicit_id.clone()
            .unwrap_or_else(|| JsonRpcId::Number(1));
        
        let request = JsonRpcMessage::request(id, &self.method, self.params.clone());
        
        let mut bytes_sent = 0usize;
        for _ in 0..self.count {
            bytes_sent += transport.send_message(&request).await?;
        }
        
        Ok(SideEffectResult {
            messages_sent: self.count as u64,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }
    
    fn supports_transport(&self, _: TransportType) -> bool {
        true
    }
    
    fn trigger(&self) -> SideEffectTrigger {
        self.trigger
    }
    
    fn name(&self) -> &'static str {
        "duplicate_request_ids"
    }
}
```

### F-013: Behavior Scoping

The system SHALL support behavior configuration at multiple scopes.

**Acceptance Criteria:**
- Server-level: Default for all phases and request types
- Phase-level: Override for specific phase
- Tool-level: Override for specific tool (applies to `tools/call` matching `params.name`)
- Resource-level: Override for specific resource (applies to `resources/read` matching `params.uri`)
- Prompt-level: Override for specific prompt (applies to `prompts/get` matching `params.name`)
- Most specific scope wins

**Scope Resolution Table:**

| Request Type | Match Field | Behavior Source Priority |
|--------------|-------------|-------------------------|
| `tools/call` | `params.name` | Tool → Phase → Server → Default |
| `resources/read` | `params.uri` | Resource → Phase → Server → Default |
| `resources/subscribe` | `params.uri` | Resource → Phase → Server → Default |
| `prompts/get` | `params.name` | Prompt → Phase → Server → Default |
| `tools/list` | — | Phase → Server → Default |
| `resources/list` | — | Phase → Server → Default |
| `prompts/list` | — | Phase → Server → Default |
| `initialize` | — | Phase → Server → Default |
| Notifications | — | Phase → Server → Default |
| Unknown methods | — | Phase → Server → Default |

**Scope Resolution for `tools/call` requests:**
```
CLI override (if --behavior flag set)
    ↓ (fallback)
Tool behavior (if params.name matches tool AND tool.behavior set)
    ↓ (fallback)
Phase behavior (if set)
    ↓ (fallback)
Server/baseline behavior (if set)
    ↓ (fallback)
Default (normal delivery, no side effects)
```

**Scope Resolution for `resources/read` and `resources/subscribe` requests:**
```
CLI override (if --behavior flag set)
    ↓ (fallback)
Resource behavior (if params.uri matches resource AND resource.behavior set)
    ↓ (fallback)
Phase behavior (if set)
    ↓ (fallback)
Server/baseline behavior (if set)
    ↓ (fallback)
Default (normal delivery, no side effects)
```

**Scope Resolution for `prompts/get` requests:**
```
CLI override (if --behavior flag set)
    ↓ (fallback)
Prompt behavior (if params.name matches prompt AND prompt.behavior set)
    ↓ (fallback)
Phase behavior (if set)
    ↓ (fallback)
Server/baseline behavior (if set)
    ↓ (fallback)
Default (normal delivery, no side effects)
```

**Scope Resolution for ALL other requests:**
```
CLI override (if --behavior flag set)
    ↓ (fallback)
Phase behavior (if set)
    ↓ (fallback)
Server/baseline behavior (if set)
    ↓ (fallback)
Default (normal delivery, no side effects)
```

**Implementation:**
```rust
fn resolve_behavior(
    &self,
    request: &JsonRpcRequest,
    phase_behavior: Option<&BehaviorConfig>,
    server_behavior: Option<&BehaviorConfig>,
) -> BehaviorConfig {
    let state = self.effective_state();
    
    // Tool-specific behavior for tools/call
    if request.method == "tools/call" {
        if let Some(tool_name) = request.params.get("name").and_then(|v| v.as_str()) {
            if let Some(tool) = state.tools.get(tool_name) {
                if let Some(ref behavior) = tool.behavior {
                    return behavior.clone();
                }
            }
        }
    }
    
    // Resource-specific behavior for resources/read and resources/subscribe
    if request.method == "resources/read" || request.method == "resources/subscribe" {
        if let Some(uri) = request.params.get("uri").and_then(|v| v.as_str()) {
            if let Some(resource) = state.resources.get(uri) {
                if let Some(ref behavior) = resource.behavior {
                    return behavior.clone();
                }
            }
        }
    }
    
    // Prompt-specific behavior for prompts/get
    if request.method == "prompts/get" {
        if let Some(name) = request.params.get("name").and_then(|v| v.as_str()) {
            if let Some(prompt) = state.prompts.get(name) {
                if let Some(ref behavior) = prompt.behavior {
                    return behavior.clone();
                }
            }
        }
    }
    
    // All other methods: phase → server → default
    phase_behavior
        .or(server_behavior)
        .cloned()
        .unwrap_or_default()
}
```

**Example:**
```yaml
baseline:
  behavior:
    delivery: normal              # Server default

phases:
  - name: slow_phase
    behavior:
      delivery: slow_loris        # Phase override
      byte_delay_ms: 50
    
  - name: mixed_phase
    # No behavior override - uses baseline (normal)
    replace_tools:
      dangerous_tool:
        tool:
          name: dangerous_tool
          # ...
        behavior:                 # Tool-specific override
          delivery: response_delay
          delay_ms: 10000
```

In this example:
- `tools/list` in `slow_phase` uses `slow_loris` (phase behavior)
- `tools/call dangerous_tool` in `mixed_phase` uses `response_delay` (tool behavior)
- `tools/list` in `mixed_phase` uses `normal` (server behavior, since phase has none)

### F-014: Side Effect Execution Coordination

The system SHALL coordinate side effect execution with request processing.

**Acceptance Criteria:**
- `on_connect` side effects start when client connects
- `on_request` side effects start when request is received
- `continuous` side effects run in background throughout
- Multiple side effects can run concurrently
- Side effects are cancelled on server shutdown

**Execution Model:**
```rust
pub struct SideEffectManager {
    transport: Arc<dyn Transport>,
    cancel: CancellationToken,
    running: Vec<JoinHandle<()>>,
}

impl SideEffectManager {
    /// Create a new manager bound to a transport and cancellation token.
    pub fn new(transport: Arc<dyn Transport>, cancel: CancellationToken) -> Self {
        Self { transport, cancel, running: Vec::new() }
    }

    /// Fire all effects matching the given trigger synchronously.
    ///
    /// Returns a list of `(name, result)` pairs for successfully completed
    /// effects. Transport-incompatible effects are skipped with a warning.
    pub async fn trigger(
        &self,
        effects: &[Box<dyn SideEffect>],
        trigger: SideEffectTrigger,
    ) -> Vec<(String, SideEffectResult)> {
        let mut results = Vec::new();
        let transport_type = self.transport.transport_type();

        for effect in effects {
            if effect.trigger() != trigger { continue; }
            if !effect.supports_transport(transport_type) {
                tracing::warn!(
                    effect = effect.name(),
                    transport = ?transport_type,
                    "side effect not supported on this transport, skipping"
                );
                continue;
            }

            let child_cancel = self.cancel.child_token();
            match effect
                .execute(
                    self.transport.as_ref(),
                    &self.transport.connection_context(),
                    child_cancel,
                )
                .await
            {
                Ok(result) => results.push((effect.name().to_string(), result)),
                Err(e) => {
                    tracing::warn!(effect = effect.name(), error = %e, "side effect failed");
                }
            }
        }
        results
    }

    /// Spawn a single owned side effect as a background task.
    ///
    /// Use this for `Continuous` effects where ownership can be transferred.
    pub fn spawn(&mut self, effect: Box<dyn SideEffect>) {
        let transport = Arc::clone(&self.transport);
        let ctx = transport.connection_context();
        let cancel = self.cancel.child_token();

        self.running.push(tokio::spawn(async move {
            let _ = effect.execute(transport.as_ref(), &ctx, cancel).await;
        }));
    }

    /// Cancel all running background effects and wait for them to finish.
    ///
    /// Each task is given a **2-second grace period** before being considered
    /// timed out. After the grace period the task is abandoned (though Tokio
    /// will still cancel it when the `JoinHandle` is dropped).
    pub async fn shutdown(&mut self) {
        self.cancel.cancel();
        for handle in self.running.drain(..) {
            match tokio::time::timeout(Duration::from_secs(2), handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) if e.is_cancelled() => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "background side effect panicked"),
                Err(_) => tracing::warn!("background side effect did not finish within 2s"),
            }
        }
    }

    /// Returns the number of currently running background tasks.
    pub fn running_count(&self) -> usize {
        self.running.len()
    }
}
```

### F-015: Behavior Metrics

The system SHALL emit metrics for all behavioral operations.

**Acceptance Criteria:**
- Delivery duration histogram per behavior type
- Bytes sent counter per behavior type
- Side effect execution counter per type
- Side effect duration histogram per type
- Transport-specific breakdowns

**Metrics:**
```rust
// Delivery metrics
counter!("thoughtjack_delivery_total", "behavior" => behavior_name);
histogram!("thoughtjack_delivery_duration_ms", "behavior" => behavior_name);
counter!("thoughtjack_delivery_bytes", "behavior" => behavior_name);

// Side effect metrics
counter!("thoughtjack_side_effect_executions", "effect" => effect_name);
histogram!("thoughtjack_side_effect_duration_ms", "effect" => effect_name);
counter!("thoughtjack_side_effect_messages_sent", "effect" => effect_name);
counter!("thoughtjack_side_effect_bytes_sent", "effect" => effect_name);
gauge!("thoughtjack_side_effect_active", "effect" => effect_name);
```

---

## 3. Edge Cases

### EC-BEH-001: Slow Loris With Zero Delay

**Scenario:** `slow_loris` with `byte_delay_ms: 0`  
**Expected:** Effectively normal delivery (no sleeps between bytes)

### EC-BEH-002: Slow Loris Interrupted by Shutdown

**Scenario:** Server shutdown during slow_loris delivery  
**Expected:** Stop sending, report partial delivery, exit cleanly

### EC-BEH-003: Nested JSON Exceeds Memory Limit

**Scenario:** `nested_json` with `depth: 1000000` on 1KB message  
**Expected:** Check against `THOUGHTJACK_MAX_PAYLOAD_BYTES`, reject if exceeded

### EC-BEH-004: Unbounded Line Client Timeout

**Scenario:** Client has read timeout, unbounded_line exceeds it  
**Expected:** Client disconnects, server logs info, continues (for HTTP) or exits (for stdio)

### EC-BEH-005: Notification Flood Rate Exceeds Transport Capacity

**Scenario:** `rate_per_sec: 1000000` but transport can only handle 10000/sec  
**Expected:** Actual rate limited by transport; no crash, backpressure handled

### EC-BEH-006: Pipe Deadlock on HTTP Transport

**Scenario:** Config specifies `pipe_deadlock` on HTTP server  
**Expected:** Warning logged, side effect skipped, server continues

### EC-BEH-007: Close Connection During Flood

**Scenario:** `notification_flood` and `close_connection` both triggered on same event  
**Expected:** Both execute; close_connection terminates flood early

### EC-BEH-008: Multiple Continuous Side Effects

**Scenario:** Two `continuous` side effects configured  
**Expected:** Both run concurrently in separate tasks

### EC-BEH-009: Side Effect Fails Immediately

**Scenario:** `notification_flood` fails on first send (transport error)  
**Expected:** Error logged, side effect stops, server continues

### EC-BEH-010: Graceful Close With Pending Slow Loris

**Scenario:** `close_connection(graceful=true)` while slow_loris is mid-delivery  
**Expected:** Slow loris completes (or times out), then connection closes

### EC-BEH-011: Duplicate Request IDs With No Explicit ID

**Scenario:** `duplicate_request_ids` without explicit `id` parameter
**Expected:** Defaults to `json!(1)` (integer 1) since null is not a valid JSON-RPC ID

### EC-BEH-012: Batch Amplify With batch_size: 0

**Scenario:** `batch_amplify` with `batch_size: 0`  
**Expected:** Validation error: "batch_size must be positive"

### EC-BEH-013: Response Delay Exceeds Client Timeout

**Scenario:** `response_delay: 60s` but client has 30s timeout  
**Expected:** Client times out; server eventually sends response to closed connection

### EC-BEH-014: Nested JSON With Empty Message

**Scenario:** `nested_json` on empty/null response  
**Expected:** Wraps null/empty value in nesting correctly

### EC-BEH-015: Tool-Level Behavior on Non-Tool Request

**Scenario:** Tool has specific behavior, but client calls `tools/list`  
**Expected:** Use phase/server behavior, not tool behavior (tool behavior only applies to that tool's responses)

### EC-BEH-016: Behavior Override to Normal

**Scenario:** Server has `slow_loris`, phase overrides with `normal`  
**Expected:** Phase uses normal delivery (explicit override)

### EC-BEH-017: Side Effect Params Validation

**Scenario:** `notification_flood` with invalid `method` (empty string)  
**Expected:** Validation error at config load time

### EC-BEH-018: Concurrent on_request Side Effects

**Scenario:** 10 requests arrive rapidly, each triggers `on_request` side effect  
**Expected:** 10 side effect instances run concurrently

### EC-BEH-019: Slow Loris Chunk Size Larger Than Message

**Scenario:** `chunk_size: 10000` on 100-byte message  
**Expected:** Single chunk containing entire message (effectively normal delivery)

### EC-BEH-020: Close Connection on_connect

**Scenario:** `close_connection` with `trigger: on_connect`  
**Expected:** Connection closed immediately after handshake, before any requests processed

### EC-BEH-021: Close Connection Targeting (HTTP)

**Scenario:** HTTP server with 5 concurrent connections, `close_connection` triggers on connection 3  
**Expected:** Only connection 3 is closed; connections 1, 2, 4, 5 remain active. Side effect uses `ConnectionContext` to target specific connection.

### EC-BEH-022: Tool-Level Behavior on Non-Tool Request

**Scenario:** Tool has specific behavior configured, but client calls `tools/list`  
**Expected:** Use phase/server behavior, NOT tool behavior. Tool behavior only applies to `tools/call` requests where `params.name` matches the tool name.

---

## 4. Non-Functional Requirements

### NFR-001: Behavior Instantiation

- Behavior objects SHALL be instantiated once at config load
- Behaviors SHALL be stateless (all state in parameters)
- Behavior cloning SHALL be O(1) (cheap)

### NFR-002: Side Effect Resource Usage

- Side effects SHALL not allocate unbounded memory
- Notification flood buffer: max 1000 pending notifications
- Batch amplify: stream generation, don't buffer entire batch

### NFR-003: Cancellation Responsiveness

- Side effects SHALL respond to cancellation within 100ms
- Use `tokio::select!` for cancellation points
- No blocking operations without timeout

### NFR-004: Metric Overhead

- Metric recording SHALL add < 1µs per operation
- Use thread-local counters with periodic aggregation

---

## 5. Behavior Configuration Reference

### 5.1 Delivery Behaviors

```yaml
behavior:
  # Normal delivery (default)
  delivery: normal
  
  # Slow loris - byte-by-byte with delays
  delivery: slow_loris
  byte_delay_ms: 100        # ms between bytes (default: 100)
  chunk_size: 1             # bytes per chunk (default: 1)
  
  # Unbounded line - no message terminator
  delivery: unbounded_line
  target_bytes: 1048576     # pad to this size (default: 0)
  padding_char: "A"         # padding character (default: "A")
  
  # Nested JSON - deep nesting wrapper
  delivery: nested_json
  depth: 10000              # nesting levels (default: 10000)
  key: "a"                  # key at each level (default: "a")
  
  # Response delay - delay before sending
  delivery: response_delay
  delay_ms: 5000            # delay in ms (default: 5000)
```

### 5.2 Side Effects

```yaml
behavior:
  side_effects:
    # Notification flood
    - type: notification_flood
      trigger: on_request | on_connect | continuous
      rate_per_sec: 1000
      duration_sec: 10
      method: "notifications/progress"
      params: { ... }
    
    # Batch amplify
    - type: batch_amplify
      trigger: on_request | on_connect
      batch_size: 10000
      method: "notifications/message"
    
    # Pipe deadlock (stdio only)
    - type: pipe_deadlock
      trigger: on_request | on_connect
      fill_bytes: 1048576
    
    # Close connection
    - type: close_connection
      trigger: on_request | on_connect
      graceful: true | false
      delay_ms: 0
    
    # Duplicate request IDs
    - type: duplicate_request_ids
      trigger: on_request | on_connect
      count: 5
      id: 1                 # or "string-id", or omit for default
      method: "sampling/createMessage"
      params: { ... }
```

### 5.3 Triggers

| Trigger | Fires When | Use Case |
|---------|------------|----------|
| `on_connect` | Client establishes connection | Pre-emptive attacks |
| `on_request` | Any request is received | Reactive attacks |
| `continuous` | Runs in background until shutdown | Persistent pressure |

---

## 6. Implementation Notes

### 6.1 Recommended Libraries

| Library | Purpose |
|---------|---------|
| `tokio` | Async runtime, timers, channels |
| `tokio_util::sync::CancellationToken` | Cooperative cancellation |
| `metrics` | Prometheus-compatible metrics |
| `async_trait` | Async trait support |

### 6.2 Behavior Factory

```rust
pub fn create_delivery_behavior(config: &BehaviorConfig) -> Box<dyn DeliveryBehavior> {
    match &config.delivery {
        DeliveryType::Normal => Box::new(NormalDelivery),
        DeliveryType::SlowLoris { byte_delay_ms, chunk_size } => {
            Box::new(SlowLorisDelivery {
                byte_delay: Duration::from_millis(*byte_delay_ms),
                chunk_size: *chunk_size,
            })
        }
        DeliveryType::UnboundedLine { target_bytes, padding_char } => {
            Box::new(UnboundedLineDelivery {
                target_bytes: *target_bytes,
                padding_char: *padding_char,
            })
        }
        DeliveryType::NestedJson { depth, key } => {
            Box::new(NestedJsonDelivery {
                depth: *depth,
                key: key.clone(),
            })
        }
        DeliveryType::ResponseDelay { delay_ms } => {
            Box::new(ResponseDelayDelivery {
                delay: Duration::from_millis(*delay_ms),
            })
        }
    }
}
```

### 6.3 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Blocking in async context | Stalls runtime | Use async I/O throughout |
| Using `std::io::stdout()` | Blocks thread, stalls runtime | Use `tokio::io::stdout()` |
| Unbounded notification queue | Memory exhaustion | Bounded channel, drop on full |
| Ignoring cancellation token | Slow shutdown | Check cancellation in loops |
| Allocating nested JSON recursively | Stack overflow | Iterative construction |
| Shared mutable state in behaviors | Race conditions | Behaviors are stateless |
| Panicking on transport error | Server crash | Return error, log, continue |
| Busy-waiting for delays | CPU waste | Use `tokio::time::sleep` |
| Large chunk sizes in slow_loris | Defeats purpose | Default to 1 byte |

### 6.4 Testing Strategy

**Unit Tests:**
- Each behavior in isolation with mock transport
- Parameter validation
- Edge cases (zero values, max values)
- Cancellation handling

**Integration Tests:**
- Behavior + real transport (stdio via pipes)
- Timing verification (slow_loris delays)
- Side effect coordination
- Scoping resolution

**Benchmark Tests:**
- Notification flood throughput
- Slow loris memory usage
- Batch amplify serialization time

---

## 6.5 Future: HTTP-Specific Attacks

The following HTTP-layer attacks are candidates for future implementation. Unlike current behaviors that operate at the JSON-RPC/MCP level, these target the HTTP transport framing itself:

| Behavior | Description | Attack Vector |
|----------|-------------|---------------|
| `http_header_slowloris` | Send HTTP response headers byte-by-byte | Tests proxy/server header timeout handling |
| `incomplete_body` | Send `Content-Length: N` but only `N-1` bytes | Tests client read timeout and buffer handling |
| `chunked_hang` | Start chunked encoding, never send final `0\r\n` | Tests chunked transfer timeout handling |
| `h2_settings_flood` | Flood HTTP/2 SETTINGS frames | Tests HTTP/2 frame handling limits |
| `h2_rst_stream` | Send RST_STREAM immediately after response | Tests client stream reset handling |

**Note:** These require HTTP transport (TJ-SPEC-002) and are not applicable to stdio.

**Status:** Deferred to post-v0.2. Current `slow_loris` behavior operates at JSON-RPC body level, which is sufficient for most MCP client testing.

---

## 7. Definition of Done

- [ ] `DeliveryBehavior` trait implemented
- [ ] `NormalDelivery` implemented
- [ ] `SlowLorisDelivery` implemented with configurable delay/chunk
- [ ] `UnboundedLineDelivery` implemented with padding support
- [ ] `NestedJsonDelivery` implemented with configurable depth
- [ ] `ResponseDelayDelivery` implemented
- [ ] `SideEffect` trait implemented
- [ ] `NotificationFlood` implemented with rate limiting
- [ ] `BatchAmplify` implemented
- [ ] `PipeDeadlock` implemented (stdio only)
- [ ] `CloseConnection` implemented (graceful and forced)
- [ ] `DuplicateRequestIds` implemented
- [ ] Behavior scoping (server → phase → tool) works
- [ ] Side effect triggers (on_connect, on_request, on_subscribe, on_unsubscribe, continuous) work
- [ ] Transport compatibility checking works
- [ ] Unsupported behaviors log warning and skip
- [ ] Cancellation works for all side effects
- [ ] Metrics emitted for all behaviors
- [ ] All 22 edge cases (EC-BEH-001 through EC-BEH-022) have tests
- [ ] Behavior instantiation is O(1) (NFR-001)
- [ ] Side effects don't allocate unbounded memory (NFR-002)
- [ ] Cancellation responds within 100ms (NFR-003)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [TJ-SPEC-001: Configuration Schema](./TJ-SPEC-001_Configuration_Schema.md)
- [TJ-SPEC-002: Transport Abstraction](./TJ-SPEC-002_Transport_Abstraction.md)
- [TJ-SPEC-003: Phase Engine](./TJ-SPEC-003_Phase_Engine.md)
- [Slowloris Attack](https://en.wikipedia.org/wiki/Slowloris_(computer_security))
- [JSON-RPC 2.0 Batch](https://www.jsonrpc.org/specification#batch)
- [Tokio Cancellation](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)

---

## Appendix A: Attack Coverage Matrix

| Attack ID | Attack Name | Behavior Type | Configuration |
|-----------|-------------|---------------|---------------|
| TAM-001 | Deeply Nested JSON DoS | `nested_json` | `depth: 100000` |
| TAM-002 | Batch Amplification | `batch_amplify` | `batch_size: 100000` |
| TAM-003 | Unbounded Line DoS | `unbounded_line` | `target_bytes: 10485760` |
| TAM-004 | Slow Loris | `slow_loris` | `byte_delay_ms: 1000` |
| TAM-005 | Pipe Deadlock | `pipe_deadlock` | `fill_bytes: 1048576` |
| TAM-006 | Notification Flood | `notification_flood` | `rate_per_sec: 10000` |
| TAM-007 | ID Collision | `duplicate_request_ids` | `count: 10, id: 1` |

---

## Appendix B: Behavior Interaction Matrix

| Delivery + Side Effect | Compatible? | Notes |
|------------------------|-------------|-------|
| slow_loris + notification_flood | ⚠️ | Flood may complete before delivery |
| slow_loris + close_connection | ⚠️ | Close may interrupt delivery |
| unbounded_line + any | ✅ | Side effects run independently |
| nested_json + any | ✅ | No timing conflicts |
| response_delay + notification_flood | ✅ | Flood runs during delay |
| any + pipe_deadlock | ⚠️ | Deadlock stops all I/O |

---

## Appendix C: Transport Compatibility

| Behavior | stdio | HTTP | Notes |
|----------|-------|------|-------|
| normal | ✅ | ✅ | |
| slow_loris | ✅ | ✅ | HTTP uses chunked encoding |
| unbounded_line | ✅ | ✅ | HTTP keeps connection open |
| nested_json | ✅ | ✅ | |
| response_delay | ✅ | ✅ | |
| notification_flood | ✅ | ✅ | |
| batch_amplify | ✅ | ✅ | |
| pipe_deadlock | ✅ | ❌ | No pipes in HTTP |
| close_connection | ✅ | ✅ | |
| duplicate_request_ids | ✅ | ✅ | |
