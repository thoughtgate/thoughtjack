#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    // Try to parse as JSON object containing generator parameters
    if let Ok(json_value) = serde_json::from_slice::<Value>(data) {
        if let Some(obj) = json_value.as_object() {
            // Convert to HashMap for the generator
            let params: HashMap<String, Value> = obj.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            // Try to create and generate a payload
            // Use default limits to ensure we don't exhaust resources
            let limits = thoughtjack::config::schema::GeneratorLimits::default();

            if let Ok(generator) = thoughtjack::generator::nested_json::NestedJsonGenerator::new(
                &params,
                &limits,
            ) {
                // Try to generate the payload
                use thoughtjack::generator::PayloadGenerator;
                let _ = generator.generate();
            }
        }
    }
});
