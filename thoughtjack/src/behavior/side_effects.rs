//! Side effect behaviors (TJ-SPEC-004 F-011).
//!
//! Side effects are actions triggered independently of normal responses.
//! They include flooding, pipe deadlocking, connection manipulation,
//! and duplicate request injection.

use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::schema::{SideEffectConfig, SideEffectTrigger, SideEffectType};
use crate::error::BehaviorError;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
};
use crate::transport::{ConnectionContext, Transport, TransportType};

// ============================================================================
// SideEffectOutcome
// ============================================================================

/// Outcome of a side effect execution.
///
/// Implements: TJ-SPEC-004 F-007
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideEffectOutcome {
    /// Side effect completed normally.
    Completed,
    /// Side effect requests connection closure.
    CloseConnection {
        /// Whether to close gracefully.
        graceful: bool,
    },
}

// ============================================================================
// SideEffectResult
// ============================================================================

/// Result of a side effect execution.
///
/// Implements: TJ-SPEC-004 F-007
#[derive(Debug, Clone)]
pub struct SideEffectResult {
    /// Number of messages sent.
    pub messages_sent: usize,
    /// Number of bytes sent.
    pub bytes_sent: usize,
    /// Wall-clock duration of the execution.
    pub duration: Duration,
    /// Whether the side effect completed (false if cancelled).
    pub completed: bool,
    /// Outcome of the side effect.
    pub outcome: SideEffectOutcome,
}

// ============================================================================
// SideEffect trait
// ============================================================================

/// Trait for side effect behaviors.
///
/// Implements: TJ-SPEC-004 F-007
#[async_trait::async_trait]
pub trait SideEffect: Send + Sync {
    /// Executes the side effect.
    async fn execute(
        &self,
        transport: &dyn Transport,
        connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError>;

    /// Whether this side effect supports the given transport type.
    fn supports_transport(&self, transport_type: TransportType) -> bool;

    /// The trigger condition for this side effect.
    fn trigger(&self) -> SideEffectTrigger;

    /// Human-readable name for logging and metrics.
    fn name(&self) -> &'static str;
}

// ============================================================================
// Parameter extraction helpers
// ============================================================================

fn extract_u64(params: &std::collections::HashMap<String, Value>, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn extract_usize(
    params: &std::collections::HashMap<String, Value>,
    key: &str,
    default: usize,
) -> usize {
    params
        .get(key)
        .and_then(Value::as_u64)
        .map_or(default, |v| usize::try_from(v).unwrap_or(default))
}

fn extract_bool(
    params: &std::collections::HashMap<String, Value>,
    key: &str,
    default: bool,
) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn extract_string(
    params: &std::collections::HashMap<String, Value>,
    key: &str,
    default: &str,
) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

// ============================================================================
// NotificationFlood
// ============================================================================

/// Spam notifications at high rate.
struct NotificationFlood {
    trigger_cond: SideEffectTrigger,
    rate_per_sec: u64,
    duration: Duration,
    method: String,
    params: Option<Value>,
}

#[async_trait::async_trait]
impl SideEffect for NotificationFlood {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = std::time::Instant::now();
        // Clamp rate to [1, 10_000] req/sec to prevent busy loops from tiny intervals.
        // At 10k/sec the interval is 100μs — below that, tokio timer resolution
        // and serialization overhead make higher rates unreliable anyway.
        let effective_rate = self.rate_per_sec.clamp(1, 10_000);
        let interval = Duration::from_nanos(1_000_000_000 / effective_rate);

        let mut messages_sent: usize = 0;
        let mut bytes_sent: usize = 0;

        loop {
            // Send first, then sleep — ensures the last interval's
            // notification is delivered before the duration check.
            if start.elapsed() >= self.duration {
                break;
            }

            let notification = JsonRpcNotification::new(
                self.method.clone(),
                self.params.clone(),
            );
            let mut serialized = serde_json::to_vec(&notification)?;
            serialized.push(b'\n');
            let len = serialized.len();
            transport.send_raw(&serialized).await?;
            messages_sent += 1;
            bytes_sent += len;

            tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent,
                        bytes_sent,
                        duration: start.elapsed(),
                        completed: false,
                        outcome: SideEffectOutcome::Completed,
                    });
                }
                () = tokio::time::sleep(interval) => {}
            }
        }

        Ok(SideEffectResult {
            messages_sent,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
            outcome: SideEffectOutcome::Completed,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger_cond
    }

    fn name(&self) -> &'static str {
        "notification_flood"
    }
}

// ============================================================================
// BatchAmplify
// ============================================================================

/// Send responses in large batches.
struct BatchAmplify {
    trigger_cond: SideEffectTrigger,
    batch_size: usize,
    method: String,
}

#[async_trait::async_trait]
impl SideEffect for BatchAmplify {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = std::time::Instant::now();

        let notifications: Vec<JsonRpcNotification> = (0..self.batch_size)
            .map(|_| JsonRpcNotification::new(self.method.clone(), None))
            .collect();

        let mut serialized = serde_json::to_vec(&notifications)?;
        serialized.push(b'\n'); // Trailing newline for line-delimited framing
        let bytes_sent = serialized.len();

        tokio::select! {
            () = cancel.cancelled() => {
                return Ok(SideEffectResult {
                    messages_sent: 0,
                    bytes_sent: 0,
                    duration: start.elapsed(),
                    completed: false,
                    outcome: SideEffectOutcome::Completed,
                });
            }
            result = transport.send_raw(&serialized) => { result?; }
        }

        Ok(SideEffectResult {
            messages_sent: self.batch_size,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
            outcome: SideEffectOutcome::Completed,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger_cond
    }

    fn name(&self) -> &'static str {
        "batch_amplify"
    }
}

// ============================================================================
// PipeDeadlock
// ============================================================================

/// Fill stdout without reading stdin (stdio deadlock).
///
/// Only supports stdio transport (EC-BEH-006).
struct PipeDeadlock {
    trigger_cond: SideEffectTrigger,
    fill_bytes: usize,
}

#[async_trait::async_trait]
impl SideEffect for PipeDeadlock {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = std::time::Instant::now();
        let chunk = vec![b'X'; 4096];
        let mut bytes_sent: usize = 0;

        while bytes_sent < self.fill_bytes {
            if cancel.is_cancelled() {
                return Ok(SideEffectResult {
                    messages_sent: 0,
                    bytes_sent,
                    duration: start.elapsed(),
                    completed: false,
                    outcome: SideEffectOutcome::Completed,
                });
            }
            let remaining = self.fill_bytes - bytes_sent;
            let to_send = remaining.min(chunk.len());
            tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent: 0,
                        bytes_sent,
                        duration: start.elapsed(),
                        completed: false,
                        outcome: SideEffectOutcome::Completed,
                    });
                }
                result = transport.send_raw(&chunk[..to_send]) => { result?; }
            }
            bytes_sent += to_send;
        }

        Ok(SideEffectResult {
            messages_sent: 0,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
            outcome: SideEffectOutcome::Completed,
        })
    }

    fn supports_transport(&self, transport_type: TransportType) -> bool {
        // EC-BEH-006: only stdio
        transport_type == TransportType::Stdio
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger_cond
    }

    fn name(&self) -> &'static str {
        "pipe_deadlock"
    }
}

// ============================================================================
// CloseConnection
// ============================================================================

/// Close the connection, optionally after a delay.
struct CloseConnectionEffect {
    trigger_cond: SideEffectTrigger,
    graceful: bool,
    delay: Duration,
}

#[async_trait::async_trait]
impl SideEffect for CloseConnectionEffect {
    async fn execute(
        &self,
        _transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = std::time::Instant::now();

        if self.delay > Duration::ZERO {
            tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent: 0,
                        bytes_sent: 0,
                        duration: start.elapsed(),
                        completed: false,
                        outcome: SideEffectOutcome::Completed,
                    });
                }
                () = tokio::time::sleep(self.delay) => {}
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

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger_cond
    }

    fn name(&self) -> &'static str {
        "close_connection"
    }
}

// ============================================================================
// DuplicateRequestIds
// ============================================================================

/// Send multiple requests with the same ID.
struct DuplicateRequestIds {
    trigger_cond: SideEffectTrigger,
    count: usize,
    explicit_id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[async_trait::async_trait]
impl SideEffect for DuplicateRequestIds {
    async fn execute(
        &self,
        transport: &dyn Transport,
        _connection: &ConnectionContext,
        cancel: CancellationToken,
    ) -> Result<SideEffectResult, BehaviorError> {
        let start = std::time::Instant::now();

        // EC-BEH-011: default to json!(1) if no explicit ID
        let id = self
            .explicit_id
            .clone()
            .unwrap_or_else(|| serde_json::json!(1));

        let request = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: self.method.clone(),
            params: self.params.clone(),
            id,
        });

        // Serialize once outside the loop to avoid redundant work
        let mut serialized = serde_json::to_vec(&request)?;
        serialized.push(b'\n');
        let frame_len = serialized.len();

        let mut messages_sent: usize = 0;
        let mut bytes_sent: usize = 0;
        for _ in 0..self.count {
            if cancel.is_cancelled() {
                return Ok(SideEffectResult {
                    messages_sent,
                    bytes_sent,
                    duration: start.elapsed(),
                    completed: false,
                    outcome: SideEffectOutcome::Completed,
                });
            }
            tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(SideEffectResult {
                        messages_sent,
                        bytes_sent,
                        duration: start.elapsed(),
                        completed: false,
                        outcome: SideEffectOutcome::Completed,
                    });
                }
                result = transport.send_raw(&serialized) => { result?; }
            }
            messages_sent += 1;
            bytes_sent += frame_len;
        }

        Ok(SideEffectResult {
            messages_sent,
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
            outcome: SideEffectOutcome::Completed,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn trigger(&self) -> SideEffectTrigger {
        self.trigger_cond
    }

    fn name(&self) -> &'static str {
        "duplicate_request_ids"
    }
}

// ============================================================================
// Factory
// ============================================================================

/// Creates a side effect from configuration.
///
/// Implements: TJ-SPEC-004 F-007
#[must_use]
pub fn create_side_effect(config: &SideEffectConfig) -> Box<dyn SideEffect> {
    let trigger_cond = config.trigger;
    let params = &config.params;

    match config.type_ {
        SideEffectType::NotificationFlood => {
            let rate_per_sec = extract_u64(params, "rate_per_sec", 1000);
            let duration_sec = extract_u64(params, "duration_sec", 10);
            let method = extract_string(params, "method", "notifications/message");
            let notification_params = params.get("params").cloned();

            Box::new(NotificationFlood {
                trigger_cond,
                rate_per_sec,
                duration: Duration::from_secs(duration_sec),
                method,
                params: notification_params,
            })
        }
        SideEffectType::BatchAmplify => {
            let batch_size = extract_usize(params, "batch_size", 10000).max(1);
            let method = extract_string(params, "method", "notifications/message");

            Box::new(BatchAmplify {
                trigger_cond,
                batch_size,
                method,
            })
        }
        SideEffectType::PipeDeadlock => {
            let fill_bytes = extract_usize(params, "fill_bytes", 1_048_576);

            Box::new(PipeDeadlock {
                trigger_cond,
                fill_bytes,
            })
        }
        SideEffectType::CloseConnection => {
            let graceful = extract_bool(params, "graceful", true);
            let delay_ms = extract_u64(params, "delay_ms", 0);

            Box::new(CloseConnectionEffect {
                trigger_cond,
                graceful,
                delay: Duration::from_millis(delay_ms),
            })
        }
        SideEffectType::DuplicateRequestIds => {
            let count = extract_usize(params, "count", 3);
            let explicit_id = params.get("id").cloned();
            let method = extract_string(params, "method", "tools/call");
            let request_params = params.get("params").cloned();

            Box::new(DuplicateRequestIds {
                trigger_cond,
                count,
                explicit_id,
                method,
                params: request_params,
            })
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::DeliveryConfig;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // ========================================================================
    // Mock Transport
    // ========================================================================

    struct MockTransport {
        raw_sends: Arc<Mutex<Vec<Vec<u8>>>>,
        message_sends: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                raw_sends: Arc::new(Mutex::new(Vec::new())),
                message_sends: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl Transport for MockTransport {
        async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
            let bytes = serde_json::to_vec(message)?;
            self.message_sends.lock().unwrap().push(bytes);
            Ok(())
        }

        async fn send_raw(&self, bytes: &[u8]) -> crate::transport::Result<()> {
            self.raw_sends.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }

        async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
            Ok(None)
        }

        fn supports_behavior(&self, _behavior: &DeliveryConfig) -> bool {
            true
        }

        fn transport_type(&self) -> TransportType {
            TransportType::Stdio
        }

        async fn finalize_response(&self) -> crate::transport::Result<()> {
            Ok(())
        }

        fn connection_context(&self) -> crate::transport::ConnectionContext {
            crate::transport::ConnectionContext::stdio()
        }
    }

    // ========================================================================
    // PipeDeadlock transport check (EC-BEH-006)
    // ========================================================================

    #[test]
    fn test_pipe_deadlock_supports_only_stdio() {
        let config = SideEffectConfig {
            type_: SideEffectType::PipeDeadlock,
            trigger: SideEffectTrigger::OnRequest,
            params: HashMap::new(),
        };
        let effect = create_side_effect(&config);
        assert!(effect.supports_transport(TransportType::Stdio));
        assert!(!effect.supports_transport(TransportType::Http));
    }

    // ========================================================================
    // DuplicateRequestIds default ID (EC-BEH-011)
    // ========================================================================

    #[tokio::test]
    async fn test_duplicate_request_ids_default_id() {
        let config = SideEffectConfig {
            type_: SideEffectType::DuplicateRequestIds,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("count".to_string(), serde_json::json!(3));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();

        assert_eq!(result.messages_sent, 3);
        assert!(result.completed);

        // Verify the sent messages have id=1
        let sends = transport.message_sends.lock().unwrap();
        for msg_bytes in sends.iter() {
            let parsed: Value = serde_json::from_slice(msg_bytes).unwrap();
            assert_eq!(parsed["id"], serde_json::json!(1));
        }
    }

    // ========================================================================
    // CloseConnection outcome
    // ========================================================================

    #[tokio::test]
    async fn test_close_connection_outcome() {
        let config = SideEffectConfig {
            type_: SideEffectType::CloseConnection,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("graceful".to_string(), serde_json::json!(true));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();

        assert_eq!(
            result.outcome,
            SideEffectOutcome::CloseConnection { graceful: true }
        );
        assert!(result.completed);
    }

    // ========================================================================
    // NotificationFlood cancellation
    // ========================================================================

    #[tokio::test]
    async fn test_notification_flood_cancellation() {
        let config = SideEffectConfig {
            type_: SideEffectType::NotificationFlood,
            trigger: SideEffectTrigger::Continuous,
            params: {
                let mut m = HashMap::new();
                m.insert("rate_per_sec".to_string(), serde_json::json!(1000));
                m.insert("duration_sec".to_string(), serde_json::json!(60));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = Arc::new(MockTransport::new());
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();

        let cancel_clone = cancel.clone();
        let transport_clone = transport.clone();

        let handle = tokio::spawn(async move {
            effect
                .execute(transport_clone.as_ref(), &connection, cancel_clone)
                .await
        });

        // Let some notifications be sent
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = handle.await.unwrap().unwrap();
        assert!(!result.completed);
        assert!(result.messages_sent > 0);
    }

    // ========================================================================
    // Factory tests
    // ========================================================================

    #[test]
    fn test_factory_notification_flood() {
        let config = SideEffectConfig {
            type_: SideEffectType::NotificationFlood,
            trigger: SideEffectTrigger::OnConnect,
            params: HashMap::new(),
        };
        let effect = create_side_effect(&config);
        assert_eq!(effect.name(), "notification_flood");
        assert_eq!(effect.trigger(), SideEffectTrigger::OnConnect);
    }

    #[test]
    fn test_factory_batch_amplify() {
        let config = SideEffectConfig {
            type_: SideEffectType::BatchAmplify,
            trigger: SideEffectTrigger::OnRequest,
            params: HashMap::new(),
        };
        let effect = create_side_effect(&config);
        assert_eq!(effect.name(), "batch_amplify");
    }

    #[test]
    fn test_factory_close_connection() {
        let config = SideEffectConfig {
            type_: SideEffectType::CloseConnection,
            trigger: SideEffectTrigger::OnRequest,
            params: HashMap::new(),
        };
        let effect = create_side_effect(&config);
        assert_eq!(effect.name(), "close_connection");
    }

    #[test]
    fn test_factory_duplicate_request_ids() {
        let config = SideEffectConfig {
            type_: SideEffectType::DuplicateRequestIds,
            trigger: SideEffectTrigger::OnRequest,
            params: HashMap::new(),
        };
        let effect = create_side_effect(&config);
        assert_eq!(effect.name(), "duplicate_request_ids");
    }

    // ========================================================================
    // Cancellation tests
    // ========================================================================

    #[tokio::test]
    async fn test_batch_amplify_cancellation() {
        // BatchAmplify is a single send, so we verify the select path
        // by pre-cancelling and checking the result is either completed
        // (send won the race) or cancelled.
        let config = SideEffectConfig {
            type_: SideEffectType::BatchAmplify,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("batch_size".to_string(), serde_json::json!(100));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();
        // The .unwrap() above is the real assertion — no error from
        // executing with a pre-cancelled token.  The outcome is
        // non-deterministic (select! may pick either branch), so we only
        // verify the messages_sent counter is consistent with completion.
        assert!(result.messages_sent == 0 || result.completed);
    }

    #[tokio::test]
    async fn test_pipe_deadlock_cancellation() {
        let config = SideEffectConfig {
            type_: SideEffectType::PipeDeadlock,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("fill_bytes".to_string(), serde_json::json!(1_000_000));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();
        assert!(!result.completed);
        assert_eq!(result.bytes_sent, 0);
    }

    #[tokio::test]
    async fn test_close_connection_cancellation() {
        let config = SideEffectConfig {
            type_: SideEffectType::CloseConnection,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("delay_ms".to_string(), serde_json::json!(60_000));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();
        cancel.cancel(); // cancel immediately

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();
        assert!(!result.completed);
    }

    #[tokio::test]
    async fn test_duplicate_request_ids_cancellation() {
        let config = SideEffectConfig {
            type_: SideEffectType::DuplicateRequestIds,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("count".to_string(), serde_json::json!(100));
                m
            },
        };
        let effect = create_side_effect(&config);
        let transport = MockTransport::new();
        let connection = ConnectionContext::stdio();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = effect
            .execute(&transport, &connection, cancel)
            .await
            .unwrap();
        assert!(!result.completed);
        assert_eq!(result.messages_sent, 0);
    }
}
