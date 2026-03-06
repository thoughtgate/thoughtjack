#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use thoughtjack::engine::mcp_server::response::dispatch_response;

fuzz_target!(|data: &[u8]| {
    // Split input into 3 sections at null bytes:
    //   request_id JSON | item JSON | context JSON
    let parts: Vec<&[u8]> = data.splitn(3, |&b| b == 0).collect();
    if parts.len() < 2 {
        return;
    }

    let Ok(request_id) = serde_json::from_slice::<Value>(parts[0]) else {
        return;
    };
    let Ok(item) = serde_json::from_slice::<Value>(parts[1]) else {
        return;
    };
    let context = parts
        .get(2)
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .unwrap_or(Value::Null);

    let extractors = HashMap::new();

    // Test without output schema
    let _ = dispatch_response(&request_id, &item, &extractors, &context, None, false, "tools/call");

    // Test with raw_synthesize enabled
    let _ = dispatch_response(&request_id, &item, &extractors, &context, None, true, "tools/call");
});
