//! Unicode spam generator (TJ-SPEC-005).
//!
//! Generates Unicode attack sequences from five categories:
//! zero-width, homoglyph, combining, RTL override, and emoji.
//! Supports carrier text interleaving and seeded RNG for determinism.

use crate::config::schema::{GeneratorLimits, UnicodeCategory};
use crate::error::GeneratorError;

use super::{GeneratedPayload, PayloadGenerator, extract_u64};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use std::collections::HashMap;

// ============================================================================
// Codepoint tables (TJ-SPEC-005 Appendix C)
// ============================================================================

/// Zero-width characters.
const ZERO_WIDTH: &[char] = &[
    '\u{200B}', // ZERO WIDTH SPACE
    '\u{200C}', // ZERO WIDTH NON-JOINER
    '\u{200D}', // ZERO WIDTH JOINER
    '\u{FEFF}', // ZERO WIDTH NO-BREAK SPACE (BOM)
    '\u{2060}', // WORD JOINER
];

/// Homoglyph characters (Cyrillic lookalikes for Latin).
const HOMOGLYPH: &[char] = &[
    '\u{0430}', // а (Cyrillic)
    '\u{0435}', // е (Cyrillic)
    '\u{043E}', // о (Cyrillic)
    '\u{0440}', // р (Cyrillic)
    '\u{0441}', // с (Cyrillic)
    '\u{0445}', // х (Cyrillic)
];

/// Combining characters (diacritics for zalgo text).
const COMBINING: &[char] = &[
    '\u{0300}', // COMBINING GRAVE ACCENT
    '\u{0301}', // COMBINING ACUTE ACCENT
    '\u{0302}', // COMBINING CIRCUMFLEX ACCENT
    '\u{0303}', // COMBINING TILDE
    '\u{0304}', // COMBINING MACRON
    '\u{0305}', // COMBINING OVERLINE
    '\u{030A}', // COMBINING RING ABOVE
    '\u{030B}', // COMBINING DOUBLE ACUTE ACCENT
    '\u{0327}', // COMBINING CEDILLA
    '\u{0328}', // COMBINING OGONEK
];

/// RTL override characters.
const RTL: &[char] = &[
    '\u{200F}', // RIGHT-TO-LEFT MARK
    '\u{202B}', // RIGHT-TO-LEFT EMBEDDING
    '\u{202E}', // RIGHT-TO-LEFT OVERRIDE
    '\u{2067}', // RIGHT-TO-LEFT ISOLATE
];

/// Emoji characters.
const EMOJI: &[char] = &[
    '\u{1F525}', // 🔥
    '\u{1F4A9}', // 💩
    '\u{1F680}', // 🚀
    '\u{2764}',  // ❤
    '\u{1F600}', // 😀
];

/// Returns the codepoint table for the given unicode category.
const fn codepoints_for_category(category: UnicodeCategory) -> &'static [char] {
    match category {
        UnicodeCategory::ZeroWidth => ZERO_WIDTH,
        UnicodeCategory::Homoglyph => HOMOGLYPH,
        UnicodeCategory::Combining => COMBINING,
        UnicodeCategory::Rtl => RTL,
        UnicodeCategory::Emoji => EMOJI,
    }
}

// ============================================================================
// Generator
// ============================================================================

/// Generates Unicode attack sequences.
///
/// Produces bytes filled with attack characters from the selected category.
/// When a carrier string is provided, attack characters are interleaved
/// with carrier text (30% carrier probability).
///
/// Output may be slightly under the `bytes` target due to multi-byte
/// UTF-8 alignment — this is expected behavior.
///
/// Implements: TJ-SPEC-005 F-006
#[derive(Debug)]
pub struct UnicodeSpamGenerator {
    bytes: usize,
    category: UnicodeCategory,
    carrier: Option<String>,
    seed: u64,
}

impl UnicodeSpamGenerator {
    /// Creates a new unicode spam generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::InvalidParameters`] if `bytes` is missing.
    /// Returns [`GeneratorError::LimitExceeded`] if bytes exceeds
    /// `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-006
    pub fn new(
        params: &HashMap<String, Value>,
        limits: &GeneratorLimits,
    ) -> Result<Self, GeneratorError> {
        let bytes = super::require_usize(params, "bytes")?;

        if bytes > limits.max_payload_bytes {
            return Err(GeneratorError::LimitExceeded(format!(
                "bytes {bytes} exceeds max_payload_bytes {}",
                limits.max_payload_bytes
            )));
        }

        let category = params
            .get("category")
            .and_then(Value::as_str)
            .map(|s| match s {
                "homoglyph" => UnicodeCategory::Homoglyph,
                "combining" => UnicodeCategory::Combining,
                "rtl" => UnicodeCategory::Rtl,
                "emoji" => UnicodeCategory::Emoji,
                _ => UnicodeCategory::ZeroWidth,
            })
            .unwrap_or_default();

        let carrier = params
            .get("carrier")
            .and_then(Value::as_str)
            .map(String::from);

        let seed = extract_u64(params, "seed", 0);

        Ok(Self {
            bytes,
            category,
            carrier,
            seed,
        })
    }
}

impl PayloadGenerator for UnicodeSpamGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        if self.bytes == 0 {
            return Ok(GeneratedPayload::Buffered(Vec::new()));
        }

        let codepoints = codepoints_for_category(self.category);
        let mut rng = StdRng::seed_from_u64(self.seed);
        let mut buf = Vec::with_capacity(self.bytes);
        let mut encode_buf = [0u8; 4];

        let carrier_chars: Vec<char> = self.carrier.as_deref().unwrap_or("").chars().collect();
        let has_carrier = !carrier_chars.is_empty();

        while buf.len() < self.bytes {
            let ch = if has_carrier && rng.random_bool(0.3) {
                // Emit a carrier character
                let idx = rng.random_range(0..carrier_chars.len());
                carrier_chars[idx]
            } else {
                // Emit an attack character
                let idx = rng.random_range(0..codepoints.len());
                codepoints[idx]
            };

            let encoded = ch.encode_utf8(&mut encode_buf);
            if buf.len() + encoded.len() > self.bytes {
                break;
            }
            buf.extend_from_slice(encoded.as_bytes());
        }

        Ok(GeneratedPayload::Buffered(buf))
    }

    fn estimated_size(&self) -> usize {
        self.bytes
    }

    fn name(&self) -> &'static str {
        "unicode_spam"
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
    fn zero_width_chars_present() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("category", json!("zero_width")),
            ("seed", json!(0)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // Should contain at least one zero-width character
        let has_zw = s.contains('\u{200B}')
            || s.contains('\u{200C}')
            || s.contains('\u{200D}')
            || s.contains('\u{FEFF}')
            || s.contains('\u{2060}');
        assert!(has_zw, "expected zero-width characters in output");
    }

    #[test]
    fn combining_chars_present() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("category", json!("combining")),
            ("seed", json!(0)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        let has_combining = s.contains('\u{0300}')
            || s.contains('\u{0301}')
            || s.contains('\u{0302}')
            || s.contains('\u{0327}');
        assert!(has_combining, "expected combining characters in output");
    }

    #[test]
    fn carrier_text_interleaved() {
        let params = make_params(vec![
            ("bytes", json!(5000)),
            ("category", json!("zero_width")),
            ("carrier", json!("Hello World")),
            ("seed", json!(42)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        // With 30% probability and 5000 bytes, carrier chars should appear
        let has_carrier = "Hello World".chars().any(|c| s.contains(c));
        assert!(has_carrier, "expected carrier characters in output");
    }

    #[test]
    fn approximate_byte_count() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("category", json!("zero_width")),
            ("seed", json!(0)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        // May be slightly under target due to multi-byte alignment
        assert!(
            data.len() <= 1000,
            "output {} exceeds target 1000",
            data.len()
        );
        assert!(
            data.len() > 900,
            "output {} is too far under target 1000",
            data.len()
        );
    }

    #[test]
    fn deterministic_with_seed() {
        let params = make_params(vec![
            ("bytes", json!(500)),
            ("category", json!("emoji")),
            ("seed", json!(42)),
        ]);
        let generator1 = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let generator2 = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn different_seeds_differ() {
        let p1 = make_params(vec![
            ("bytes", json!(500)),
            ("category", json!("emoji")),
            ("seed", json!(1)),
        ]);
        let p2 = make_params(vec![
            ("bytes", json!(500)),
            ("category", json!("emoji")),
            ("seed", json!(2)),
        ]);
        let generator1 = UnicodeSpamGenerator::new(&p1, &default_limits()).unwrap();
        let generator2 = UnicodeSpamGenerator::new(&p2, &default_limits()).unwrap();
        assert_ne!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn zero_bytes_empty() {
        let params = make_params(vec![("bytes", json!(0))]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.is_empty());
    }

    #[test]
    fn valid_utf8_output() {
        for category in &["zero_width", "homoglyph", "combining", "rtl", "emoji"] {
            let params = make_params(vec![
                ("bytes", json!(1000)),
                ("category", json!(category)),
                ("seed", json!(0)),
            ]);
            let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
            let data = generator.generate().unwrap().into_bytes();
            assert!(
                std::str::from_utf8(&data).is_ok(),
                "invalid UTF-8 for category {category}"
            );
        }
    }

    #[test]
    fn rejects_bytes_over_limit() {
        let limits = GeneratorLimits {
            max_payload_bytes: 100,
            ..default_limits()
        };
        let params = make_params(vec![("bytes", json!(101))]);
        let err = UnicodeSpamGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    #[test]
    fn emoji_category() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("category", json!("emoji")),
            ("seed", json!(0)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        let s = String::from_utf8(data).unwrap();

        let has_emoji = s.contains('\u{1F525}')
            || s.contains('\u{1F4A9}')
            || s.contains('\u{1F680}')
            || s.contains('\u{2764}')
            || s.contains('\u{1F600}');
        assert!(has_emoji, "expected emoji characters in output");
    }

    // EC-GEN-014: Unicode spam with empty carrier (pure unicode, no carrier text)
    #[test]
    fn empty_carrier_produces_output() {
        let params = make_params(vec![
            ("bytes", json!(200)),
            ("category", json!("rtl")),
            ("carrier", json!("")),
            ("seed", json!(42)),
        ]);
        let generator = UnicodeSpamGenerator::new(&params, &default_limits()).unwrap();
        let payload = generator.generate().unwrap();
        let data = payload.into_bytes();
        assert!(
            !data.is_empty(),
            "empty carrier should still produce output"
        );
        // Should be valid UTF-8
        String::from_utf8(data).expect("output should be valid UTF-8");
    }
}
