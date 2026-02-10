//! MCP `resources/list` and `resources/read` handlers.
//!
//! Maps resource definitions from the effective state to MCP protocol
//! response format and resolves content values at read time.

use serde_json::json;

use crate::config::schema::ContentValue;
use crate::dynamic::context::{ItemType, TemplateContext};
use crate::dynamic::sequence::CallTracker;
use crate::error::ThoughtJackError;
use crate::handlers::RequestContext;
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
/// content value (with dynamic template/match/sequence/handler support).
///
/// # Errors
///
/// Returns an error if content resolution fails (generator, file I/O,
/// or dynamic handler error).
///
/// Implements: TJ-SPEC-002 F-001, TJ-SPEC-009 F-001
#[allow(clippy::too_many_lines)]
pub async fn handle_read(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    rctx: &RequestContext<'_>,
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

    let Some(ref resp) = resource.response else {
        let mut content_obj = json!({
            "uri": resource.resource.uri,
            "text": "",
        });
        if let Some(ref mime) = resource.resource.mime_type {
            content_obj["mimeType"] = json!(mime);
        }
        return Ok(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "contents": [content_obj] }),
        ));
    };

    // Native MCP-style contents array — return directly without dynamic pipeline
    if let Some(ref entries) = resp.contents {
        let contents: Vec<serde_json::Value> = entries
            .iter()
            .map(|entry| {
                let mut obj = json!({
                    "uri": entry.uri,
                    "text": entry.text.as_deref().unwrap_or(""),
                });
                if let Some(ref mime) = entry.mime_type {
                    obj["mimeType"] = json!(mime);
                }
                obj
            })
            .collect();
        return Ok(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "contents": contents }),
        ));
    }

    // Increment call counter
    let tracker_key = CallTracker::make_key(rctx.connection_id, rctx.state_scope, "resource", uri);
    let call_count = rctx.call_tracker.increment(&tracker_key);

    // Check for dynamic features
    let has_dynamic = resp.match_block.is_some()
        || resp.sequence.is_some()
        || resp.handler.is_some()
        || matches!(&resp.content, ContentValue::Static(s) if crate::dynamic::template::has_templates(s));

    if !has_dynamic {
        let text = resolve_content(&resp.content, rctx.limits).await?;
        let mut content_obj = json!({
            "uri": resource.resource.uri,
            "text": text,
        });
        if let Some(ref mime) = resource.resource.mime_type {
            content_obj["mimeType"] = json!(mime);
        }
        return Ok(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "contents": [content_obj] }),
        ));
    }

    let template_ctx = TemplateContext {
        args: json!({}),
        item_name: uri.to_string(),
        item_type: ItemType::Resource,
        call_count,
        phase_name: rctx.phase_name.to_string(),
        phase_index: rctx.phase_index,
        request_id: Some(request.id.clone()),
        request_method: request.method.clone(),
        connection_id: rctx.connection_id,
        resource_name: Some(resource.resource.name.clone()),
        resource_mime_type: resource.resource.mime_type.clone(),
    };

    // Try resource-specific match contents first (F-002)
    if let Some(match_branches) = resp.match_block.as_deref() {
        if let Some(entries) =
            crate::dynamic::resolve_resource_match_contents(match_branches, &template_ctx)?
        {
            let contents: Vec<serde_json::Value> = entries
                .iter()
                .map(|entry| {
                    let mut obj = json!({
                        "uri": entry.uri,
                        "text": entry.text.as_deref().unwrap_or(""),
                    });
                    if let Some(ref mime) = entry.mime_type {
                        obj["mimeType"] = json!(mime);
                    }
                    obj
                })
                .collect();
            return Ok(JsonRpcResponse::success(
                request.id.clone(),
                json!({ "contents": contents }),
            ));
        }
    }

    // Fall through to tool content pipeline for handler/sequence/content
    let content_item = crate::config::schema::ContentItem::Text {
        text: resp.content.clone(),
    };
    let resolved = crate::dynamic::resolve_tool_content(
        &[content_item],
        resp.match_block.as_deref(),
        resp.sequence.as_deref(),
        resp.on_exhausted.unwrap_or_default(),
        resp.handler.as_ref(),
        &template_ctx,
        rctx.allow_external_handlers,
        rctx.http_client,
    )
    .await?;

    // Extract text from resolved content
    let mut text = String::new();
    for item in &resolved.content {
        if let crate::config::schema::ContentItem::Text {
            text: ContentValue::Static(s),
        } = item
        {
            text.push_str(s);
        }
    }

    // Resolve generators/files if dynamic produced no static text
    if text.is_empty() {
        text = resolve_content(&resp.content, rctx.limits).await?;
    }

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
        ContentValue, GeneratorLimits, ResourceDefinition, ResourcePattern, ResourceResponse,
        StateScope,
    };
    use crate::dynamic::sequence::CallTracker;
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
                    ..Default::default()
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
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_read(&req, &state, &rctx).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["contents"][0]["text"], "hello");
        assert_eq!(result["contents"][0]["uri"], "file:///test");
    }

    #[tokio::test]
    async fn read_missing_uri_returns_error() {
        let state = make_state_with_resource("file:///test", "Test", "hello");
        let req = make_request("resources/read", Some(json!({})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_read(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn read_unknown_resource_returns_error() {
        let state = make_state_with_resource("file:///test", "Test", "hello");
        let req = make_request("resources/read", Some(json!({"uri": "file:///other"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_read(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn read_with_match_contents_returns_resource_entries() {
        use crate::config::schema::{MatchBranchConfig, ResourceContentConfig};

        let mut resources = IndexMap::new();
        resources.insert(
            "file:///secret".to_string(),
            ResourcePattern {
                resource: ResourceDefinition {
                    uri: "file:///secret".to_string(),
                    name: "Secret".to_string(),
                    description: None,
                    mime_type: Some("text/plain".to_string()),
                },
                response: Some(ResourceResponse {
                    content: ContentValue::Static("default".to_string()),
                    match_block: Some(vec![MatchBranchConfig::Default {
                        default: json!(true),
                        content: vec![],
                        sequence: None,
                        on_exhausted: None,
                        handler: None,
                        messages: vec![],
                        contents: Some(vec![ResourceContentConfig {
                            uri: "file:///secret".to_string(),
                            text: Some("injected secret".to_string()),
                            mime_type: Some("text/plain".to_string()),
                        }]),
                    }]),
                    ..Default::default()
                }),
                behavior: None,
            },
        );
        let state = EffectiveState {
            tools: IndexMap::new(),
            resources,
            prompts: IndexMap::new(),
            capabilities: None,
            behavior: None,
        };
        let req = make_request("resources/read", Some(json!({"uri": "file:///secret"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_read(&req, &state, &rctx).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["contents"][0]["uri"], "file:///secret");
        assert_eq!(result["contents"][0]["text"], "injected secret");
        assert_eq!(result["contents"][0]["mimeType"], "text/plain");
    }
}
