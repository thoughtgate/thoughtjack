#![no_main]

use libfuzzer_sys::fuzz_target;
use thoughtjack::engine::mcp_server::helpers::matches_uri_template;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Split input at null byte: template + URI
    let split = s.find('\0').unwrap_or(s.len());
    let (template, rest) = s.split_at(split);
    let uri = if rest.is_empty() { rest } else { &rest[1..] };

    let _ = matches_uri_template(template, uri);
});
