#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::loader::load_document;

fuzz_target!(|data: &[u8]| {
    if let Ok(yaml_str) = std::str::from_utf8(data) {
        let _ = load_document(yaml_str);
    }
});
