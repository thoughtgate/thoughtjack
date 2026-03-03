#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::protocol::a2a_client::fuzz_a2a_sse_feed;

fuzz_target!(|data: &[u8]| {
    let _ = fuzz_a2a_sse_feed(data);
});
