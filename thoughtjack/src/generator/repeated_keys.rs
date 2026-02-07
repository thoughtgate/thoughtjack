//! Repeated keys generator (TJ-SPEC-005).
//!
//! Generates JSON objects with duplicate keys to trigger hash collision
//! attacks in JSON parsers. Builds raw JSON manually since `serde_json::Map`
//! deduplicates keys.

use crate::config::schema::GeneratorLimits;
use crate::error::GeneratorError;

use super::{GeneratedPayload, PayloadGenerator, extract_usize, extract_value};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write;

/// Generates JSON objects with duplicate keys to trigger hash collision
/// attacks in JSON parsers.
///
/// All entries use the same key (padded to `key_length`), producing raw
/// JSON like `{"kkkkkkkk":"x","kkkkkkkk":"x",...}`. Since standard JSON
/// libraries deduplicate keys, the JSON is built manually as a raw string.
///
/// Output is fully deterministic.
///
/// Implements: TJ-SPEC-005 F-005
#[derive(Debug)]
pub struct RepeatedKeysGenerator {
    count: usize,
    /// The single key string repeated for all entries (e.g., `"kkkkkkkk"`)
    key: String,
    value_json: String,
}

impl RepeatedKeysGenerator {
    /// Creates a new repeated keys generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::InvalidParameters`] if `count` is missing.
    /// Returns [`GeneratorError::LimitExceeded`] if count exceeds
    /// `limits.max_batch_size` or estimated size exceeds `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-005
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

        let key_length = extract_usize(params, "key_length", 8).max(1);
        let value =
            extract_value(params, "value").unwrap_or_else(|| Value::String("x".to_string()));
        let value_json = serde_json::to_string(&value)
            .map_err(|e| GeneratorError::GenerationFailed(e.to_string()))?;

        // Build the single key string: 'k' padded to key_length
        let key = "k".repeat(key_length);

        let this = Self {
            count,
            key,
            value_json,
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

impl PayloadGenerator for RepeatedKeysGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        if self.count == 0 {
            return Ok(GeneratedPayload::Buffered(b"{}".to_vec()));
        }

        let mut output = String::with_capacity(self.estimated_size());
        output.push('{');

        for i in 0..self.count {
            if i > 0 {
                output.push(',');
            }
            // Write "kkkkkkkk":value_json — same key for all entries
            write!(
                output,
                "\"{key}\":{value}",
                key = self.key,
                value = self.value_json
            )
            .expect("string write should not fail");
        }

        output.push('}');

        Ok(GeneratedPayload::Buffered(output.into_bytes()))
    }

    fn estimated_size(&self) -> usize {
        if self.count == 0 {
            return 2; // "{}"
        }

        // Each entry: "kkkkkkkk":value_json
        // Key part: 1 (") + key.len() + 1 (") + 1 (:) = key.len() + 3
        // Entry: key.len() + 3 + value_json.len()
        let entry_size = self.key.len() + 3 + self.value_json.len();

        // Total: { + entries + commas + }
        // = 2 + count * entry_size + (count - 1)
        2 + self.count * entry_size + self.count.saturating_sub(1)
    }

    fn name(&self) -> &'static str {
        "repeated_keys"
    }

    fn produces_json(&self) -> bool {
        true
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
    fn all_keys_are_duplicates() {
        let params = make_params(vec![("count", json!(100))]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // All 100 entries use the same key "kkkkkkkk" (default key_length=8)
        let key = "\"kkkkkkkk\"";
        let occurrences = s.matches(key).count();
        assert_eq!(
            occurrences, 100,
            "expected 100 duplicate keys, got {occurrences}"
        );
    }

    #[test]
    fn key_length_correct() {
        let params = make_params(vec![("count", json!(5)), ("key_length", json!(12))]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // Each key should be 12 chars: kkkkkkkkkkkk
        let key = "\"kkkkkkkkkkkk\"";
        let occurrences = s.matches(key).count();
        assert_eq!(occurrences, 5, "expected 5 keys of length 12");
    }

    #[test]
    fn custom_value() {
        let params = make_params(vec![("count", json!(2)), ("value", json!(42))]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        assert!(s.contains(":42"));
    }

    #[test]
    fn zero_count_empty_object() {
        let params = make_params(vec![("count", json!(0))]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data, b"{}");
    }

    #[test]
    fn deterministic_output() {
        let params = make_params(vec![("count", json!(50))]);
        let generator1 = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let generator2 = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn rejects_count_over_limit() {
        let limits = GeneratorLimits {
            max_batch_size: 10,
            ..default_limits()
        };
        let params = make_params(vec![("count", json!(11))]);
        let err = RepeatedKeysGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    #[test]
    fn estimated_size_matches_actual() {
        let params = make_params(vec![
            ("count", json!(100)),
            ("key_length", json!(10)),
            ("value", json!({"nested": true})),
        ]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let estimated = generator.estimated_size();
        let actual = generator.generate().unwrap().into_bytes().len();
        assert_eq!(estimated, actual);
    }

    #[test]
    fn default_value_is_x() {
        let params = make_params(vec![("count", json!(1))]);
        let generator = RepeatedKeysGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        assert!(s.contains(r#":"x""#));
    }
}
