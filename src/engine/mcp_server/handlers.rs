use std::collections::HashMap;

use serde_json::{Value, json};

use crate::transport::jsonrpc::error_codes;
use crate::transport::{JsonRpcRequest, JsonRpcResponse};

use super::helpers::{find_by_field, find_matching_template, strip_internal_fields};
use super::response::dispatch_response;

/// Default MCP protocol version (MCP 2025-11-25).
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// Handle `initialize` — return server capabilities, `serverInfo`, and `instructions`.
///
/// Protocol version precedence (§4.1, EC-OATF-016):
///   `state.protocol_version` > default `"2025-11-25"`
///
/// `serverInfo` merges `state.server_info` fields over defaults to enable
/// server impersonation attacks (EC-OATF-015).
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_initialize(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let capabilities = state
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| default_capabilities(state));

    // Protocol version: state > default "2025-11-25"
    let protocol_version = state
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    // Build serverInfo from defaults + state.server_info overrides
    let mut server_info = json!({
        "name": "thoughtjack",
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(si) = state.get("server_info").and_then(Value::as_object) {
        let obj = server_info.as_object_mut().unwrap();
        for (key, value) in si {
            obj.insert(key.clone(), value.clone());
        }
    }

    let mut result = json!({
        "protocolVersion": protocol_version,
        "capabilities": capabilities,
        "serverInfo": server_info,
    });

    // Optional instructions (prompt injection attack vector)
    if let Some(instructions) = state.get("instructions") {
        result
            .as_object_mut()
            .unwrap()
            .insert("instructions".to_string(), instructions.clone());
    }

    JsonRpcResponse::success(request.id.clone(), result)
}

/// Derive capabilities from the state's declared tools/resources/prompts/tasks/completions.
///
/// Detects non-empty collections in state and emits the corresponding
/// MCP 2025-11-25 capability structures, including nested `tasks.requests`.
pub fn default_capabilities(state: &Value) -> Value {
    let mut caps = serde_json::Map::new();

    if state
        .get("tools")
        .is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert("tools".to_string(), json!({"listChanged": true}));
    }
    let has_resources = state
        .get("resources")
        .is_some_and(|r| r.as_array().is_some_and(|a| !a.is_empty()));
    let has_templates = state
        .get("resource_templates")
        .is_some_and(|r| r.as_array().is_some_and(|a| !a.is_empty()));
    if has_resources || has_templates {
        caps.insert(
            "resources".to_string(),
            json!({"subscribe": true, "listChanged": true}),
        );
    }
    if state
        .get("prompts")
        .is_some_and(|p| p.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert("prompts".to_string(), json!({"listChanged": true}));
    }
    if state
        .get("logging")
        .is_some_and(|l| l.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert("logging".to_string(), json!({}));
    }
    // Tasks: nested capability structure (MCP 2025-11-25, SEP-1686)
    if state
        .get("tasks")
        .is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty()))
    {
        caps.insert(
            "tasks".to_string(),
            json!({
                "list": {},
                "cancel": {},
                "requests": {
                    "tools": {
                        "call": {}
                    }
                }
            }),
        );
    }
    // Completions capability
    if state.get("completions").is_some() {
        caps.insert("completions".to_string(), json!({}));
    }

    Value::Object(caps)
}

/// Handle `tools/list` — return tool definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_tools_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let tools = state
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| strip_internal_fields(tool, &["responses"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "tools": tools }))
}

/// Handle `resources/list` — return resource definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_resources_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let resources = state
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .map(|r| strip_internal_fields(r, &["responses", "content"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "resources": resources }))
}

/// Handle `resources/read` — dispatch response from resource state.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_resources_read(
    request: &JsonRpcRequest,
    state: &Value,
    extractors: &HashMap<String, String>,
    raw_synthesize: bool,
) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // Try exact match on resources first
    let resource = find_by_field(state, "resources", "uri", uri)
        .or_else(|| find_matching_template(state, uri));

    let Some(resource) = resource else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("resource not found: {uri}"),
        );
    };

    dispatch_response(
        &request.id,
        &resource,
        extractors,
        params,
        None,
        raw_synthesize,
        "resources/read",
    )
}

/// Handle `resources/templates/list` — return resource template definitions.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_resources_templates_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let templates = state
        .get("resource_templates")
        .and_then(Value::as_array)
        .map(|templates| {
            templates
                .iter()
                .map(|t| strip_internal_fields(t, &["responses", "content"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(
        request.id.clone(),
        json!({ "resourceTemplates": templates }),
    )
}

/// Handle `prompts/list` — return prompt definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_prompts_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let prompts = state
        .get("prompts")
        .and_then(Value::as_array)
        .map(|prompts| {
            prompts
                .iter()
                .map(|p| strip_internal_fields(p, &["responses"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "prompts": prompts }))
}

/// Handle `ping` — return empty object.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_ping(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `logging/setLevel` — accept the log level, return empty object.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_logging_set_level(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `resources/subscribe` and `resources/unsubscribe` — no-op accept.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_subscribe(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `completion/complete` — return empty completions.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_completion(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        request.id.clone(),
        json!({
            "completion": {
                "values": [],
                "hasMore": false,
            }
        }),
    )
}

/// Handle unknown methods — return null result per §11.1.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_unknown(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), Value::Null)
}

/// Handle `sampling/createMessage` — receive-only acknowledgement per §4.6.
///
/// The server does not initiate sampling; this simply acknowledges receipt
/// so the event can be used for trigger/extractor evaluation.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_sampling(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `roots/list` — receive-only acknowledgement per §4.6.
///
/// The server does not request roots; returns an empty roots list.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_roots_list(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({ "roots": [] }))
}

/// Handle `elicitation/create` response — acknowledge agent's elicitation response.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_elicitation_response(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `tasks/get` — look up a task by ID in `state["tasks"]`.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_tasks_get(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "taskId", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    JsonRpcResponse::success(
        request.id.clone(),
        strip_internal_fields(&task, &["_internal"]),
    )
}

/// Handle `tasks/result` — return a task's result content by ID.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_tasks_result(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "taskId", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    let result = task.get("result").cloned().unwrap_or(Value::Null);
    JsonRpcResponse::success(request.id.clone(), result)
}

/// Handle `tasks/list` — return all tasks from state, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_tasks_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let tasks = state
        .get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .map(|t| strip_internal_fields(t, &["_internal"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    JsonRpcResponse::success(request.id.clone(), json!({ "tasks": tasks }))
}

/// Handle `tasks/cancel` — return cancelled status for the given task.
///
/// Implements: TJ-SPEC-013 F-001
pub fn handle_tasks_cancel(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(_task) = find_by_field(state, "tasks", "taskId", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    JsonRpcResponse::success(
        request.id.clone(),
        json!({ "taskId": task_id, "status": "cancelled" }),
    )
}
