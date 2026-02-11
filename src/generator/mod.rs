//! Payload generator module (TJ-SPEC-005).
//!
//! This module implements the `$generate` directive system for creating
//! adversarial test payloads. Generators are **factories** — they store
//! configuration at load time and produce bytes at response time.
//!
//! # Architecture
//!
//! ```text
//! Config Load:  $generate: {...}  →  Box<dyn PayloadGenerator>   (no bytes yet)
//! Response:     generator.generate()  →  GeneratedPayload        (bytes now)
//! ```
//!
//! # Generators
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`NestedJsonGenerator`] | Deeply nested JSON structures |
//! | [`GarbageGenerator`] | Random bytes with configurable charset |
//! | [`BatchNotificationsGenerator`] | JSON-RPC notification arrays |
//! | [`RepeatedKeysGenerator`] | JSON with duplicate keys (hash collision) |
//! | [`UnicodeSpamGenerator`] | Unicode attack sequences |
//! | [`AnsiEscapeGenerator`] | ANSI terminal escape sequences |

mod ansi_escape;
mod batch_notifications;
mod garbage;
mod nested_json;
mod repeated_keys;
mod unicode_spam;

pub use ansi_escape::AnsiEscapeGenerator;
pub use batch_notifications::BatchNotificationsGenerator;
pub use garbage::{GarbageGenerator, GarbageStream};
pub use nested_json::NestedJsonGenerator;
pub use repeated_keys::RepeatedKeysGenerator;
pub use unicode_spam::UnicodeSpamGenerator;

use crate::config::schema::{GeneratorConfig, GeneratorLimits, GeneratorType};
use crate::error::GeneratorError;
use serde_json::Value;
use std::collections::HashMap;

/// Streaming threshold: payloads larger than this are streamed (1 MB).
///
/// Implements: TJ-SPEC-005 F-009
pub const STREAMING_THRESHOLD: usize = 1_024 * 1_024;

// ============================================================================
// Core Traits
// ============================================================================

/// A payload generator factory.
///
/// Generators are created at config load time but produce bytes only
/// when `generate()` is called at response time. This prevents OOM
/// on startup for large payload definitions.
///
/// Implements: TJ-SPEC-005 F-001
pub trait PayloadGenerator: Send + Sync + std::fmt::Debug {
    /// Generate the payload bytes.
    ///
    /// Called at response time. May return buffered or streamed data
    /// depending on estimated size.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::GenerationFailed`] if payload generation fails.
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError>;

    /// Estimated output size in bytes.
    ///
    /// Used for limit checking before generation. Must be computable
    /// without actually generating the payload.
    fn estimated_size(&self) -> usize;

    /// Human-readable name for logging and metrics.
    fn name(&self) -> &'static str;

    /// Whether this generator produces valid JSON output.
    ///
    /// Used by the transport layer to set appropriate content types
    /// and by validation to verify schema compatibility.
    ///
    /// Implements: TJ-SPEC-005 F-001
    fn produces_json(&self) -> bool {
        false
    }
}

/// Generated payload — either fully buffered or streamed in chunks.
///
/// Implements: TJ-SPEC-005 F-001
#[derive(Debug)]
pub enum GeneratedPayload {
    /// Fully materialized payload bytes.
    Buffered(Vec<u8>),

    /// Streamed payload for large outputs (> [`STREAMING_THRESHOLD`]).
    Streamed(Box<dyn PayloadStream>),
}

impl GeneratedPayload {
    /// Returns the size hint for this payload.
    ///
    /// For [`Buffered`](GeneratedPayload::Buffered), returns the exact size.
    /// For [`Streamed`](GeneratedPayload::Streamed), returns the estimated total.
    ///
    /// Implements: TJ-SPEC-005 F-001
    #[must_use]
    pub fn size_hint(&self) -> usize {
        match self {
            Self::Buffered(data) => data.len(),
            Self::Streamed(stream) => stream.estimated_total(),
        }
    }

    /// Materializes the payload into a byte vector.
    ///
    /// For buffered payloads, returns the inner vector directly.
    /// For streamed payloads, collects all chunks. Use with caution
    /// on large payloads — prefer streaming where possible.
    ///
    /// Implements: TJ-SPEC-005 F-001
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        /// Maximum materialization size (100 MB).
        const MAX_MATERIALIZATION: usize = 100 * 1024 * 1024;

        match self {
            Self::Buffered(data) => data,
            Self::Streamed(mut stream) => {
                let estimated = stream.estimated_total().min(MAX_MATERIALIZATION);
                let mut result = Vec::with_capacity(estimated);
                while let Some(chunk) = stream.next_chunk() {
                    result.extend_from_slice(&chunk);
                    if result.len() > MAX_MATERIALIZATION {
                        tracing::warn!(
                            bytes = result.len(),
                            "stream exceeded 100MB materialization limit, truncating"
                        );
                        break;
                    }
                }
                result
            }
        }
    }
}

/// A streaming payload source for large outputs.
///
/// Produces chunks of bytes until exhausted. Used when the full
/// payload would exceed [`STREAMING_THRESHOLD`].
///
/// Implements: TJ-SPEC-005 F-009
pub trait PayloadStream: Send + std::fmt::Debug {
    /// Returns the next chunk of bytes, or `None` when exhausted.
    fn next_chunk(&mut self) -> Option<Vec<u8>>;

    /// Estimated total size in bytes.
    fn estimated_total(&self) -> usize;
}

// ============================================================================
// Parameter extraction helpers
// ============================================================================

/// Extracts a `usize` parameter from the params map, returning `default` if missing.
///
/// Logs a warning if the value is present but not a valid non-negative integer.
pub(crate) fn extract_usize(params: &HashMap<String, Value>, key: &str, default: usize) -> usize {
    let Some(value) = params.get(key) else {
        return default;
    };
    value.as_u64().map_or_else(
        || {
            tracing::warn!(key, ?value, "expected non-negative integer, using default");
            default
        },
        |v| {
            usize::try_from(v).unwrap_or_else(|_| {
                tracing::warn!(key, v, "value exceeds usize range, using default");
                default
            })
        },
    )
}

/// Extracts a `u64` parameter from the params map, returning `default` if missing.
pub(crate) fn extract_u64(params: &HashMap<String, Value>, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// Extracts a `String` parameter from the params map, returning `default` if missing.
pub(crate) fn extract_string(params: &HashMap<String, Value>, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// Extracts a `Value` parameter from the params map, returning `None` if missing.
pub(crate) fn extract_value(params: &HashMap<String, Value>, key: &str) -> Option<Value> {
    params.get(key).cloned()
}

/// Extracts a required `usize` parameter, returning an error if missing or invalid.
pub(crate) fn require_usize(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<usize, GeneratorError> {
    let value = params.get(key).ok_or_else(|| {
        GeneratorError::InvalidParameters(format!("missing required parameter: {key}"))
    })?;
    let n = value.as_u64().ok_or_else(|| {
        GeneratorError::InvalidParameters(format!(
            "parameter '{key}' must be a non-negative integer, got {value}"
        ))
    })?;
    usize::try_from(n).map_err(|_| {
        GeneratorError::InvalidParameters(format!(
            "parameter '{key}' value {n} exceeds platform usize range"
        ))
    })
}

// ============================================================================
// Factory
// ============================================================================

/// Creates a generator from configuration.
///
/// Dispatches on [`GeneratorType`] to construct the appropriate generator.
/// Limit validation happens in each generator's constructor, not in `generate()`.
///
/// # Errors
///
/// Returns [`GeneratorError::LimitExceeded`] if parameters exceed limits.
/// Returns [`GeneratorError::InvalidParameters`] if required params are missing.
///
// TODO(TJ-SPEC-005 F-010): Add generator caching with LRU eviction and
// memory budget tracking. Currently generators are created per-call which
// is correct but suboptimal for repeated identical configs.
// Tracked for v0.3 milestone.
///
/// Implements: TJ-SPEC-005 F-001
pub fn create_generator(
    config: &GeneratorConfig,
    limits: &GeneratorLimits,
) -> Result<Box<dyn PayloadGenerator>, GeneratorError> {
    match config.type_ {
        GeneratorType::NestedJson => {
            Ok(Box::new(NestedJsonGenerator::new(&config.params, limits)?))
        }
        GeneratorType::Garbage => Ok(Box::new(GarbageGenerator::new(&config.params, limits)?)),
        GeneratorType::BatchNotifications => Ok(Box::new(BatchNotificationsGenerator::new(
            &config.params,
            limits,
        )?)),
        GeneratorType::RepeatedKeys => Ok(Box::new(RepeatedKeysGenerator::new(
            &config.params,
            limits,
        )?)),
        GeneratorType::UnicodeSpam => {
            Ok(Box::new(UnicodeSpamGenerator::new(&config.params, limits)?))
        }
        GeneratorType::AnsiEscape => {
            Ok(Box::new(AnsiEscapeGenerator::new(&config.params, limits)?))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::GeneratorLimits;
    use serde_json::json;

    const fn make_config(type_: GeneratorType, params: HashMap<String, Value>) -> GeneratorConfig {
        GeneratorConfig { type_, params }
    }

    fn default_limits() -> GeneratorLimits {
        GeneratorLimits::default()
    }

    #[test]
    fn factory_creates_nested_json() {
        let mut params = HashMap::new();
        params.insert("depth".to_string(), json!(10));
        let config = make_config(GeneratorType::NestedJson, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "nested_json");
    }

    #[test]
    fn factory_creates_garbage() {
        let mut params = HashMap::new();
        params.insert("bytes".to_string(), json!(100));
        let config = make_config(GeneratorType::Garbage, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "garbage");
    }

    #[test]
    fn factory_creates_batch_notifications() {
        let mut params = HashMap::new();
        params.insert("count".to_string(), json!(10));
        let config = make_config(GeneratorType::BatchNotifications, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "batch_notifications");
    }

    #[test]
    fn factory_creates_repeated_keys() {
        let mut params = HashMap::new();
        params.insert("count".to_string(), json!(10));
        let config = make_config(GeneratorType::RepeatedKeys, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "repeated_keys");
    }

    #[test]
    fn factory_creates_unicode_spam() {
        let mut params = HashMap::new();
        params.insert("bytes".to_string(), json!(100));
        let config = make_config(GeneratorType::UnicodeSpam, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "unicode_spam");
    }

    #[test]
    fn factory_creates_ansi_escape() {
        let mut params = HashMap::new();
        params.insert("sequences".to_string(), json!(["cursor_move"]));
        let config = make_config(GeneratorType::AnsiEscape, params);
        let generator = create_generator(&config, &default_limits()).unwrap();
        assert_eq!(generator.name(), "ansi_escape");
    }

    #[test]
    fn factory_rejects_depth_over_limit() {
        let limits = GeneratorLimits {
            max_nest_depth: 100,
            ..default_limits()
        };
        let mut params = HashMap::new();
        params.insert("depth".to_string(), json!(101));
        let config = make_config(GeneratorType::NestedJson, params);
        let err = create_generator(&config, &limits).unwrap_err();
        assert!(
            matches!(err, GeneratorError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn factory_rejects_bytes_over_limit() {
        let limits = GeneratorLimits {
            max_payload_bytes: 100,
            ..default_limits()
        };
        let mut params = HashMap::new();
        params.insert("bytes".to_string(), json!(101));
        let config = make_config(GeneratorType::Garbage, params);
        let err = create_generator(&config, &limits).unwrap_err();
        assert!(
            matches!(err, GeneratorError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn factory_rejects_count_over_limit() {
        let limits = GeneratorLimits {
            max_batch_size: 10,
            ..default_limits()
        };
        let mut params = HashMap::new();
        params.insert("count".to_string(), json!(11));
        let config = make_config(GeneratorType::BatchNotifications, params);
        let err = create_generator(&config, &limits).unwrap_err();
        assert!(
            matches!(err, GeneratorError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn factory_rejects_missing_required_param() {
        let params = HashMap::new();
        let config = make_config(GeneratorType::Garbage, params);
        let err = create_generator(&config, &default_limits()).unwrap_err();
        assert!(
            matches!(err, GeneratorError::InvalidParameters(_)),
            "expected InvalidParameters, got {err:?}"
        );
    }

    #[test]
    fn generated_payload_buffered_size_hint() {
        let payload = GeneratedPayload::Buffered(vec![0u8; 42]);
        assert_eq!(payload.size_hint(), 42);
    }

    #[test]
    fn generated_payload_buffered_into_bytes() {
        let data = vec![1u8, 2, 3, 4, 5];
        let payload = GeneratedPayload::Buffered(data.clone());
        assert_eq!(payload.into_bytes(), data);
    }

    // EC-GEN-020: Concurrent generation from multiple threads
    #[test]
    fn test_concurrent_generation() {
        use std::sync::Arc;

        let mut params = HashMap::new();
        params.insert("bytes".to_string(), json!(256));
        params.insert("charset".to_string(), json!("ascii"));
        params.insert("seed".to_string(), json!(42));
        let config = make_config(GeneratorType::Garbage, params);
        let generator: Arc<dyn PayloadGenerator> =
            Arc::from(create_generator(&config, &default_limits()).unwrap());

        // Use std::thread::scope for structured concurrency — all threads
        // are guaranteed to join before the scope exits.
        std::thread::scope(|s| {
            // Collect is needed: s.spawn borrows s, preventing chained iteration.
            #[allow(clippy::needless_collect)]
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let gen_ref = Arc::clone(&generator);
                    s.spawn(move || gen_ref.generate().unwrap().into_bytes())
                })
                .collect();

            let results: Vec<Vec<u8>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

            // Same seed + same config = identical output from both threads
            assert_eq!(
                results[0], results[1],
                "concurrent calls with same seed should produce identical output"
            );
            assert_eq!(results[0].len(), 256);
        });
    }
}
