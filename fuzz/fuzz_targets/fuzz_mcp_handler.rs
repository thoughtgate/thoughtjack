#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use thoughtjack::engine::mcp_server::handlers;
use thoughtjack::transport::JsonRpcRequest;

fuzz_target!(|data: &[u8]| {
    // Split input at null byte: request JSON + state JSON
    let split = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (req_bytes, rest) = data.split_at(split);
    let state_bytes = if rest.is_empty() { rest } else { &rest[1..] };

    let Ok(request) = serde_json::from_slice::<JsonRpcRequest>(req_bytes) else {
        return;
    };
    let Ok(state) = serde_json::from_slice::<Value>(state_bytes) else {
        return;
    };

    let _ = match request.method.as_str() {
        // Handlers that take (request, state)
        "initialize" => handlers::handle_initialize(&request, &state),
        "tools/list" => handlers::handle_tools_list(&request, &state),
        "resources/list" => handlers::handle_resources_list(&request, &state),
        "resources/templates/list" => handlers::handle_resources_templates_list(&request, &state),
        "resources/read" => {
            handlers::handle_resources_read(&request, &state, &HashMap::new(), false)
        }
        "prompts/list" => handlers::handle_prompts_list(&request, &state),
        "tasks/list" => handlers::handle_tasks_list(&request, &state),
        "tasks/get" => handlers::handle_tasks_get(&request, &state),
        "tasks/result" => handlers::handle_tasks_result(&request, &state),
        "tasks/cancel" => handlers::handle_tasks_cancel(&request, &state),

        // Handlers that take (request) only
        "ping" => handlers::handle_ping(&request),
        "logging/setLevel" => handlers::handle_logging_set_level(&request),
        "resources/subscribe" => handlers::handle_subscribe(&request),
        "completion/complete" => handlers::handle_completion(&request),
        "sampling/createMessage" => handlers::handle_sampling(&request),
        "roots/list" => handlers::handle_roots_list(&request),
        "elicitation/response" => handlers::handle_elicitation_response(&request),

        _ => handlers::handle_unknown(&request),
    };
});
