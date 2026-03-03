#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::protocol::agui::fuzz_agui_sse_feed;

fuzz_target!(|data: &[u8]| {
    let _ = fuzz_agui_sse_feed(data);
});
