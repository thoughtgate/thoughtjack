//! Delivery behaviors (TJ-SPEC-004 F-010).
//!
//! Controls how response bytes are transmitted to the client. Each behavior
//! implements the [`DeliveryBehavior`] trait which wraps a JSON-RPC message
//! into bytes and sends them via the transport.

use std::time::Duration;

use crate::config::schema::DeliveryConfig;
use crate::error::BehaviorError;
use crate::transport::jsonrpc::JsonRpcMessage;
use crate::transport::{Transport, TransportType};

// ============================================================================
// DeliveryResult
// ============================================================================

/// Result of a delivery operation.
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
#[async_trait::async_trait]
pub trait DeliveryBehavior: Send + Sync {
    /// Delivers a JSON-RPC message via the given transport.
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
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
pub struct NormalDelivery;

#[async_trait::async_trait]
impl DeliveryBehavior for NormalDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
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
pub struct SlowLorisDelivery {
    byte_delay: Duration,
    chunk_size: usize,
}

impl SlowLorisDelivery {
    /// Creates a new slow loris delivery.
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
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        let serialized = serde_json::to_vec(message)?;
        let mut buf = serialized;
        buf.push(b'\n');
        let total = buf.len();

        for chunk in buf.chunks(self.chunk_size) {
            transport.send_raw(chunk).await?;
            if self.byte_delay > Duration::ZERO {
                tokio::time::sleep(self.byte_delay).await;
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
pub struct UnboundedLineDelivery {
    target_bytes: usize,
    padding_char: char,
}

impl UnboundedLineDelivery {
    /// Creates a new unbounded line delivery.
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

        let pad_byte = self.padding_char as u8;
        buf.resize(serialized_len + padding_needed, pad_byte);

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

/// Wrap response in deep JSON nesting (iterative, no recursion).
pub struct NestedJsonDelivery {
    depth: usize,
    key: String,
}

impl NestedJsonDelivery {
    /// Creates a new nested JSON delivery.
    #[must_use]
    pub const fn new(depth: usize, key: String) -> Self {
        Self { depth, key }
    }
}

#[async_trait::async_trait]
impl DeliveryBehavior for NestedJsonDelivery {
    async fn deliver(
        &self,
        message: &JsonRpcMessage,
        transport: &dyn Transport,
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
pub struct ResponseDelayDelivery {
    delay: Duration,
}

impl ResponseDelayDelivery {
    /// Creates a new response delay delivery.
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
    ) -> Result<DeliveryResult, BehaviorError> {
        let start = std::time::Instant::now();
        tokio::time::sleep(self.delay).await;

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

        let result = delivery.deliver(&msg, &transport).await.unwrap();

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

        let result = delivery.deliver(&msg, &transport).await.unwrap();

        let serialized = serde_json::to_vec(&msg).unwrap();
        let expected_chunks = serialized.len() + 1; // +1 for newline
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

        let result = delivery.deliver(&msg, &transport).await.unwrap();
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

        let result = delivery.deliver(&msg, &transport).await.unwrap();
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

        let result = delivery.deliver(&msg, &transport).await.unwrap();
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
        let result = delivery.deliver(&msg, &transport).await.unwrap();
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

        let result = delivery.deliver(&msg, &transport).await.unwrap();
        assert!(result.completed);

        let sent = transport.all_bytes();
        assert_ne!(sent.last(), Some(&b'\n'));
    }

    #[tokio::test]
    async fn test_unbounded_line_with_padding() {
        let transport = MockTransport::new();
        let delivery = UnboundedLineDelivery::new(1000, 'X');
        let msg = test_message();

        let result = delivery.deliver(&msg, &transport).await.unwrap();
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

        let result = delivery.deliver(&msg, &transport).await.unwrap();
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
}
