//! Delivery behaviors (TJ-SPEC-004 F-010).
//!
//! Controls how response bytes are transmitted to the client. Each behavior
//! implements the [`DeliveryBehavior`] trait which wraps a JSON-RPC message
//! into bytes and sends them via the transport.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::config::schema::DeliveryConfig;
use crate::error::BehaviorError;
use crate::transport::jsonrpc::JsonRpcMessage;
use crate::transport::{Transport, TransportType};

// ============================================================================
// DeliveryResult
// ============================================================================

/// Result of a delivery operation.
///
/// Implements: TJ-SPEC-004 F-001
#[derive(Debug, Clone)]
pub struct DeliveryResult {
    /// Number of bytes sent to the transport.
    pub bytes_sent: usize,
    /// Wall-clock duration of the delivery.
    pub duration: Duration,
    /// Whether the delivery completed normally.
    pub completed: bool,
}

// ============================================================================
// DeliveryBehavior trait
// ============================================================================

/// Trait for delivery behaviors that control how responses are transmitted.
///
/// Implements: TJ-SPEC-004 F-001
#[async_trait::async_trait]
pub trait DeliveryBehavior: Send + Sync {
    /// Delivers a JSON-RPC message via the given transport.
    ///
    /// The `cancel` token allows cooperative cancellation of
    /// long-running deliveries (e.g., slow loris byte dripping).
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError>;

    /// Whether this behavior supports the given transport type.
    fn supports_transport(&self, transport_type: TransportType) -> bool;

    /// Human-readable name for logging and metrics.
    fn name(&self) -> &'static str;
}

// ============================================================================
// NormalDelivery
// ============================================================================

/// Standard delivery — serialize and send immediately with newline.
///
/// Implements: TJ-SPEC-004 F-002
pub struct NormalDelivery;

#[async_trait::async_trait]
impl DeliveryBehavior for NormalDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let len = serialized.len();
        let mut buf = serialized;
        buf.push(b'\n');
        transport.send_raw(&buf).await?;
        Ok(DeliveryResult {
            bytes_sent: len + 1,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "normal"
    }
}

// ============================================================================
// SlowLorisDelivery
// ============================================================================

/// Slow loris — drip bytes with delay between chunks.
///
/// Implements: TJ-SPEC-004 F-003
pub struct SlowLorisDelivery {
    byte_delay: Duration,
    chunk_size: usize,
}

impl SlowLorisDelivery {
    /// Creates a new slow loris delivery.
    ///
    /// Implements: TJ-SPEC-004 F-003
    #[must_use]
    pub fn new(byte_delay: Duration, chunk_size: usize) -> Self {
        Self {
            byte_delay,
            chunk_size: chunk_size.max(1),
        }
    }
}

#[async_trait::async_trait]
impl DeliveryBehavior for SlowLorisDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let mut buf = serialized;
        buf.push(b'\n');
        let total = buf.len();
        let mut sent_so_far = 0;

        for chunk in buf.chunks(self.chunk_size) {
            tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(DeliveryResult {
                        bytes_sent: sent_so_far,
                        duration: start.elapsed(),
                        completed: false,
                    });
                }
                result = transport.send_raw(chunk) => { result?; }
            }
            sent_so_far += chunk.len();
            if self.byte_delay > Duration::ZERO {
                tokio::select! {
                    () = cancel.cancelled() => {
                        return Ok(DeliveryResult {
                            bytes_sent: sent_so_far,
                            duration: start.elapsed(),
                            completed: false,
                        });
                    }
                    () = tokio::time::sleep(self.byte_delay) => {}
                }
            }
        }

        Ok(DeliveryResult {
            bytes_sent: total,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "slow_loris"
    }
}

// ============================================================================
// UnboundedLineDelivery
// ============================================================================

/// Never send newline — keeps sending bytes without `\n`.
///
/// Implements: TJ-SPEC-004 F-004
pub struct UnboundedLineDelivery {
    target_bytes: usize,
    padding_char: char,
}

impl UnboundedLineDelivery {
    /// Creates a new unbounded line delivery.
    ///
    /// Implements: TJ-SPEC-004 F-004
    #[must_use]
    pub const fn new(target_bytes: usize, padding_char: char) -> Self {
        Self {
            target_bytes,
            padding_char,
        }
    }
}

#[async_trait::async_trait]
impl DeliveryBehavior for UnboundedLineDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let serialized_len = serialized.len();

        if self.target_bytes == 0 {
            // Send just the serialized bytes, no newline
            transport.send_raw(&serialized).await?;
            return Ok(DeliveryResult {
                bytes_sent: serialized_len,
                duration: start.elapsed(),
                completed: true,
            });
        }

        // Build output: serialized bytes + padding up to target_bytes, no newline
        let padding_needed = self.target_bytes.saturating_sub(serialized_len);
        let mut buf = Vec::with_capacity(serialized_len + padding_needed);
        buf.extend_from_slice(&serialized);

        // Use the UTF-8 encoding of padding_char. For ASCII chars (the common
        // case) this is a single byte. For multi-byte chars, we repeat the full
        // encoding to fill the remaining space.
        if self.padding_char.is_ascii() {
            buf.resize(serialized_len + padding_needed, self.padding_char as u8);
        } else {
            let mut pad_buf = [0u8; 4];
            let pad = self.padding_char.encode_utf8(&mut pad_buf).as_bytes();
            while buf.len() < serialized_len + padding_needed {
                let remaining = (serialized_len + padding_needed) - buf.len();
                // NOTE: when remaining < pad.len(), the final character is
                // truncated, producing invalid UTF-8. This is intentional for
                // the unbounded-line attack mode.
                let chunk = remaining.min(pad.len());
                buf.extend_from_slice(&pad[..chunk]);
            }
        }

        transport.send_raw(&buf).await?;

        Ok(DeliveryResult {
            bytes_sent: buf.len(),
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "unbounded_line"
    }
}

// ============================================================================
// NestedJsonDelivery
// ============================================================================

/// Maximum nesting depth for `NestedJsonDelivery`.
///
/// Prevents OOM from extreme depth values that would produce
/// multi-gigabyte allocations via `Vec::with_capacity`.
const MAX_NESTED_JSON_DEPTH: usize = 100_000;

/// Maximum key length for `NestedJsonDelivery`.
///
/// A long key multiplied by `MAX_NESTED_JSON_DEPTH` can OOM:
/// e.g. 10k-char key × 100k depth ≈ 1GB. Capped at 1024 to keep
/// worst-case allocation under ~100MB.
const MAX_NESTED_JSON_KEY_LEN: usize = 1024;

/// Wrap response in deep JSON nesting (iterative, no recursion).
///
/// Implements: TJ-SPEC-004 F-005
pub struct NestedJsonDelivery {
    depth: usize,
    key: String,
}

impl NestedJsonDelivery {
    /// Creates a new nested JSON delivery.
    ///
    /// Depth is clamped to [`MAX_NESTED_JSON_DEPTH`] (100,000) and
    /// key length to [`MAX_NESTED_JSON_KEY_LEN`] (1024) to prevent OOM.
    ///
    /// Implements: TJ-SPEC-004 F-005
    #[must_use]
    pub fn new(depth: usize, mut key: String) -> Self {
        key.truncate(MAX_NESTED_JSON_KEY_LEN);
        Self {
            depth: depth.min(MAX_NESTED_JSON_DEPTH),
            key,
        }
    }
}

#[async_trait::async_trait]
impl DeliveryBehavior for NestedJsonDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        _cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        let inner_bytes = serde_json::to_vec(message)?;

        // Build nested JSON iteratively: {"key":{"key":...inner...}}
        let key_json = serde_json::to_string(&self.key)?;
        let prefix = format!("{{{key_json}:");
        let prefix_bytes = prefix.as_bytes();

        let total_size = prefix_bytes.len() * self.depth + inner_bytes.len() + self.depth + 1; // +1 for newline
        let mut output = Vec::with_capacity(total_size);

        for _ in 0..self.depth {
            output.extend_from_slice(prefix_bytes);
        }
        output.extend_from_slice(&inner_bytes);
        output.extend(std::iter::repeat_n(b'}', self.depth));
        output.push(b'\n');

        let bytes_sent = output.len();
        transport.send_raw(&output).await?;

        Ok(DeliveryResult {
            bytes_sent,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "nested_json"
    }
}

// ============================================================================
// ResponseDelayDelivery
// ============================================================================

/// Delay before responding, then send normally.
///
/// Implements: TJ-SPEC-004 F-006
pub struct ResponseDelayDelivery {
    delay: Duration,
}

impl ResponseDelayDelivery {
    /// Creates a new response delay delivery.
    ///
    /// Implements: TJ-SPEC-004 F-006
    #[must_use]
    pub const fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

#[async_trait::async_trait]
impl DeliveryBehavior for ResponseDelayDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
        cancel: CancellationToken,
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        tokio::select! {
            () = cancel.cancelled() => {
                return Ok(DeliveryResult {
                    bytes_sent: 0,
                    duration: start.elapsed(),
                    completed: false,
                });
            }
            () = tokio::time::sleep(self.delay) => {}
        }

        let serialized = serde_json::to_vec(message)?;
        let len = serialized.len();
        let mut buf = serialized;
        buf.push(b'\n');
        transport.send_raw(&buf).await?;

        Ok(DeliveryResult {
            bytes_sent: len + 1,
            duration: start.elapsed(),
            completed: true,
        })
    }

    fn supports_transport(&self, _transport_type: TransportType) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "response_delay"
    }
}

// ============================================================================
// Factory
// ============================================================================

/// Creates a delivery behavior from a configuration.
///
/// Implements: TJ-SPEC-004 F-001
#[must_use]
pub fn create_delivery_behavior(config: &DeliveryConfig) -> Box<dyn DeliveryBehavior> {
    match config {
        DeliveryConfig::Normal => Box::new(NormalDelivery),
        DeliveryConfig::SlowLoris {
            byte_delay_ms,
            chunk_size,
        } => Box::new(SlowLorisDelivery::new(
            Duration::from_millis(byte_delay_ms.unwrap_or(100)),
            chunk_size.unwrap_or(1),
        )),
        DeliveryConfig::UnboundedLine {
            target_bytes,
            padding_char,
        } => Box::new(UnboundedLineDelivery::new(
            target_bytes.unwrap_or(0),
            padding_char.unwrap_or('A'),
        )),
        DeliveryConfig::NestedJson { depth, key } => Box::new(NestedJsonDelivery::new(
            *depth,
            key.clone().unwrap_or_else(|| "a".to_string()),
        )),
        DeliveryConfig::ResponseDelay { delay_ms } => {
            Box::new(ResponseDelayDelivery::new(Duration::from_millis(*delay_ms)))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportType;
    use crate::transport::jsonrpc::JsonRpcResponse;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    // ========================================================================
    // Mock Transport
    // ========================================================================

    /// Mock transport that records all raw sends.
    struct MockTransport {
        raw_sends: Arc<Mutex<Vec<Vec<u8>>>>,
        transport_type: TransportType,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                raw_sends: Arc::new(Mutex::new(Vec::new())),
                transport_type: TransportType::Stdio,
            }
        }

        fn all_bytes(&self) -> Vec<u8> {
            self.raw_sends
                .lock()
                .unwrap()
                .iter()
                .flat_map(|v| v.iter().copied())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl Transport for MockTransport {
        async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
            let bytes = serde_json::to_vec(message)?;
            self.raw_sends.lock().unwrap().push(bytes);
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
            self.transport_type
        }

        async fn finalize_response(&self) -> crate::transport::Result<()> {
            Ok(())
        }

        fn connection_context(&self) -> crate::transport::ConnectionContext {
            crate::transport::ConnectionContext::stdio()
        }
    }

    fn test_message() -> JsonRpcMessage {
        JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!({"result": "ok"})))
    }

    // ========================================================================
    // Normal delivery
    // ========================================================================

    #[tokio::test]
    async fn test_normal_delivery_bytes() {
        let transport = MockTransport::new();
        let delivery = NormalDelivery;
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();

        let expected_json = serde_json::to_vec(&msg).unwrap();
        // bytes_sent = serialized + newline
        assert_eq!(result.bytes_sent, expected_json.len() + 1);
        assert!(result.completed);

        let sent = transport.all_bytes();
        assert_eq!(sent.last(), Some(&b'\n'));
    }

    // ========================================================================
    // SlowLoris
    // ========================================================================

    #[tokio::test]
    async fn test_slow_loris_timing() {
        let delay = Duration::from_millis(10);
        let transport = MockTransport::new();
        let delivery = SlowLorisDelivery::new(delay, 1);
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();

        let serialized = serde_json::to_vec(&msg).unwrap();
        let expected_chunks = serialized.len() + 1; // +1 for newline
        #[allow(clippy::cast_possible_truncation)]
        let expected_min = delay * (expected_chunks as u32) * 9 / 10; // 90% tolerance

        assert!(
            result.duration >= expected_min,
            "duration {:?} < expected min {:?} (chunks: {expected_chunks})",
            result.duration,
            expected_min
        );
        assert!(result.completed);
    }

    #[tokio::test]
    async fn test_slow_loris_zero_delay() {
        // EC-BEH-001: zero delay should complete quickly
        let transport = MockTransport::new();
        let delivery = SlowLorisDelivery::new(Duration::ZERO, 1);
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.duration < Duration::from_millis(100));
        assert!(result.completed);
    }

    // ========================================================================
    // NestedJson
    // ========================================================================

    #[tokio::test]
    async fn test_nested_json_valid_at_depth_100() {
        // serde_json has a default recursion limit of 128, so use depth 100
        let transport = MockTransport::new();
        let delivery = NestedJsonDelivery::new(100, "a".to_string());
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.completed);

        // Verify the output is valid JSON (minus trailing newline)
        let all = transport.all_bytes();
        let json_bytes = &all[..all.len() - 1]; // strip newline
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        // Navigate into the nesting to verify structure
        let mut current = &parsed;
        for _ in 0..100 {
            current = current.get("a").expect("expected nested key 'a'");
        }
        // The inner value should be the original message
        assert!(current.get("jsonrpc").is_some());
    }

    #[tokio::test]
    async fn test_nested_json_structure_at_depth_1000() {
        // Verify structural correctness without parsing (serde_json recursion limit)
        let transport = MockTransport::new();
        let delivery = NestedJsonDelivery::new(1000, "a".to_string());
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.completed);

        let all = transport.all_bytes();
        let json_bytes = &all[..all.len() - 1]; // strip newline
        let prefix = b"{\"a\":";
        // Should start with 1000 repetitions of the prefix
        for i in 0..1000 {
            let offset = i * prefix.len();
            assert_eq!(
                &json_bytes[offset..offset + prefix.len()],
                prefix,
                "prefix mismatch at nesting level {i}"
            );
        }
        // Should end with 1000 closing braces
        let closing = &json_bytes[json_bytes.len() - 1000..];
        assert!(closing.iter().all(|&b| b == b'}'));
    }

    #[tokio::test]
    async fn test_nested_json_no_stack_overflow_at_15000() {
        let transport = MockTransport::new();
        let delivery = NestedJsonDelivery::new(15000, "a".to_string());
        let msg = test_message();

        // Should complete without stack overflow
        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.completed);
        assert!(result.bytes_sent > 0);
    }

    // ========================================================================
    // UnboundedLine
    // ========================================================================

    #[tokio::test]
    async fn test_unbounded_line_no_newline() {
        let transport = MockTransport::new();
        let delivery = UnboundedLineDelivery::new(0, 'A');
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.completed);

        let sent = transport.all_bytes();
        assert_ne!(sent.last(), Some(&b'\n'));
    }

    #[tokio::test]
    async fn test_unbounded_line_with_padding() {
        let transport = MockTransport::new();
        let delivery = UnboundedLineDelivery::new(1000, 'X');
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.bytes_sent, 1000);
        assert!(result.completed);

        let sent = transport.all_bytes();
        assert_ne!(sent.last(), Some(&b'\n'));
    }

    // ========================================================================
    // ResponseDelay
    // ========================================================================

    #[tokio::test]
    async fn test_response_delay_timing() {
        let delay = Duration::from_millis(50);
        let transport = MockTransport::new();
        let delivery = ResponseDelayDelivery::new(delay);
        let msg = test_message();

        let result = delivery
            .deliver(&msg, &transport, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            result.duration >= delay,
            "duration {:?} < configured delay {:?}",
            result.duration,
            delay
        );
        assert!(result.completed);
    }

    // ========================================================================
    // Factory
    // ========================================================================

    #[test]
    fn test_factory_normal() {
        let behavior = create_delivery_behavior(&DeliveryConfig::Normal);
        assert_eq!(behavior.name(), "normal");
    }

    #[test]
    fn test_factory_slow_loris_defaults() {
        let behavior = create_delivery_behavior(&DeliveryConfig::SlowLoris {
            byte_delay_ms: None,
            chunk_size: None,
        });
        assert_eq!(behavior.name(), "slow_loris");
    }

    #[test]
    fn test_factory_nested_json_default_key() {
        let behavior = create_delivery_behavior(&DeliveryConfig::NestedJson {
            depth: 100,
            key: None,
        });
        assert_eq!(behavior.name(), "nested_json");
    }

    #[test]
    fn test_factory_response_delay() {
        let behavior = create_delivery_behavior(&DeliveryConfig::ResponseDelay { delay_ms: 500 });
        assert_eq!(behavior.name(), "response_delay");
    }

    #[tokio::test]
    async fn test_slow_loris_precancelled() {
        // EC-BEH-002: pre-cancelled token → completed: false
        // Note: select! may poll both branches; the first chunk's send_raw
        // may complete before cancellation is noticed, so bytes_sent can be
        // 0 or chunk_size. The key invariant is completed == false.
        let transport = MockTransport::new();
        let delivery = SlowLorisDelivery::new(Duration::from_millis(100), 1);
        let msg = test_message();

        let cancel = CancellationToken::new();
        cancel.cancel(); // Pre-cancel

        let result = delivery.deliver(&msg, &transport, cancel).await.unwrap();

        assert!(
            !result.completed,
            "should not be completed when pre-cancelled"
        );
        // bytes_sent must be less than the full message
        let full_size = serde_json::to_vec(&msg).unwrap().len() + 1;
        assert!(
            result.bytes_sent < full_size,
            "should not have sent full message ({} >= {full_size})",
            result.bytes_sent
        );
    }

    #[tokio::test]
    async fn test_response_delay_precancelled() {
        // EC-BEH-002: pre-cancel → completed: false
        let transport = MockTransport::new();
        let delivery = ResponseDelayDelivery::new(Duration::from_secs(60));
        let msg = test_message();

        let cancel = CancellationToken::new();
        cancel.cancel(); // Pre-cancel

        let result = delivery.deliver(&msg, &transport, cancel).await.unwrap();

        assert!(
            !result.completed,
            "should not be completed when pre-cancelled"
        );
        assert_eq!(
            result.bytes_sent, 0,
            "should not send any bytes when pre-cancelled"
        );
    }

    #[test]
    fn test_all_support_stdio() {
        let configs: Vec<DeliveryConfig> = vec![
            DeliveryConfig::Normal,
            DeliveryConfig::SlowLoris {
                byte_delay_ms: Some(10),
                chunk_size: Some(1),
            },
            DeliveryConfig::UnboundedLine {
                target_bytes: Some(100),
                padding_char: None,
            },
            DeliveryConfig::NestedJson {
                depth: 10,
                key: None,
            },
            DeliveryConfig::ResponseDelay { delay_ms: 100 },
        ];

        for config in &configs {
            let behavior = create_delivery_behavior(config);
            assert!(behavior.supports_transport(TransportType::Stdio));
        }
    }

    // ========================================================================
    // EC-BEH-019: Slow loris chunk_size larger than message
    // ========================================================================

    #[tokio::test]
    async fn test_slow_loris_chunk_larger_than_message() {
        // EC-BEH-019: chunk_size: 10000 on small message → effectively normal
        let transport = MockTransport::new();
        let delivery = SlowLorisDelivery::new(Duration::from_millis(10), 10_000);
        let msg = test_message();
        let cancel = CancellationToken::new();

        let result = delivery.deliver(&msg, &transport, cancel).await.unwrap();

        assert!(result.completed);
        let full_size = serde_json::to_vec(&msg).unwrap().len() + 1;
        assert_eq!(result.bytes_sent, full_size);
        // Should be fast since entire message fits in one chunk
        assert!(result.duration.as_millis() < 500);
    }

    // ========================================================================
    // EC-BEH-014: Nested JSON with empty/null result
    // ========================================================================

    #[tokio::test]
    async fn test_nested_json_with_null_result() {
        // EC-BEH-014: wrapping null result should still produce valid JSON
        let transport = MockTransport::new();
        let delivery = create_delivery_behavior(&DeliveryConfig::NestedJson {
            depth: 5,
            key: None,
        });
        let msg = JsonRpcMessage::Response(JsonRpcResponse::success(json!(1), json!(null)));
        let cancel = CancellationToken::new();

        let result = delivery.deliver(&msg, &transport, cancel).await.unwrap();
        assert!(result.completed);

        // NestedJsonDelivery wraps the ENTIRE JSON-RPC message in nesting:
        // {"a":{"a":{"a":{"a":{"a":{"jsonrpc":"2.0","id":1,"result":null}}}}}}
        let sent = transport.all_bytes();
        let sent_str = String::from_utf8(sent).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(sent_str.trim()).unwrap();

        // Drill through 5 levels of "a" nesting to find the original message
        let mut inner = &parsed;
        for _ in 0..5 {
            inner = inner.get("a").expect("expected nesting key 'a'");
        }
        assert_eq!(inner.get("result"), Some(&json!(null)));
    }

    // ========================================================================
    // EC-BEH-016: Behavior override to normal
    // ========================================================================

    #[tokio::test]
    async fn test_normal_override_produces_single_send() {
        // EC-BEH-016: Normal delivery → single send, no delays
        let transport = MockTransport::new();
        let delivery = create_delivery_behavior(&DeliveryConfig::Normal);
        let msg = test_message();
        let cancel = CancellationToken::new();

        let result = delivery.deliver(&msg, &transport, cancel).await.unwrap();

        assert!(result.completed);
        assert!(result.duration.as_millis() < 100);
        // Normal delivery should produce exactly 1 send_raw call
        let sends = transport.raw_sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "normal delivery should produce one send");
    }

    // ========================================================================
    // EC-BEH-012: Batch amplify with batch_size: 0
    // ========================================================================

    #[test]
    fn test_batch_amplify_zero_clamped() {
        // EC-BEH-012: batch_size 0 is clamped to 1 (max(1))
        use crate::config::schema::{SideEffectConfig, SideEffectType, SideEffectTrigger};
        use std::collections::HashMap;
        use crate::behavior::side_effects::create_side_effect;

        let config = SideEffectConfig {
            type_: SideEffectType::BatchAmplify,
            trigger: SideEffectTrigger::OnRequest,
            params: {
                let mut m = HashMap::new();
                m.insert("batch_size".to_string(), json!(0));
                m
            },
        };
        let effect = create_side_effect(&config);
        assert_eq!(effect.name(), "batch_amplify");
        // The effect is created successfully; batch_size is clamped to 1 via .max(1)
    }
}
