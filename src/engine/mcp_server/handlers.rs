use std::collections::HashMap;

use serde_json::{Value, json};

use crate::transport::jsonrpc::error_codes;
use crate::transport::{JsonRpcRequest, JsonRpcResponse};

use super::helpers::{find_by_field, find_matching_template, strip_internal_fields};
use super::response::dispatch_response;

/// Handle `initialize` — return server capabilities.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_initialize(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let capabilities = state
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| default_capabilities(state));

    JsonRpcResponse::success(
        request.id.clone(),
        json!({
            "protocolVersion": "2025-03-26",
            "capabilities": capabilities,
            "serverInfo": {
                "name": "thoughtjack",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )
}

/// Derive capabilities from the state's declared tools/resources/prompts.
pub(super) fn default_capabilities(state: &Value) -> Value {
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

    Value::Object(caps)
}

/// Handle `tools/list` — return tool definitions, stripping internal fields.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_tools_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
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
pub(super) fn handle_resources_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
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
pub(super) fn handle_resources_read(
    request: &JsonRpcRequest,
    state: &Value,
    extractors: &HashMap<String, String>,
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

    dispatch_response(&request.id, &resource, extractors, params, None, false)
}

/// Handle `resources/templates/list` — return resource template definitions.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_resources_templates_list(
    request: &JsonRpcRequest,
    state: &Value,
) -> JsonRpcResponse {
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
pub(super) fn handle_prompts_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
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
pub(super) fn handle_ping(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `logging/setLevel` — accept the log level, return empty object.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_logging_set_level(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `resources/subscribe` and `resources/unsubscribe` — no-op accept.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_subscribe(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `completion/complete` — return empty completions.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_completion(request: &JsonRpcRequest) -> JsonRpcResponse {
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
pub(super) fn handle_unknown(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), Value::Null)
}

/// Handle `sampling/createMessage` — receive-only acknowledgement per §4.6.
///
/// The server does not initiate sampling; this simply acknowledges receipt
/// so the event can be used for trigger/extractor evaluation.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_sampling(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `roots/list` — receive-only acknowledgement per §4.6.
///
/// The server does not request roots; returns an empty roots list.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_roots_list(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({ "roots": [] }))
}

/// Handle `elicitation/create` response — acknowledge agent's elicitation response.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_elicitation_response(request: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(request.id.clone(), json!({}))
}

/// Handle `tasks/get` — look up a task by ID in `state["tasks"]`.
///
/// Implements: TJ-SPEC-013 F-001
pub(super) fn handle_tasks_get(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "id", task_id) else {
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
pub(super) fn handle_tasks_result(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(task) = find_by_field(state, "tasks", "id", task_id) else {
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
pub(super) fn handle_tasks_list(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
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
pub(super) fn handle_tasks_cancel(request: &JsonRpcRequest, state: &Value) -> JsonRpcResponse {
    let params = request.params.as_ref().unwrap_or(&Value::Null);
    let task_id = params.get("id").and_then(Value::as_str).unwrap_or_default();

    let Some(_task) = find_by_field(state, "tasks", "id", task_id) else {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("task not found: {task_id}"),
        );
    };

    JsonRpcResponse::success(
        request.id.clone(),
        json!({ "id": task_id, "status": "cancelled" }),
    )
}
