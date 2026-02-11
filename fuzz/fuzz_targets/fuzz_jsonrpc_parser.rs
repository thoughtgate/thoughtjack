#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse as JSON-RPC message
    // We don't care about the result, just that it doesn't panic
    let _: Result<thoughtjack::transport::jsonrpc::JsonRpcMessage, _> =
        serde_json::from_slice(data);
});
