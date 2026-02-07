//! Nested JSON generator (TJ-SPEC-005).
//!
//! Generates deeply nested JSON structures to test parser stack limits.
//! Uses an **iterative** algorithm to avoid stack overflow at any depth.

use crate::config::schema::{GeneratorLimits, NestedStructure};
use crate::error::GeneratorError;

use super::{GeneratedPayload, PayloadGenerator, extract_string, extract_value, require_usize};
use serde_json::Value;
use std::collections::HashMap;

/// Generates deeply nested JSON structures.
///
/// Supports object nesting (`{"key": {"key": ...}}`), array nesting
/// (`[[...]]`), and mixed nesting (alternating objects and arrays).
///
/// Uses an iterative algorithm — safe at any depth without stack overflow.
///
/// Implements: TJ-SPEC-005 F-002
#[derive(Debug)]
pub struct NestedJsonGenerator {
    depth: usize,
    structure: NestedStructure,
    key: String,
    inner: Value,
}

impl NestedJsonGenerator {
    /// Creates a new nested JSON generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::InvalidParameters`] if `depth` is missing.
    /// Returns [`GeneratorError::LimitExceeded`] if depth exceeds
    /// `limits.max_nest_depth` or estimated size exceeds `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-002
    pub fn new(
        params: &HashMap<String, Value>,
        limits: &GeneratorLimits,
    ) -> Result<Self, GeneratorError> {
        let depth = require_usize(params, "depth")?;

        if depth > limits.max_nest_depth {
            return Err(GeneratorError::LimitExceeded(format!(
                "depth {depth} exceeds max_nest_depth {}",
                limits.max_nest_depth
            )));
        }

        let structure = params
            .get("structure")
            .and_then(Value::as_str)
            .map(|s| match s {
                "array" => NestedStructure::Array,
                "mixed" => NestedStructure::Mixed,
                _ => NestedStructure::Object,
            })
            .unwrap_or_default();

        let key = extract_string(params, "key", "a");
        let inner = extract_value(params, "inner").unwrap_or(Value::Null);

        let this = Self {
            depth,
            structure,
            key,
            inner,
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

    /// Computes the prefix for a given nesting level.
    const fn prefix_for_level(&self, level: usize) -> &[u8] {
        match self.structure {
            NestedStructure::Object => b"", // handled separately
            NestedStructure::Array => b"[",
            NestedStructure::Mixed => {
                if level % 2 == 0 {
                    b"" // object level, handled separately
                } else {
                    b"["
                }
            }
        }
    }

    /// Computes the closing bracket for a given nesting level.
    const fn closing_for_level(&self, level: usize) -> u8 {
        match self.structure {
            NestedStructure::Object => b'}',
            NestedStructure::Array => b']',
            NestedStructure::Mixed => {
                if level % 2 == 0 {
                    b'}'
                } else {
                    b']'
                }
            }
        }
    }

    /// Checks if a given nesting level is an object level.
    const fn is_object_level(&self, level: usize) -> bool {
        match self.structure {
            NestedStructure::Object => true,
            NestedStructure::Array => false,
            NestedStructure::Mixed => level % 2 == 0,
        }
    }
}

impl PayloadGenerator for NestedJsonGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        // EC-GEN-001: depth=0 returns just the inner value
        if self.depth == 0 {
            let inner_bytes = serde_json::to_vec(&self.inner)
                .map_err(|e| GeneratorError::GenerationFailed(e.to_string()))?;
            return Ok(GeneratedPayload::Buffered(inner_bytes));
        }

        let inner_bytes = serde_json::to_vec(&self.inner)
            .map_err(|e| GeneratorError::GenerationFailed(e.to_string()))?;
        let key_json =
            serde_json::to_string(&self.key).expect("string serialization should not fail");

        let mut output = Vec::with_capacity(self.estimated_size());

        // Write depth prefixes
        for level in 0..self.depth {
            if self.is_object_level(level) {
                output.push(b'{');
                output.extend_from_slice(key_json.as_bytes());
                output.push(b':');
            } else {
                let prefix = self.prefix_for_level(level);
                output.extend_from_slice(prefix);
            }
        }

        // Write inner value
        output.extend_from_slice(&inner_bytes);

        // Write closing brackets in reverse order
        for level in (0..self.depth).rev() {
            output.push(self.closing_for_level(level));
        }

        Ok(GeneratedPayload::Buffered(output))
    }

    fn estimated_size(&self) -> usize {
        if self.depth == 0 {
            // Just the inner value
            return serde_json::to_vec(&self.inner).map_or(4, |v| v.len());
        }

        let key_json = serde_json::to_string(&self.key).unwrap_or_default();
        let inner_size = serde_json::to_vec(&self.inner).map_or(4, |v| v.len());

        // Per level: object = `{"key":` + `}` ; array = `[` + `]`
        let object_prefix_len = 1 + key_json.len() + 1; // {key_json:
        let array_prefix_len = 1; // [
        let closing_len = 1; // } or ]

        let prefix_total: usize = (0..self.depth)
            .map(|level| {
                if self.is_object_level(level) {
                    object_prefix_len
                } else {
                    array_prefix_len
                }
            })
            .sum();

        prefix_total + inner_size + (self.depth * closing_len)
    }

    fn name(&self) -> &'static str {
        "nested_json"
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
    fn generates_valid_json_at_depth_100() {
        let params = make_params(vec![("depth", json!(100))]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&payload).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn depth_zero_returns_inner_only() {
        let params = make_params(vec![("depth", json!(0)), ("inner", json!(42))]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let parsed: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed, json!(42));
    }

    #[test]
    fn object_structure() {
        let params = make_params(vec![
            ("depth", json!(3)),
            ("structure", json!("object")),
            ("key", json!("a")),
            ("inner", json!("x")),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        assert_eq!(s, r#"{"a":{"a":{"a":"x"}}}"#);
    }

    #[test]
    fn array_structure() {
        let params = make_params(vec![
            ("depth", json!(3)),
            ("structure", json!("array")),
            ("inner", json!(1)),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        assert_eq!(s, "[[[1]]]");
    }

    #[test]
    fn mixed_structure() {
        let params = make_params(vec![
            ("depth", json!(4)),
            ("structure", json!("mixed")),
            ("key", json!("k")),
            ("inner", json!(null)),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        // depth 4: level 0=obj, 1=arr, 2=obj, 3=arr
        assert_eq!(s, r#"{"k":[{"k":[null]}]}"#);
    }

    #[test]
    fn no_stack_overflow_at_depth_50000() {
        let params = make_params(vec![
            ("depth", json!(50_000)),
            ("structure", json!("array")),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        // Should start with 50000 `[` and end with 50000 `]`
        assert_eq!(payload[0], b'[');
        assert_eq!(payload[payload.len() - 1], b']');
        assert!(payload.len() > 100_000);
    }

    #[test]
    fn deterministic_output() {
        let params = make_params(vec![("depth", json!(10)), ("inner", json!("test"))]);
        let gen1 = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let gen2 = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            gen1.generate().unwrap().into_bytes(),
            gen2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn rejects_depth_over_limit() {
        let limits = GeneratorLimits {
            max_nest_depth: 10,
            ..default_limits()
        };
        let params = make_params(vec![("depth", json!(11))]);
        let err = NestedJsonGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    #[test]
    fn rejects_missing_depth() {
        let params = HashMap::new();
        let err = NestedJsonGenerator::new(&params, &default_limits()).unwrap_err();
        assert!(matches!(err, GeneratorError::InvalidParameters(_)));
    }

    #[test]
    fn default_inner_is_null() {
        let params = make_params(vec![("depth", json!(1))]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        assert_eq!(s, r#"{"a":null}"#);
    }

    #[test]
    fn estimated_size_matches_actual() {
        let params = make_params(vec![
            ("depth", json!(50)),
            ("structure", json!("mixed")),
            ("key", json!("mykey")),
            ("inner", json!({"foo": "bar"})),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let estimated = generator.estimated_size();
        let actual = generator.generate().unwrap().into_bytes().len();
        assert_eq!(estimated, actual);
    }

    // EC-GEN-007: Nested JSON with large inner value
    #[test]
    fn large_inner_value_plus_nesting() {
        let big_inner = "x".repeat(1000);
        let limits = GeneratorLimits {
            max_payload_bytes: 100_000,
            max_nest_depth: 50,
            ..GeneratorLimits::default()
        };
        let params = make_params(vec![
            ("depth", json!(10)),
            ("inner", json!(big_inner)),
        ]);
        let generator = NestedJsonGenerator::new(&params, &limits).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        // Should contain the full inner value
        assert!(s.contains(&big_inner));
        // Should be valid JSON
        let _parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    }

    // EC-GEN-019: Mixed nested structure at odd depth
    #[test]
    fn mixed_structure_odd_depth() {
        let params = make_params(vec![
            ("depth", json!(3)),
            ("structure", json!("mixed")),
            ("key", json!("k")),
            ("inner", json!(true)),
        ]);
        let generator = NestedJsonGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(payload).unwrap();
        // depth 3: level 0=obj, 1=arr, 2=obj
        assert_eq!(s, r#"{"k":[{"k":true}]}"#);
    }

    // EC-GEN-002: Depth exceeds limit
    #[test]
    fn rejects_depth_exceeding_max() {
        let limits = GeneratorLimits {
            max_nest_depth: 100,
            ..GeneratorLimits::default()
        };
        let params = make_params(vec![("depth", json!(101))]);
        let err = NestedJsonGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    // EC-GEN-012: Object nesting vs array nesting size difference
    #[test]
    fn object_larger_than_array_due_to_keys() {
        let params_obj = make_params(vec![
            ("depth", json!(20)),
            ("structure", json!("object")),
        ]);
        let params_arr = make_params(vec![
            ("depth", json!(20)),
            ("structure", json!("array")),
        ]);
        let limits = default_limits();
        let gen_obj = NestedJsonGenerator::new(&params_obj, &limits).unwrap();
        let gen_arr = NestedJsonGenerator::new(&params_arr, &limits).unwrap();

        let size_obj = gen_obj.estimated_size();
        let size_arr = gen_arr.estimated_size();

        assert!(
            size_obj > size_arr,
            "object ({size_obj}) should be larger than array ({size_arr}) due to key overhead"
        );
    }
}
