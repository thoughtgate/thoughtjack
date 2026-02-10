#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::config::loader::ConfigLoader;

fuzz_target!(|data: &[u8]| {
    // Convert bytes to string, ignoring invalid UTF-8
    if let Ok(yaml_str) = std::str::from_utf8(data) {
        // Create a loader with default options
        let mut loader = ConfigLoader::with_defaults();

        // Attempt to load the configuration
        // We don't care about the result, just that it doesn't panic
        let _ = loader.load_from_str(yaml_str);
    }
});
