//! ANSI escape sequence generator (TJ-SPEC-005).
//!
//! Generates ANSI terminal escape sequences for testing terminal
//! rendering security. Supports cursor movement, color, title
//! manipulation, hyperlinks, and screen clear sequences.

use crate::config::schema::{AnsiSequenceType, GeneratorLimits};
use crate::error::GeneratorError;

use super::{GeneratedPayload, PayloadGenerator, extract_u64, extract_usize};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write;

/// Generates ANSI escape sequences for terminal attacks.
///
/// Cycles through the configured sequence types for `count` iterations,
/// using a seeded RNG for random components (cursor positions, colors).
/// Deterministic: same seed always produces the same output.
///
/// Implements: TJ-SPEC-005 F-007
#[derive(Debug)]
pub struct AnsiEscapeGenerator {
    sequences: Vec<AnsiSequenceType>,
    count: usize,
    payload: Option<String>,
    seed: u64,
}

impl AnsiEscapeGenerator {
    /// Creates a new ANSI escape generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::LimitExceeded`] if estimated size exceeds
    /// `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-007
    pub fn new(
        params: &HashMap<String, Value>,
        limits: &GeneratorLimits,
    ) -> Result<Self, GeneratorError> {
        let sequences = params
            .get("sequences")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|name| {
                        let parsed = parse_sequence_type(name);
                        if parsed.is_none() {
                            tracing::warn!(
                                sequence_type = name,
                                "unknown ANSI sequence type, ignoring"
                            );
                        }
                        parsed
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let count = extract_usize(params, "count", 100);

        if count > limits.max_batch_size {
            return Err(GeneratorError::LimitExceeded(format!(
                "count {count} exceeds max_batch_size {}",
                limits.max_batch_size
            )));
        }

        let payload = params
            .get("payload")
            .and_then(Value::as_str)
            .map(String::from);
        let seed = extract_u64(params, "seed", 0);

        let this = Self {
            sequences,
            count,
            payload,
            seed,
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

/// Parses a sequence type string into an `AnsiSequenceType`.
fn parse_sequence_type(s: &str) -> Option<AnsiSequenceType> {
    match s {
        "cursor_move" => Some(AnsiSequenceType::CursorMove),
        "color" => Some(AnsiSequenceType::Color),
        "title" => Some(AnsiSequenceType::Title),
        "hyperlink" => Some(AnsiSequenceType::Hyperlink),
        "clear" => Some(AnsiSequenceType::Clear),
        _ => None,
    }
}

/// Generates a single ANSI escape sequence.
fn generate_sequence(
    seq_type: AnsiSequenceType,
    rng: &mut StdRng,
    payload: Option<&str>,
    buf: &mut String,
) {
    let payload_str = payload.unwrap_or("payload");
    match seq_type {
        AnsiSequenceType::CursorMove => {
            let row = rng.random_range(0..1000);
            let col = rng.random_range(0..1000);
            write!(buf, "\x1B[{row};{col}H").expect("write to string should not fail");
        }
        AnsiSequenceType::Color => {
            let code = rng.random_range(0..=255);
            write!(buf, "\x1B[38;5;{code}m").expect("write to string should not fail");
        }
        AnsiSequenceType::Title => {
            write!(buf, "\x1B]0;{payload_str}\x07").expect("write to string should not fail");
        }
        AnsiSequenceType::Hyperlink => {
            write!(buf, "\x1B]8;;{payload_str}\x1B\\Click Here\x1B]8;;\x1B\\")
                .expect("write to string should not fail");
        }
        AnsiSequenceType::Clear => {
            buf.push_str("\x1B[2J\x1B[H");
        }
    }
}

/// Estimates the byte size of a single sequence.
fn estimate_sequence_size(seq_type: AnsiSequenceType, payload: Option<&str>) -> usize {
    let payload_len = payload.map_or(7, str::len); // "payload" = 7
    match seq_type {
        // \x1B[999;999H = 12 bytes max
        AnsiSequenceType::CursorMove => 12,
        // \x1B[38;5;255m = 13 bytes max
        AnsiSequenceType::Color => 13,
        // \x1B]0;{payload}\x07
        AnsiSequenceType::Title => 4 + payload_len + 1,
        // \x1B]8;;{payload}\x1B\\Click Here\x1B]8;;\x1B\\
        AnsiSequenceType::Hyperlink => 5 + payload_len + 2 + 10 + 5 + 2,
        // \x1B[2J\x1B[H = 7 bytes
        AnsiSequenceType::Clear => 7,
    }
}

impl PayloadGenerator for AnsiEscapeGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        if self.sequences.is_empty() || self.count == 0 {
            return Ok(GeneratedPayload::Buffered(Vec::new()));
        }

        let mut rng = StdRng::seed_from_u64(self.seed);
        let mut output = String::with_capacity(self.estimated_size());

        for i in 0..self.count {
            let seq_type = self.sequences[i % self.sequences.len()];
            generate_sequence(seq_type, &mut rng, self.payload.as_deref(), &mut output);
        }

        Ok(GeneratedPayload::Buffered(output.into_bytes()))
    }

    fn estimated_size(&self) -> usize {
        if self.sequences.is_empty() || self.count == 0 {
            return 0;
        }

        // Estimate based on cycling through sequence types
        let per_cycle_size: usize = self
            .sequences
            .iter()
            .map(|s| estimate_sequence_size(*s, self.payload.as_deref()))
            .sum();

        let full_cycles = self.count / self.sequences.len();
        let remainder = self.count % self.sequences.len();

        let remainder_size: usize = self.sequences[..remainder]
            .iter()
            .map(|s| estimate_sequence_size(*s, self.payload.as_deref()))
            .sum();

        full_cycles * per_cycle_size + remainder_size
    }

    fn name(&self) -> &'static str {
        "ansi_escape"
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
    fn cursor_move_format() {
        let params = make_params(vec![
            ("sequences", json!(["cursor_move"])),
            ("count", json!(5)),
            ("seed", json!(0)),
        ]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // Should contain ESC[ and H
        assert!(s.contains("\x1B["));
        assert!(s.contains('H'));
        // Count semicolons (row;col)
        let h_count = s.matches('H').count();
        assert_eq!(h_count, 5);
    }

    #[test]
    fn title_contains_payload() {
        let params = make_params(vec![
            ("sequences", json!(["title"])),
            ("count", json!(1)),
            ("payload", json!("EVIL TITLE")),
        ]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        assert!(s.contains("EVIL TITLE"));
        assert!(s.contains("\x1B]0;"));
        assert!(s.contains('\x07'));
    }

    #[test]
    fn hyperlink_format() {
        let params = make_params(vec![
            ("sequences", json!(["hyperlink"])),
            ("count", json!(1)),
            ("payload", json!("https://evil.com")),
        ]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        assert!(s.contains("https://evil.com"));
        assert!(s.contains("Click Here"));
        assert!(s.contains("\x1B]8;;"));
    }

    #[test]
    fn clear_sequence() {
        let params = make_params(vec![("sequences", json!(["clear"])), ("count", json!(3))]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        assert_eq!(s.matches("\x1B[2J\x1B[H").count(), 3);
    }

    #[test]
    fn count_correct() {
        let params = make_params(vec![
            ("sequences", json!(["color"])),
            ("count", json!(50)),
            ("seed", json!(0)),
        ]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();
        // Each color sequence contains "38;5;"
        assert_eq!(s.matches("38;5;").count(), 50);
    }

    #[test]
    fn empty_sequences_empty_output() {
        let params = make_params(vec![("sequences", json!([]))]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.is_empty());
    }

    #[test]
    fn zero_count_empty_output() {
        let params = make_params(vec![("sequences", json!(["clear"])), ("count", json!(0))]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.is_empty());
    }

    #[test]
    fn deterministic_output() {
        let params = make_params(vec![
            ("sequences", json!(["cursor_move", "color", "title"])),
            ("count", json!(100)),
            ("payload", json!("test")),
            ("seed", json!(42)),
        ]);
        let generator1 = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let generator2 = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn different_seeds_differ() {
        let p1 = make_params(vec![
            ("sequences", json!(["cursor_move"])),
            ("count", json!(10)),
            ("seed", json!(1)),
        ]);
        let p2 = make_params(vec![
            ("sequences", json!(["cursor_move"])),
            ("count", json!(10)),
            ("seed", json!(2)),
        ]);
        let generator1 = AnsiEscapeGenerator::new(&p1, &default_limits()).unwrap();
        let generator2 = AnsiEscapeGenerator::new(&p2, &default_limits()).unwrap();
        assert_ne!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn mixed_sequence_types_cycle() {
        let params = make_params(vec![
            ("sequences", json!(["cursor_move", "clear"])),
            ("count", json!(4)),
            ("seed", json!(0)),
        ]);
        let generator = AnsiEscapeGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // 4 iterations cycling [cursor_move, clear]: cursor, clear, cursor, clear
        assert_eq!(s.matches("\x1B[2J\x1B[H").count(), 2);
        // cursor_move sequences contain H (not part of clear)
        // Count H that are cursor moves (not part of \x1B[H in clear)
        let cursor_moves = s.matches(';').count(); // row;col pairs
        assert_eq!(cursor_moves, 2);
    }
}
