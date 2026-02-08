//! Batch notifications generator (TJ-SPEC-005).
//!
//! Generates arrays of JSON-RPC notifications for batch attack testing.
//! Builds the JSON array manually for efficiency with large counts.

use crate::config::schema::GeneratorLimits;
use crate::error::GeneratorError;

use super::{
    GeneratedPayload, PayloadGenerator, PayloadStream, STREAMING_THRESHOLD, extract_string,
    extract_value,
};
use serde_json::Value;
use std::collections::HashMap;

/// Threshold for switching to streaming batch generation.
const STREAM_COUNT_THRESHOLD: usize = 10_000;

/// Generates JSON-RPC notification arrays.
///
/// Produces a JSON array of identical JSON-RPC notifications, useful for
/// testing batch processing limits and memory handling. Output is fully
/// deterministic (no random component).
///
/// Implements: TJ-SPEC-005 F-003
#[derive(Debug)]
pub struct BatchNotificationsGenerator {
    count: usize,
    /// Cached single notification JSON string.
    single_notification: String,
}

impl BatchNotificationsGenerator {
    /// Creates a new batch notifications generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::InvalidParameters`] if `count` is missing.
    /// Returns [`GeneratorError::LimitExceeded`] if count exceeds
    /// `limits.max_batch_size` or estimated size exceeds `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-003
    pub fn new(
        params: &HashMap<String, Value>,
        limits: &GeneratorLimits,
    ) -> Result<Self, GeneratorError> {
        let count = super::require_usize(params, "count")?;

        if count > limits.max_batch_size {
            return Err(GeneratorError::LimitExceeded(format!(
                "count {count} exceeds max_batch_size {}",
                limits.max_batch_size
            )));
        }

        let method = extract_string(params, "method", "notifications/message");
        let notification_params = extract_value(params, "params");

        // Build the single notification JSON
        let notification = notification_params.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": method,
                })
            },
            |p| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": p,
                })
            },
        );

        let single_notification = serde_json::to_string(&notification)
            .map_err(|e| GeneratorError::GenerationFailed(e.to_string()))?;

        let this = Self {
            count,
            single_notification,
        };

        let estimated = this.estimated_size();
        if estimated > limits.max_payload_bytes {
            return Err(GeneratorError::LimitExceeded(format!(
                "estimated size {estimated} exceeds max_payload_bytes {}",
                limits.max_payload_bytes
            )));
        }

        Ok(this)
    }
}

impl PayloadGenerator for BatchNotificationsGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        if self.count == 0 {
            return Ok(GeneratedPayload::Buffered(b"[]".to_vec()));
        }

        let estimated = self.estimated_size();

        if self.count > STREAM_COUNT_THRESHOLD && estimated > STREAMING_THRESHOLD {
            return Ok(GeneratedPayload::Streamed(Box::new(
                BatchNotificationsStream::new(
                    self.count,
                    self.single_notification.clone(),
                    estimated,
                ),
            )));
        }

        // Build the array manually for efficiency
        let notif_len = self.single_notification.len();
        // [notif,notif,...notif] = 2 + count * notif_len + (count - 1) commas
        let capacity = 2 + self.count * notif_len + self.count.saturating_sub(1);
        let mut output = Vec::with_capacity(capacity);

        output.push(b'[');
        for i in 0..self.count {
            if i > 0 {
                output.push(b',');
            }
            output.extend_from_slice(self.single_notification.as_bytes());
        }
        output.push(b']');

        Ok(GeneratedPayload::Buffered(output))
    }

    fn estimated_size(&self) -> usize {
        if self.count == 0 {
            return 2; // "[]"
        }
        // [notif,notif,...notif]
        let notif_len = self.single_notification.len();
        2 + self.count * notif_len + self.count.saturating_sub(1)
    }

    fn name(&self) -> &'static str {
        "batch_notifications"
    }

    fn produces_json(&self) -> bool {
        true
    }
}

/// Streaming batch notifications source.
#[derive(Debug)]
struct BatchNotificationsStream {
    remaining: usize,
    total_estimated: usize,
    single_notification: String,
    started: bool,
    finished: bool,
}

impl BatchNotificationsStream {
    #[allow(clippy::missing_const_for_fn)]
    fn new(count: usize, single_notification: String, total_estimated: usize) -> Self {
        Self {
            remaining: count,
            total_estimated,
            single_notification,
            started: false,
            finished: false,
        }
    }
}

impl PayloadStream for BatchNotificationsStream {
    fn next_chunk(&mut self) -> Option<Vec<u8>> {
        if self.finished {
            return None;
        }

        if !self.started {
            self.started = true;
            // Emit opening bracket + first batch of notifications
            let batch_size = self.remaining.min(1000);
            let mut chunk =
                Vec::with_capacity(1 + batch_size * (self.single_notification.len() + 1));
            chunk.push(b'[');
            for i in 0..batch_size {
                if i > 0 {
                    chunk.push(b',');
                }
                chunk.extend_from_slice(self.single_notification.as_bytes());
            }
            self.remaining -= batch_size;
            if self.remaining == 0 {
                chunk.push(b']');
                self.finished = true;
            }
            return Some(chunk);
        }

        // remaining > 0 here: if it were 0, the first chunk would have set
        // finished=true and we would have returned None above.
        debug_assert!(self.remaining > 0);

        let batch_size = self.remaining.min(1000);
        let mut chunk = Vec::with_capacity(batch_size * (self.single_notification.len() + 1));
        for _ in 0..batch_size {
            chunk.push(b',');
            chunk.extend_from_slice(self.single_notification.as_bytes());
        }
        self.remaining -= batch_size;

        if self.remaining == 0 {
            chunk.push(b']');
            self.finished = true;
        }

        Some(chunk)
    }

    fn estimated_total(&self) -> usize {
        self.total_estimated
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn default_limits() -> GeneratorLimits {
        GeneratorLimits::default()
    }

    fn make_params(pairs: Vec<(&str, Value)>) -> HashMap<String, Value> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn valid_json_rpc_array() {
        let params = make_params(vec![("count", json!(3))]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for item in arr {
            assert_eq!(item["jsonrpc"], "2.0");
            assert_eq!(item["method"], "notifications/message");
        }
    }

    #[test]
    fn correct_count() {
        let params = make_params(vec![("count", json!(100))]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 100);
    }

    #[test]
    fn custom_method_and_params() {
        let params = make_params(vec![
            ("count", json!(2)),
            ("method", json!("test/notify")),
            ("params", json!({"key": "value"})),
        ]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr[0]["method"], "test/notify");
        assert_eq!(arr[0]["params"]["key"], "value");
    }

    #[test]
    fn zero_count_empty_array() {
        let params = make_params(vec![("count", json!(0))]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data, b"[]");
    }

    #[test]
    fn deterministic_output() {
        let params = make_params(vec![("count", json!(10))]);
        let generator1 = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let generator2 = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn rejects_count_over_limit() {
        let limits = GeneratorLimits {
            max_batch_size: 5,
            ..default_limits()
        };
        let params = make_params(vec![("count", json!(6))]);
        let err = BatchNotificationsGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    #[test]
    fn rejects_missing_count() {
        let params = HashMap::new();
        let err = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap_err();
        assert!(matches!(err, GeneratorError::InvalidParameters(_)));
    }

    #[test]
    fn estimated_size_matches_actual() {
        let params = make_params(vec![
            ("count", json!(50)),
            ("method", json!("test/method")),
            ("params", json!({"data": [1,2,3]})),
        ]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let estimated = generator.estimated_size();
        let actual = generator.generate().unwrap().into_bytes().len();
        assert_eq!(estimated, actual);
    }

    // EC-GEN-013: Batch notifications with deeply nested params object
    #[test]
    fn deeply_nested_params() {
        let nested_params = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "data": [1, 2, 3],
                        "flag": true
                    }
                }
            }
        });
        let params = make_params(vec![("count", json!(5)), ("params", nested_params.clone())]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 5);
        for item in arr {
            assert_eq!(item["params"], nested_params);
        }
    }

    // EC-GEN-007: count=0 produces empty JSON array
    #[test]
    fn test_count_zero_rejected() {
        let params = make_params(vec![("count", json!(0))]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data, b"[]", "count=0 should produce empty JSON array");
    }

    // EC-GEN-011: count exceeding max_batch_size returns LimitExceeded
    #[test]
    fn test_exceeds_max_batch_size() {
        let limits = GeneratorLimits {
            max_batch_size: 20,
            ..default_limits()
        };
        // Exactly at limit should succeed
        let params_at = make_params(vec![("count", json!(20))]);
        assert!(BatchNotificationsGenerator::new(&params_at, &limits).is_ok());

        // One over limit should fail
        let params_over = make_params(vec![("count", json!(21))]);
        let err = BatchNotificationsGenerator::new(&params_over, &limits).unwrap_err();
        assert!(
            matches!(err, GeneratorError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    // Verify output parses as a valid JSON array with correct structure
    #[test]
    fn test_produces_json_array() {
        let params = make_params(vec![
            ("count", json!(5)),
            ("method", json!("test/ping")),
            ("params", json!({"status": "ok"})),
        ]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        assert!(parsed.is_array(), "output should be a JSON array");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 5, "array should have exactly 5 elements");
        for (i, item) in arr.iter().enumerate() {
            assert_eq!(
                item["jsonrpc"], "2.0",
                "item {i} should have jsonrpc=2.0"
            );
            assert_eq!(
                item["method"], "test/ping",
                "item {i} should have correct method"
            );
            assert_eq!(
                item["params"]["status"], "ok",
                "item {i} should have correct params"
            );
            // Notifications should NOT have an "id" field
            assert!(
                item.get("id").is_none(),
                "notifications should not have an id field"
            );
        }
    }

    // EC-GEN-003: single notification (count=1) produces valid single-element array
    #[test]
    fn single_notification_valid_array() {
        let params = make_params(vec![("count", json!(1))]);
        let generator = BatchNotificationsGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&data).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["jsonrpc"], "2.0");
    }
}
