//! MCP `prompts/list` and `prompts/get` handlers.
//!
//! Maps prompt definitions from the effective state to MCP protocol
//! response format and resolves content values at get time.

use serde_json::json;

use crate::config::schema::ContentValue;
use crate::dynamic::context::{ItemType, TemplateContext};
use crate::dynamic::sequence::CallTracker;
use crate::error::ThoughtJackError;
use crate::handlers::RequestContext;
use crate::handlers::resolve_content;
use crate::phase::EffectiveState;
use crate::transport::jsonrpc::{JsonRpcRequest, JsonRpcResponse, error_codes};

/// Handles `prompts/list`.
///
/// Implements: TJ-SPEC-002 F-001
#[must_use]
pub fn handle_list(request: &JsonRpcRequest, effective_state: &EffectiveState) -> JsonRpcResponse {
    let prompts_list: Vec<serde_json::Value> = effective_state
        .prompts
        .values()
        .map(|pp| {
            let mut obj = json!({ "name": pp.prompt.name });
            if let Some(ref desc) = pp.prompt.description {
                obj["description"] = json!(desc);
            }
            if let Some(ref args) = pp.prompt.arguments {
                let arguments: Vec<serde_json::Value> = args
                    .iter()
                    .map(|a| {
                        let mut arg = json!({ "name": a.name });
                        if let Some(ref desc) = a.description {
                            arg["description"] = json!(desc);
                        }
                        if let Some(required) = a.required {
                            arg["required"] = json!(required);
                        }
                        arg
                    })
                    .collect();
                obj["arguments"] = json!(arguments);
            }
            obj
        })
        .collect();

    JsonRpcResponse::success(request.id.clone(), json!({ "prompts": prompts_list }))
}

/// Handles `prompts/get`.
///
/// Looks up the prompt by name from request params and resolves each
/// message's content value (with dynamic template/match/sequence/handler
/// support).
///
/// # Errors
///
/// Returns an error if content resolution fails (generator, file I/O,
/// or dynamic handler error).
///
/// Implements: TJ-SPEC-002 F-001, TJ-SPEC-009 F-001
#[allow(clippy::too_many_lines)]
pub async fn handle_get(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    rctx: &RequestContext<'_>,
) -> Result<JsonRpcResponse, ThoughtJackError> {
    let prompt_name = request
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(serde_json::Value::as_str);

    let Some(name) = prompt_name else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            "missing required parameter: name",
        ));
    };

    let Some(prompt) = effective_state.prompts.get(name) else {
        return Ok(JsonRpcResponse::error(
            request.id.clone(),
            error_codes::INVALID_PARAMS,
            format!("prompt not found: {name}"),
        ));
    };

    // Extract arguments for template context
    let args = request
        .params
        .as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Increment call counter
    let tracker_key = CallTracker::make_key(rctx.connection_id, rctx.state_scope, "prompt", name);
    let call_count = rctx.call_tracker.increment(&tracker_key);

    let resp = &prompt.response;

    // Check for dynamic features
    let has_dynamic = resp.match_block.is_some()
        || resp.sequence.is_some()
        || resp.handler.is_some()
        || resp.messages.iter().any(|m| matches!(&m.content, ContentValue::Static(s) if crate::dynamic::template::has_templates(s)));

    let messages = if has_dynamic {
        let template_ctx = TemplateContext {
            args,
            item_name: name.to_string(),
            item_type: ItemType::Prompt,
            call_count,
            phase_name: rctx.phase_name.to_string(),
            phase_index: rctx.phase_index,
            request_id: Some(request.id.clone()),
            request_method: request.method.clone(),
            connection_id: rctx.connection_id,
            resource_name: None,
            resource_mime_type: None,
        };

        let resolved = crate::dynamic::resolve_prompt_content(
            &resp.messages,
            resp.match_block.as_deref(),
            resp.sequence.as_deref(),
            resp.on_exhausted.unwrap_or_default(),
            resp.handler.as_ref(),
            &template_ctx,
            rctx.allow_external_handlers,
            rctx.http_client,
        )
        .await?;

        // Resolve content values (generators/files) in messages
        let mut msgs = Vec::with_capacity(resolved.messages.len());
        for msg in &resolved.messages {
            let text = resolve_content(&msg.content, rctx.limits).await?;
            msgs.push(json!({
                "role": match msg.role {
                    crate::config::schema::Role::User => "user",
                    crate::config::schema::Role::Assistant => "assistant",
                },
                "content": {
                    "type": "text",
                    "text": text,
                }
            }));
        }
        msgs
    } else {
        // Static path — resolve content values
        let mut msgs = Vec::with_capacity(resp.messages.len());
        for msg in &resp.messages {
            let text = resolve_content(&msg.content, rctx.limits).await?;
            msgs.push(json!({
                "role": match msg.role {
                    crate::config::schema::Role::User => "user",
                    crate::config::schema::Role::Assistant => "assistant",
                },
                "content": {
                    "type": "text",
                    "text": text,
                }
            }));
        }
        msgs
    };

    let mut result = json!({ "messages": messages });
    if let Some(ref desc) = prompt.prompt.description {
        result["description"] = json!(desc);
    }

    Ok(JsonRpcResponse::success(request.id.clone(), result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ContentValue, GeneratorLimits, PromptArgument, PromptDefinition, PromptMessage,
        PromptPattern, PromptResponse, Role, StateScope,
    };
    use crate::dynamic::sequence::CallTracker;
    use crate::phase::EffectiveState;
    use crate::transport::jsonrpc::JSONRPC_VERSION;
    use indexmap::IndexMap;

    fn make_state_with_prompt(name: &str, message_text: &str) -> EffectiveState {
        let mut prompts = IndexMap::new();
        prompts.insert(
            name.to_string(),
            PromptPattern {
                prompt: PromptDefinition {
                    name: name.to_string(),
                    description: Some("A test prompt".to_string()),
                    arguments: Some(vec![PromptArgument {
                        name: "code".to_string(),
                        description: Some("Code to review".to_string()),
                        required: Some(true),
                    }]),
                },
                response: PromptResponse {
                    messages: vec![PromptMessage {
                        role: Role::User,
                        content: ContentValue::Static(message_text.to_string()),
                    }],
                    ..Default::default()
                },
                behavior: None,
            },
        );
        EffectiveState {
            tools: IndexMap::new(),
            resources: IndexMap::new(),
            prompts,
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
    fn list_returns_all_prompts() {
        let state = make_state_with_prompt("review", "Review this");
        let req = make_request("prompts/list", None);
        let resp = handle_list(&req, &state);
        let result = resp.result.unwrap();
        let prompts_list = result["prompts"].as_array().unwrap();
        assert_eq!(prompts_list.len(), 1);
        assert_eq!(prompts_list[0]["name"], "review");
        assert_eq!(prompts_list[0]["description"], "A test prompt");
        assert_eq!(prompts_list[0]["arguments"][0]["name"], "code");
        assert_eq!(prompts_list[0]["arguments"][0]["required"], true);
    }

    #[tokio::test]
    async fn get_returns_prompt_messages() {
        let state = make_state_with_prompt("review", "Review this code");
        let req = make_request("prompts/get", Some(json!({"name": "review"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_get(&req, &state, &rctx).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["messages"][0]["role"], "user");
        assert_eq!(result["messages"][0]["content"]["text"], "Review this code");
        assert_eq!(result["description"], "A test prompt");
    }

    #[tokio::test]
    async fn get_missing_name_returns_error() {
        let state = make_state_with_prompt("review", "Review this");
        let req = make_request("prompts/get", Some(json!({})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_get(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn get_unknown_prompt_returns_error() {
        let state = make_state_with_prompt("review", "Review this");
        let req = make_request("prompts/get", Some(json!({"name": "nonexistent"})));
        let limits = GeneratorLimits::default();
        let tracker = CallTracker::new();
        let client = crate::dynamic::handlers::http::create_http_client();
        let rctx = make_rctx(&limits, &tracker, &client);
        let resp = handle_get(&req, &state, &rctx).await.unwrap();
        assert!(resp.error.is_some());
    }
}
