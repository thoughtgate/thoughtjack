#![no_main]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use thoughtjack::engine::validate_synthesized_output;

fuzz_target!(|data: &[u8]| {
    // Split input at null byte: protocol string + JSON content
    let split = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (proto_bytes, rest) = data.split_at(split);
    let content_bytes = if rest.is_empty() { rest } else { &rest[1..] };

    let Ok(protocol) = std::str::from_utf8(proto_bytes) else {
        return;
    };
    let Ok(content) = serde_json::from_slice::<Value>(content_bytes) else {
        return;
    };

    // Test without schema
    let _ = validate_synthesized_output(protocol, &content, None);

    // Test with a simple schema
    let schema = serde_json::json!({"type": "object"});
    let _ = validate_synthesized_output(protocol, &content, Some(&schema));
});
