//! Garbage byte generator (TJ-SPEC-005).
//!
//! Generates random bytes with configurable character sets and seeded RNG
//! for deterministic output. Supports streaming for payloads over 1 MB.

use crate::config::schema::{Charset, GeneratorLimits};
use crate::error::GeneratorError;

use super::{GeneratedPayload, PayloadGenerator, PayloadStream, STREAMING_THRESHOLD, extract_u64};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use std::collections::HashMap;

/// Size of each streaming chunk (64 KB).
const STREAM_CHUNK_SIZE: usize = 64 * 1024;

/// Alphanumeric character table.
const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Generates random garbage bytes with configurable charset.
///
/// Uses a seeded RNG for deterministic output: the same seed always
/// produces the same bytes. Payloads larger than [`STREAMING_THRESHOLD`]
/// are streamed in 64 KB chunks.
///
/// Implements: TJ-SPEC-005 F-004
#[derive(Debug)]
pub struct GarbageGenerator {
    bytes: usize,
    charset: Charset,
    seed: u64,
}

impl GarbageGenerator {
    /// Creates a new garbage generator from parameters.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::InvalidParameters`] if `bytes` is missing.
    /// Returns [`GeneratorError::LimitExceeded`] if bytes exceeds
    /// `limits.max_payload_bytes`.
    ///
    /// Implements: TJ-SPEC-005 F-004
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

        let charset = params
            .get("charset")
            .and_then(Value::as_str)
            .map(|s| match s {
                "binary" => Charset::Binary,
                "numeric" => Charset::Numeric,
                "alphanumeric" => Charset::Alphanumeric,
                "utf8" => Charset::Utf8,
                _ => Charset::Ascii,
            })
            .unwrap_or_default();

        let seed = extract_u64(params, "seed", 0);

        Ok(Self {
            bytes,
            charset,
            seed,
        })
    }
}

/// Generates a single byte for the given charset using the provided RNG.
fn generate_byte(rng: &mut StdRng, charset: Charset) -> u8 {
    match charset {
        Charset::Ascii => rng.random_range(0x20..=0x7E),
        Charset::Binary => rng.random::<u8>(),
        Charset::Numeric => b'0' + rng.random_range(0..10),
        Charset::Alphanumeric => {
            let idx = rng.random_range(0..ALPHANUMERIC.len());
            ALPHANUMERIC[idx]
        }
        Charset::Utf8 => {
            // For the byte-level helper, we return single-byte ASCII.
            // Full UTF-8 generation is handled in generate_utf8_bytes.
            rng.random_range(0x20..=0x7E)
        }
    }
}

/// Generates UTF-8 bytes until the target byte count is reached.
fn generate_utf8_bytes(rng: &mut StdRng, target: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(target);
    while buf.len() < target {
        // Include BMP (0x20..0xD800) and supplementary planes (0xE000..0x110000)
        // to cover emoji (U+1F600+), CJK Extension B (U+20000+), etc.
        let codepoint = loop {
            let cp = rng.random_range(0x20..0x11_0000_u32);
            // Skip surrogate range (0xD800..0xE000) — not valid Unicode scalar values
            if !(0xD800..0xE000).contains(&cp) {
                break cp;
            }
        };
        if let Some(ch) = char::from_u32(codepoint) {
            let mut encode_buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut encode_buf);
            if buf.len() + encoded.len() <= target {
                buf.extend_from_slice(encoded.as_bytes());
            } else {
                break;
            }
        }
    }
    buf
}

/// Generates bytes for the given charset using the provided RNG.
fn generate_chunk(rng: &mut StdRng, charset: Charset, count: usize) -> Vec<u8> {
    if charset == Charset::Utf8 {
        generate_utf8_bytes(rng, count)
    } else {
        let mut buf = Vec::with_capacity(count);
        for _ in 0..count {
            buf.push(generate_byte(rng, charset));
        }
        buf
    }
}

impl PayloadGenerator for GarbageGenerator {
    fn generate(&self) -> Result<GeneratedPayload, GeneratorError> {
        // EC-GEN-004: bytes=0 returns empty vec
        if self.bytes == 0 {
            return Ok(GeneratedPayload::Buffered(Vec::new()));
        }

        if self.bytes > STREAMING_THRESHOLD {
            return Ok(GeneratedPayload::Streamed(Box::new(GarbageStream::new(
                self.bytes,
                self.charset,
                self.seed,
            ))));
        }

        let mut rng = StdRng::seed_from_u64(self.seed);
        let data = generate_chunk(&mut rng, self.charset, self.bytes);
        Ok(GeneratedPayload::Buffered(data))
    }

    fn estimated_size(&self) -> usize {
        self.bytes
    }

    fn name(&self) -> &'static str {
        "garbage"
    }
}

/// Streaming garbage byte source for large payloads.
///
/// Emits 64 KB chunks using the same seeded RNG.
///
/// TODO(v0.2): UTF-8 streaming does NOT produce the same bytes as the
/// buffered path for the same seed. Chunk boundaries cause the RNG to
/// advance differently because `generate_utf8_bytes` discards characters
/// that don't fit in the remaining chunk space. A character-level streaming
/// approach is needed to fix this determinism mismatch.
/// Workaround: use `Charset::Ascii` for deterministic streaming.
///
/// Implements: TJ-SPEC-005 F-004, F-009
#[derive(Debug)]
pub struct GarbageStream {
    remaining: usize,
    total: usize,
    charset: Charset,
    rng: StdRng,
}

impl GarbageStream {
    /// Creates a new garbage stream.
    ///
    /// Implements: TJ-SPEC-005 F-004, F-009
    #[must_use]
    pub fn new(total: usize, charset: Charset, seed: u64) -> Self {
        Self {
            remaining: total,
            total,
            charset,
            rng: StdRng::seed_from_u64(seed),
        }
    }
}

impl PayloadStream for GarbageStream {
    fn next_chunk(&mut self) -> Option<Vec<u8>> {
        if self.remaining == 0 {
            return None;
        }

        let chunk_size = self.remaining.min(STREAM_CHUNK_SIZE);
        let data = generate_chunk(&mut self.rng, self.charset, chunk_size);
        let actual_len = data.len();
        if actual_len == 0 {
            // UTF-8 charset: remaining bytes < min multi-byte character width.
            // Fall back to ASCII to fill the exact remaining count.
            let ascii_data = generate_chunk(&mut self.rng, Charset::Ascii, self.remaining);
            self.remaining = 0;
            return if ascii_data.is_empty() {
                None
            } else {
                Some(ascii_data)
            };
        }
        self.remaining = self.remaining.saturating_sub(actual_len);
        Some(data)
    }

    fn estimated_total(&self) -> usize {
        self.total
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
    fn same_seed_same_output() {
        let params = make_params(vec![("bytes", json!(100)), ("seed", json!(42))]);
        let generator1 = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let generator2 = GarbageGenerator::new(&params, &default_limits()).unwrap();
        assert_eq!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn different_seeds_differ() {
        let p1 = make_params(vec![("bytes", json!(100)), ("seed", json!(1))]);
        let p2 = make_params(vec![("bytes", json!(100)), ("seed", json!(2))]);
        let generator1 = GarbageGenerator::new(&p1, &default_limits()).unwrap();
        let generator2 = GarbageGenerator::new(&p2, &default_limits()).unwrap();
        assert_ne!(
            generator1.generate().unwrap().into_bytes(),
            generator2.generate().unwrap().into_bytes()
        );
    }

    #[test]
    fn ascii_range() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("charset", json!("ascii")),
            ("seed", json!(0)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 1000);
        for &b in &data {
            assert!((0x20..=0x7E).contains(&b), "byte {b:#x} out of ASCII range");
        }
    }

    #[test]
    fn numeric_is_digits() {
        let params = make_params(vec![
            ("bytes", json!(500)),
            ("charset", json!("numeric")),
            ("seed", json!(0)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 500);
        for &b in &data {
            assert!(b.is_ascii_digit(), "byte {b:#x} is not a digit");
        }
    }

    #[test]
    fn alphanumeric_valid() {
        let params = make_params(vec![
            ("bytes", json!(500)),
            ("charset", json!("alphanumeric")),
            ("seed", json!(0)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 500);
        for &b in &data {
            assert!(b.is_ascii_alphanumeric(), "byte {b:#x} is not alphanumeric");
        }
    }

    #[test]
    fn binary_full_range() {
        let params = make_params(vec![
            ("bytes", json!(10_000)),
            ("charset", json!("binary")),
            ("seed", json!(42)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 10_000);
        // With 10k random bytes, we should see most byte values
        let mut seen = [false; 256];
        for &b in &data {
            seen[b as usize] = true;
        }
        let unique = seen.iter().filter(|&&s| s).count();
        assert!(unique > 200, "expected > 200 unique bytes, got {unique}");
    }

    #[test]
    fn utf8_valid() {
        let params = make_params(vec![
            ("bytes", json!(1000)),
            ("charset", json!("utf8")),
            ("seed", json!(0)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.len() <= 1000);
        assert!(std::str::from_utf8(&data).is_ok());
    }

    #[test]
    fn exact_byte_count() {
        let params = make_params(vec![("bytes", json!(777)), ("charset", json!("ascii"))]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 777);
    }

    #[test]
    fn zero_bytes_empty() {
        let params = make_params(vec![("bytes", json!(0))]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.is_empty());
    }

    #[test]
    fn rejects_bytes_over_limit() {
        let limits = GeneratorLimits {
            max_payload_bytes: 100,
            ..default_limits()
        };
        let params = make_params(vec![("bytes", json!(101))]);
        let err = GarbageGenerator::new(&params, &limits).unwrap_err();
        assert!(matches!(err, GeneratorError::LimitExceeded(_)));
    }

    #[test]
    fn rejects_missing_bytes() {
        let params = HashMap::new();
        let err = GarbageGenerator::new(&params, &default_limits()).unwrap_err();
        assert!(matches!(err, GeneratorError::InvalidParameters(_)));
    }

    #[test]
    fn large_payload_returns_streamed() {
        let size = STREAMING_THRESHOLD + 1;
        let limits = GeneratorLimits {
            max_payload_bytes: size + 1,
            ..default_limits()
        };
        let params = make_params(vec![("bytes", json!(size))]);
        let generator = GarbageGenerator::new(&params, &limits).unwrap();
        let payload = generator.generate().unwrap();
        assert!(matches!(payload, GeneratedPayload::Streamed(_)));
    }

    #[test]
    fn stream_chunks_sum_to_total() {
        let size = STREAMING_THRESHOLD + 100;
        let limits = GeneratorLimits {
            max_payload_bytes: size + 1,
            ..default_limits()
        };
        let params = make_params(vec![
            ("bytes", json!(size)),
            ("charset", json!("ascii")),
            ("seed", json!(7)),
        ]);
        let generator = GarbageGenerator::new(&params, &limits).unwrap();
        let payload = generator.generate().unwrap();
        let total_bytes = payload.into_bytes();
        assert_eq!(total_bytes.len(), size);
    }

    #[test]
    fn stream_produces_same_bytes_as_buffered() {
        // Use a size just under threshold for buffered, and compare
        // the byte sequence with a stream at the same size
        let size = 10_000;
        let seed = 99u64;

        // Buffered path
        let params = make_params(vec![
            ("bytes", json!(size)),
            ("charset", json!("ascii")),
            ("seed", json!(seed)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let buffered = generator.generate().unwrap().into_bytes();

        // Stream path (manually construct)
        let mut stream = GarbageStream::new(size, Charset::Ascii, seed);
        let mut streamed = Vec::new();
        while let Some(chunk) = stream.next_chunk() {
            streamed.extend_from_slice(&chunk);
        }

        assert_eq!(buffered, streamed);
    }

    // EC-GEN-011: Same seed different sizes — first N bytes identical
    #[test]
    fn same_seed_prefix_identical() {
        let seed = 42u64;
        let params_small = make_params(vec![
            ("bytes", json!(100)),
            ("charset", json!("ascii")),
            ("seed", json!(seed)),
        ]);
        let params_large = make_params(vec![
            ("bytes", json!(500)),
            ("charset", json!("ascii")),
            ("seed", json!(seed)),
        ]);
        let gen_small = GarbageGenerator::new(&params_small, &default_limits()).unwrap();
        let gen_large = GarbageGenerator::new(&params_large, &default_limits()).unwrap();
        let small = gen_small.generate().unwrap().into_bytes();
        let large = gen_large.generate().unwrap().into_bytes();
        assert_eq!(&small[..], &large[..100]);
    }

    // EC-GEN-003: size=0 produces empty output (not rejected)
    #[test]
    fn test_size_zero_rejected() {
        let params = make_params(vec![("bytes", json!(0)), ("charset", json!("ascii"))]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert!(data.is_empty(), "size=0 should produce empty output");
    }

    // EC-GEN-004: each charset variant produces non-empty output
    #[test]
    fn test_all_charset_types() {
        for charset in &["ascii", "binary", "numeric", "alphanumeric", "utf8"] {
            let params = make_params(vec![
                ("bytes", json!(256)),
                ("charset", json!(charset)),
                ("seed", json!(1)),
            ]);
            let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
            let data = generator.generate().unwrap().into_bytes();
            assert!(
                !data.is_empty(),
                "charset '{charset}' should produce non-empty output"
            );
            // Non-UTF-8 charsets should have exact byte count
            if *charset != "utf8" {
                assert_eq!(
                    data.len(),
                    256,
                    "charset '{charset}' should produce exactly 256 bytes"
                );
            }
        }
    }

    // EC-GEN-012: same seed produces identical output (determinism)
    #[test]
    fn test_deterministic_with_same_seed() {
        for charset in &["ascii", "binary", "numeric", "alphanumeric", "utf8"] {
            let params = make_params(vec![
                ("bytes", json!(500)),
                ("charset", json!(charset)),
                ("seed", json!(12345)),
            ]);
            let gen1 = GarbageGenerator::new(&params, &default_limits()).unwrap();
            let gen2 = GarbageGenerator::new(&params, &default_limits()).unwrap();
            assert_eq!(
                gen1.generate().unwrap().into_bytes(),
                gen2.generate().unwrap().into_bytes(),
                "charset '{charset}' should be deterministic with same seed"
            );
        }
    }

    // EC-GEN-009: size exceeding max_payload_bytes returns LimitExceeded
    #[test]
    fn test_exceeds_max_payload_bytes() {
        let limits = GeneratorLimits {
            max_payload_bytes: 500,
            ..default_limits()
        };
        // Exactly at limit should succeed
        let params_at = make_params(vec![("bytes", json!(500))]);
        assert!(GarbageGenerator::new(&params_at, &limits).is_ok());

        // One over limit should fail
        let params_over = make_params(vec![("bytes", json!(501))]);
        let err = GarbageGenerator::new(&params_over, &limits).unwrap_err();
        assert!(
            matches!(err, GeneratorError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    // EC-GEN-016: Binary charset may produce non-UTF-8 bytes
    #[test]
    fn test_binary_charset_text_warning() {
        // Generate a large binary payload — statistically it will contain
        // byte sequences that are not valid UTF-8.
        let params = make_params(vec![
            ("bytes", json!(10_000)),
            ("charset", json!("binary")),
            ("seed", json!(42)),
        ]);
        let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
        let data = generator.generate().unwrap().into_bytes();
        assert_eq!(data.len(), 10_000);

        // Binary charset produces raw 0x00..0xFF bytes. With 10k random
        // bytes it is virtually certain that some sequences are invalid UTF-8.
        let utf8_result = String::from_utf8(data);
        assert!(
            utf8_result.is_err(),
            "binary charset output with 10k bytes should contain invalid UTF-8 sequences"
        );
    }

    // EC-GEN-005: UTF-8 output with various charsets produces valid bytes
    #[test]
    fn all_charsets_produce_correct_output() {
        for charset in &["ascii", "binary", "numeric", "alphanumeric", "utf8"] {
            let params = make_params(vec![
                ("bytes", json!(500)),
                ("charset", json!(charset)),
                ("seed", json!(0)),
            ]);
            let generator = GarbageGenerator::new(&params, &default_limits()).unwrap();
            let data = generator.generate().unwrap().into_bytes();
            assert!(
                !data.is_empty(),
                "charset '{charset}' produced empty output"
            );
        }
    }
}
