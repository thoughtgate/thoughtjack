//! MCP `tools/list` and `tools/call` handlers.
//!
//! Maps tool definitions from the effective state to MCP protocol
//! response format and resolves content values at call time.

use serde_json::json;

use crate::config::schema::{ContentItem, GeneratorLimits};
use crate::error::ThoughtJackError;
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
/// item in the response, and returns the MCP-formatted result.
///
/// # Errors
///
/// Returns an error if content resolution fails (generator or file I/O).
///
/// Implements: TJ-SPEC-002 F-001
pub async fn handle_call(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    limits: &GeneratorLimits,
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

    let mut content = Vec::with_capacity(tool.response.content.len());
    for item in &tool.response.content {
        let resolved = resolve_content_item(item, limits).await?;
        content.push(resolved);
    }

    let mut result = json!({ "content": content });
    if let Some(is_error) = tool.response.is_error {
        result["isError"] = json!(is_error);
    }

    Ok(JsonRpcResponse::success(request.id.clone(), result))
}

/// Resolves a single `ContentItem` to its MCP JSON representation.
async fn resolve_content_item(
    item: &ContentItem,
    limits: &GeneratorLimits,
) -> Result<serde_json::Value, ThoughtJackError> {
    match item {
        ContentItem::Text { text } => {
            let resolved = resolve_content(text, limits).await?;
            Ok(json!({ "type": "text", "text": resolved }))
        }
        ContentItem::Image { mime_type, data } => {
            let resolved_data = if let Some(d) = data {
                resolve_content(d, limits).await?
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
        ContentItem, ContentValue, ResponseConfig, ToolDefinition, ToolPattern,
    };
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
        let resp = handle_call(&req, &state, &limits).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "42");
    }

    #[tokio::test]
    async fn call_missing_name_returns_error() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/call", Some(json!({})));
        let limits = GeneratorLimits::default();
        let resp = handle_call(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_error() {
        let state = make_state_with_tool("calc", "42");
        let req = make_request("tools/call", Some(json!({"name": "nonexistent"})));
        let limits = GeneratorLimits::default();
        let resp = handle_call(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
    }
}
