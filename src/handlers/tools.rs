//! MCP `tools/list` and `tools/call` handlers.
//!
//! Maps tool definitions from the effective state to MCP protocol
//! response format and resolves content values at call time.

use serde_json::json;

use crate::config::schema::ContentItem;
use crate::dynamic::context::{ItemType, TemplateContext};
use crate::dynamic::sequence::CallTracker;
use crate::error::ThoughtJackError;
use crate::handlers::RequestContext;
use crate::handlers::resolve_content;
use crate::phase::EffectiveState;
use crate::transport::jsonrpc::{JsonRpcRequest, JsonRpcResponse, error_codes};

/// Handles `tools/list`.
///
/// Implements: TJ-SPEC-002 F-001
#[must_use]
pub fn handle_list(request: &JsonRpcRequest, effective_state: &EffectiveState) -> JsonRpcResponse {
    let tools: Vec<serde_json::Value> = effective_state
        .tools
        .values()
        .map(|tp| {
            json!({
                "name": tp.tool.name,
                "description": tp.tool.description,
                "inputSchema": tp.tool.input_schema,
            })
        })
        .collect();

    JsonRpcResponse::success(request.id.clone(), json!({ "tools": tools }))
}

/// Handles `tools/call`.
///
/// Looks up the tool by name from request params, resolves each content
/// item in the response (with dynamic template/match/sequence/handler
/// support), and returns the MCP-formatted result.
///
/// # Errors
///
/// Returns an error if content resolution fails (generator, file I/O,
/// or dynamic handler error).
///
/// Implements: TJ-SPEC-002 F-001, TJ-SPEC-009 F-001
pub async fn handle_call(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    rctx: &RequestContext<'_>,
) -> Result<JsonRpcResponse, ThoughtJackError> {
    let tool_name = request
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(serde_json::Value::as_str);

    let Some(name) = tool_name else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            "missing required parameter: name",
        ));
    };

    let Some(tool) = effective_state.tools.get(name) else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("tool not found: {name}"),
        ));
    };

    // Extract arguments for template context
    let args = request
        .params
        .as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Increment call counter and get current count
    let tracker_key = CallTracker::make_key(rctx.connection_id, rctx.state_scope, "tool", name);
    let call_count = rctx.call_tracker.increment(&tracker_key);

    let resp = &tool.response;

    // Check if this response has dynamic features
    let has_dynamic = resp.match_block.is_some()
        || resp.sequence.is_some()
        || resp.handler.is_some()
        || resp.content.iter().any(|c| matches!(c, ContentItem::Text { text: crate::config::schema::ContentValue::Static(s) } if crate::dynamic::template::has_templates(s)));

    if has_dynamic {
        let template_ctx = TemplateContext {
            args,
            item_name: name.to_string(),
            item_type: ItemType::Tool,
            call_count,
            phase_name: rctx.phase_name.to_string(),
            phase_index: rctx.phase_index,
            request_id: Some(request.id.clone()),
            request_method: request.method.clone(),
            connection_id: rctx.connection_id,
            resource_name: None,
            resource_mime_type: None,
        };

        let resolved = crate::dynamic::resolve_tool_content(
            &resp.content,
            resp.match_block.as_deref(),
            resp.sequence.as_deref(),
            resp.on_exhausted.unwrap_or_default(),
            resp.handler.as_ref(),
            &template_ctx,
            rctx.allow_external_handlers,
            rctx.http_client,
        )
        .await?;

        // Resolve content values (generators/files) for dynamic results
        let mut content = Vec::with_capacity(resolved.content.len());
        for item in &resolved.content {
            let resolved_item = resolve_content_item(item, rctx).await?;
            content.push(resolved_item);
        }

        let mut result = json!({ "content": content });
        if let Some(is_error) = resp.is_error {
            result["isError"] = json!(is_error);
        }

        return Ok(JsonRpcResponse::success(request.id.clone(), result));
    }

    // Static path — no dynamic features
    let mut content = Vec::with_capacity(resp.content.len());
    for item in &resp.content {
        let resolved = resolve_content_item(item, rctx).await?;
        content.push(resolved);
    }

    let mut result = json!({ "content": content });
    if let Some(is_error) = resp.is_error {
        result["isError"] = json!(is_error);
    }

    Ok(JsonRpcResponse::success(request.id.clone(), result))
}

/// Resolves a single `ContentItem` to its MCP JSON representation.
async fn resolve_content_item(
    item: &ContentItem,
    rctx: &RequestContext<'_>,
) -> Result<serde_json::Value, ThoughtJackError> {
    match item {
        ContentItem::Text { text } => {
            let resolved = resolve_content(text, rctx.limits).await?;
            Ok(json!({ "type": "text", "text": resolved }))
        }
        ContentItem::Image { mime_type, data } => {
            let resolved_data = if let Some(d) = data {
                resolve_content(d, rctx.limits).await?
            } else {
                String::new()
            };
            Ok(json!({
                "type": "image",
                "mimeType": mime_type,
                "data": resolved_data,
            }))
        }
        ContentItem::Resource { resource } => Ok(json!({
            "type": "resource",
            "resource": {
                "uri": resource.uri,
                "mimeType": resource.mime_type,
                "text": resource.text,
            }
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ContentItem, ContentValue, GeneratorLimits, ResponseConfig, StateScope, ToolDefinition,
        ToolPattern,
    };
    use crate::dynamic::sequence::CallTracker;
    use crate::phase::EffectiveState;
    use crate::transport::jsonrpc::JSONRPC_VERSION;
    use indexmap::IndexMap;

    fn make_state_with_tool(name: &str, text: &str) -> EffectiveState {
        let mut tools = IndexMap::new();
        tools.insert(
            name.to_string(),
            ToolPattern {
                tool: ToolDefinition {
                    name: name.to_string(),
                    description: "Test tool".to_string(),
                    input_schema: json!({"type": "object"}),
                },
                response: ResponseConfig {
                    content: vec![ContentItem::Text {
                        text: ContentValue::Static(text.to_string()),
                    }],
                    is_error: None,
                    ..Default::default()
                },
                behavior: None,
            },
        );
        EffectiveState {
            tools,
            resources: IndexMap::new(),
            prompts: IndexMap::new(),
            capabilities: None,
            behavior: None,
        }
    }

    fn make_request(method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
            id: json!(1),
        }
    }

    const fn make_rctx<'a>(
        limits: &'a GeneratorLimits,
        call_tracker: &'a CallTracker,
        http_client: &'a reqwest::Client,
    ) -> RequestContext<'a> {
        RequestContext {
            limits,
            call_tracker,
            phase_name: "<none>",
            phase_index: -1,
            connection_id: 0,
            allow_external_handlers: false,
            http_client,
            state_scope: StateScope::Global,
        }
    }

    #[test]
    fn list_returns_all_tools() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/list", None);
        let resp = handle_list(&req, &state);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "calc");
        assert_eq!(tools[0]["description"], "Test tool");
    }

    #[tokio::test]
    async fn call_returns_tool_content() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/call", Some(json!({"name": "calc"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_call(&req, &state, &rctx).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "42");
    }

    #[tokio::test]
    async fn call_missing_name_returns_error() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/call", Some(json!({})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_call(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_error() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/call", Some(json!({"name": "nonexistent"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_call(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
    }
}
