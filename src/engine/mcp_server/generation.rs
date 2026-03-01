use std::fmt::Write as _;

use serde_json::Value;

use super::helpers::u64_to_usize;

/// Maximum nesting depth for generated JSON to prevent stack overflow.
pub(super) const MAX_GENERATION_DEPTH: usize = 1000;

/// Maximum payload size for generated content (50 MB).
pub(super) const MAX_GENERATION_SIZE: usize = 50 * 1024 * 1024;

/// Apply payload generation to content items that have a `generate` block.
///
/// For each content item with a `generate` key, replaces the `text` field
/// with the generated payload and removes the `generate` key.
pub(super) fn apply_generation(content: &mut Value) {
    if let Some(items) = content.get_mut("content").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(generator) = item.get("generate").cloned() {
                let kind = generator.get("kind").and_then(Value::as_str).unwrap_or("");
                let params = generator.get("parameters");
                let seed = generator.get("seed").and_then(Value::as_u64);
                let generated = match kind {
                    "nested_json" => generate_nested_json(params, seed),
                    "random_bytes" => generate_random_bytes(params, seed),
                    "unbounded_line" => generate_unbounded_line(params, seed),
                    "unicode_stress" => generate_unicode_stress(params, seed),
                    _ => {
                        tracing::warn!(kind, "unknown generator kind");
                        continue;
                    }
                };
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("text".to_string(), Value::String(generated));
                    obj.remove("generate");
                }
            }
        }
    }
}

/// Generate deeply nested JSON: `{"a":{"a":...}}` to the specified depth.
pub(super) fn generate_nested_json(params: Option<&Value>, _seed: Option<u64>) -> String {
    let depth = u64_to_usize(
        params
            .and_then(|p| p.get("depth"))
            .and_then(Value::as_u64)
            .unwrap_or(100),
    );
    let clamped_depth = depth.min(MAX_GENERATION_DEPTH);

    let mut result = String::with_capacity(clamped_depth * 6 + 10);
    for _ in 0..clamped_depth {
        result.push_str(r#"{"a":"#);
    }
    result.push_str(r#""leaf""#);
    for _ in 0..clamped_depth {
        result.push('}');
    }
    result
}

/// Generate deterministic pseudo-random bytes, hex-encoded.
///
/// Uses a simple LCG (no `rand` dependency) seeded by the provided seed.
pub(super) fn generate_random_bytes(params: Option<&Value>, seed: Option<u64>) -> String {
    let size = u64_to_usize(
        params
            .and_then(|p| p.get("size"))
            .and_then(Value::as_u64)
            .unwrap_or(1024),
    );
    let clamped_size = size.min(MAX_GENERATION_SIZE);

    // Simple LCG: x = (a * x + c) mod m
    let mut lcg_state = seed.unwrap_or(42);
    let mut hex = String::with_capacity(clamped_size * 2);
    for _ in 0..clamped_size {
        lcg_state = lcg_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        #[allow(clippy::cast_possible_truncation)]
        let byte = (lcg_state >> 33) as u8;
        let _ = write!(hex, "{byte:02x}");
    }

    hex
}

/// Generate an unbounded single-line string of repeated characters.
pub(super) fn generate_unbounded_line(params: Option<&Value>, _seed: Option<u64>) -> String {
    let length = u64_to_usize(
        params
            .and_then(|p| p.get("length"))
            .and_then(Value::as_u64)
            .unwrap_or(1_000_000),
    );
    let clamped_length = length.min(MAX_GENERATION_SIZE);

    let ch = params
        .and_then(|p| p.get("char"))
        .and_then(Value::as_str)
        .and_then(|s| s.chars().next())
        .unwrap_or('A');

    ch.to_string().repeat(clamped_length)
}

/// Generate a Unicode stress-test string with category-based sequences.
///
/// Categories: RTL overrides, zero-width characters, combining marks,
/// emoji sequences, and other edge-case Unicode.
pub(super) fn generate_unicode_stress(params: Option<&Value>, _seed: Option<u64>) -> String {
    let category = params
        .and_then(|p| p.get("category"))
        .and_then(Value::as_str)
        .unwrap_or("mixed");
    let repeat = u64_to_usize(
        params
            .and_then(|p| p.get("repeat"))
            .and_then(Value::as_u64)
            .unwrap_or(100),
    );

    let pattern = match category {
        "rtl" => "\u{202E}\u{200F}\u{202B}\u{2067}", // RTL override, RLM, RLE, RLI
        "zero_width" => "\u{200B}\u{200C}\u{200D}\u{FEFF}", // ZWSP, ZWNJ, ZWJ, BOM
        "combining" => "a\u{0300}\u{0301}\u{0302}\u{0303}\u{0304}", // a + 5 combining marks
        "emoji" => "\u{1F600}\u{200D}\u{1F525}\u{FE0F}\u{20E3}", // emoji + ZWJ + fire + VS16 + keycap
        _ => "\u{202E}\u{200B}a\u{0300}\u{0301}\u{1F600}\u{200D}\u{FEFF}", // mixed
    };

    pattern.repeat(repeat.min(MAX_GENERATION_SIZE / pattern.len()))
}
