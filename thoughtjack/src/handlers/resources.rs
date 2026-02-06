//! MCP `resources/list` and `resources/read` handlers.
//!
//! Maps resource definitions from the effective state to MCP protocol
//! response format and resolves content values at read time.

use serde_json::json;

use crate::config::schema::GeneratorLimits;
use crate::error::ThoughtJackError;
use crate::handlers::resolve_content;
use crate::phase::EffectiveState;
use crate::transport::jsonrpc::{JsonRpcRequest, JsonRpcResponse, error_codes};

/// Handles `resources/list`.
///
/// Implements: TJ-SPEC-002 F-001
#[must_use]
pub fn handle_list(request: &JsonRpcRequest, effective_state: &EffectiveState) -> JsonRpcResponse {
    let resources: Vec<serde_json::Value> = effective_state
        .resources
        .values()
        .map(|rp| {
            let mut obj = json!({
                "uri": rp.resource.uri,
                "name": rp.resource.name,
            });
            if let Some(ref desc) = rp.resource.description {
                obj["description"] = json!(desc);
            }
            if let Some(ref mime) = rp.resource.mime_type {
                obj["mimeType"] = json!(mime);
            }
            obj
        })
        .collect();

    JsonRpcResponse::success(request.id.clone(), json!({ "resources": resources }))
}

/// Handles `resources/read`.
///
/// Looks up the resource by URI from request params and resolves its
/// content value.
///
/// # Errors
///
/// Returns an error if content resolution fails (generator or file I/O).
///
/// Implements: TJ-SPEC-002 F-001
pub async fn handle_read(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    limits: &GeneratorLimits,
) -> Result<JsonRpcResponse, ThoughtJackError> {
    let uri = request
        .params
        .as_ref()
        .and_then(|p| p.get("uri"))
        .and_then(serde_json::Value::as_str);

    let Some(uri) = uri else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            "missing required parameter: uri",
        ));
    };

    let Some(resource) = effective_state.resources.get(uri) else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("resource not found: {uri}"),
        ));
    };

    let text = if let Some(ref resp) = resource.response {
        resolve_content(&resp.content, limits).await?
    } else {
        String::new()
    };

    let mut content_obj = json!({
        "uri": resource.resource.uri,
        "text": text,
    });
    if let Some(ref mime) = resource.resource.mime_type {
        content_obj["mimeType"] = json!(mime);
    }

    Ok(JsonRpcResponse::success(
        request.id.clone(),
        json!({ "contents": [content_obj] }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ContentValue, ResourceDefinition, ResourcePattern, ResourceResponse,
    };
    use crate::phase::EffectiveState;
    use crate::transport::jsonrpc::JSONRPC_VERSION;
    use indexmap::IndexMap;

    fn make_state_with_resource(uri: &str, name: &str, content: &str) -> EffectiveState {
        let mut resources = IndexMap::new();
        resources.insert(
            uri.to_string(),
            ResourcePattern {
                resource: ResourceDefinition {
                    uri: uri.to_string(),
                    name: name.to_string(),
                    description: Some("A test resource".to_string()),
                    mime_type: Some("text/plain".to_string()),
                },
                response: Some(ResourceResponse {
                    content: ContentValue::Static(content.to_string()),
                }),
                behavior: None,
            },
        );
        EffectiveState {
            tools: IndexMap::new(),
            resources,
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
    fn list_returns_all_resources() {
        let state = make_state_with_resource("file:///test", "Test", "content");
        let req = make_request("resources/list", None);
        let resp = handle_list(&req, &state);
        let result = resp.result.unwrap();
        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "file:///test");
        assert_eq!(resources[0]["name"], "Test");
        assert_eq!(resources[0]["mimeType"], "text/plain");
    }

    #[tokio::test]
    async fn read_returns_resource_content() {
        let state = make_state_with_resource("file:///test", "Test", "hello");
        let req = make_request("resources/read", Some(json!({"uri": "file:///test"})));
        let limits = GeneratorLimits::default();
        let resp = handle_read(&req, &state, &limits).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["contents"][0]["text"], "hello");
        assert_eq!(result["contents"][0]["uri"], "file:///test");
    }

    #[tokio::test]
    async fn read_missing_uri_returns_error() {
        let state = make_state_with_resource("file:///test", "Test", "hello");
        let req = make_request("resources/read", Some(json!({})));
        let limits = GeneratorLimits::default();
        let resp = handle_read(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn read_unknown_resource_returns_error() {
        let state = make_state_with_resource("file:///test", "Test", "hello");
        let req = make_request("resources/read", Some(json!({"uri": "file:///other"})));
        let limits = GeneratorLimits::default();
        let resp = handle_read(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
    }
}
