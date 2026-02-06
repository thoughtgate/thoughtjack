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
/// Emits 64 KB chunks using the same seeded RNG, ensuring the
/// same seed produces the same byte sequence as the buffered path.
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
}
