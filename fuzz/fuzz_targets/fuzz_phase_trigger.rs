#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::config::schema::Trigger;

fuzz_target!(|data: &[u8]| {
    // Try to parse as JSON and then as a Trigger
    if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(data) {
        // Attempt to deserialize as a Trigger config
        let _: Result<Trigger, _> = serde_json::from_value(json_value);
    }
});
