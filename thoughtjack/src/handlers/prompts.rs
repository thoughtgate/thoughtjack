//! MCP `prompts/list` and `prompts/get` handlers.
//!
//! Maps prompt definitions from the effective state to MCP protocol
//! response format and resolves content values at get time.

use serde_json::json;

use crate::config::schema::GeneratorLimits;
use crate::error::ThoughtJackError;
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
/// message's content value.
///
/// # Errors
///
/// Returns an error if content resolution fails (generator or file I/O).
///
/// Implements: TJ-SPEC-002 F-001
pub async fn handle_get(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    limits: &GeneratorLimits,
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

    let mut messages = Vec::with_capacity(prompt.response.messages.len());
    for msg in &prompt.response.messages {
        let text = resolve_content(&msg.content, limits).await?;
        messages.push(json!({
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
        ContentValue, PromptArgument, PromptDefinition, PromptMessage, PromptPattern,
        PromptResponse, Role,
    };
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
        let resp = handle_get(&req, &state, &limits).await.unwrap();
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
        let resp = handle_get(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn get_unknown_prompt_returns_error() {
        let state = make_state_with_prompt("review", "Review this");
        let req = make_request("prompts/get", Some(json!({"name": "nonexistent"})));
        let limits = GeneratorLimits::default();
        let resp = handle_get(&req, &state, &limits).await.unwrap();
        assert!(resp.error.is_some());
    }
}
