use serde_json::{Value, json};

use crate::error::EngineError;
use crate::transport::{JsonRpcMessage, JsonRpcResponse};

use super::types::ChatMessage;

/// Extracts conversation messages from a `RunAgentInput` JSON-RPC message.
///
/// Maps each message by role: `system` → `System`, `user` → `User`,
/// `assistant` → `AssistantText`. Returns `Err` if the message is malformed.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the message lacks `params` or the
/// `messages` array within params.
///
/// Implements: TJ-SPEC-022 F-001
pub fn extract_run_agent_input_messages(
    msg: &JsonRpcMessage,
) -> Result<Vec<ChatMessage>, EngineError> {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        _ => None,
    };
    let params =
        params.ok_or_else(|| EngineError::Driver("initial AG-UI message missing params".into()))?;

    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EngineError::Driver("initial AG-UI message missing 'messages' array in params".into())
        })?;

    let mut result = Vec::with_capacity(messages.len());
    for (idx, entry) in messages.iter().enumerate() {
        let role = entry.get("role").and_then(Value::as_str).ok_or_else(|| {
            EngineError::Driver(format!(
                "message at index {idx} missing or non-string 'role' field"
            ))
        })?;
        let content = entry
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                EngineError::Driver(format!(
                    "message at index {idx} (role={role}) missing or non-string 'content' field"
                ))
            })?;
        match role {
            "system" => result.push(ChatMessage::System(content.to_string())),
            "assistant" => result.push(ChatMessage::AssistantText(content.to_string())),
            "user" => result.push(ChatMessage::User(content.to_string())),
            other => {
                return Err(EngineError::Driver(format!(
                    "message at index {idx} has unrecognized role '{other}'"
                )));
            }
        }
    }
    Ok(result)
}

/// Extracts the `context` array from a `RunAgentInput` message and formats
/// it as text suitable for injection as a system message.
///
/// AG-UI's `RunAgentInput` can carry key-value context items (e.g., user
/// preferences, state overrides). This function serializes them so the LLM
/// can see injected state — enabling state-injection attack scenarios like
/// OATF-017.
///
/// Returns `None` if no context items are present.
///
/// Implements: TJ-SPEC-022 F-024
#[must_use]
pub fn extract_run_agent_input_context(msg: &JsonRpcMessage) -> Option<String> {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        _ => None,
    }?;
    let context = params.get("context").and_then(Value::as_array)?;
    if context.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(context.len() + 1);
    lines.push("[Agent Run Context]".to_string());
    for entry in context {
        let key = entry
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let value = &entry["value"];
        let formatted = value.as_str().map_or_else(
            || serde_json::to_string_pretty(value).unwrap_or_default(),
            ToString::to_string,
        );
        lines.push(format!("{key}: {formatted}"));
    }
    Some(lines.join("\n"))
}

/// Extracts the last user message from a follow-up `RunAgentInput`.
///
/// Append-only invariant: only the last user message is extracted from
/// follow-up turns. The drive loop owns the full conversation history.
///
/// Implements: TJ-SPEC-022 F-001
pub fn extract_user_message(msg: &JsonRpcMessage) -> String {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        _ => None,
    };
    if let Some(params) = params
        && let Some(messages) = params.get("messages").and_then(Value::as_array)
    {
        // Find last message with role "user"
        for entry in messages.iter().rev() {
            let role = entry.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "user"
                && let Some(content) = entry.get("content").and_then(Value::as_str)
            {
                return content.to_string();
            }
        }
    }
    // Fallback: serialize entire params
    tracing::warn!("could not extract user message from follow-up, using serialized params");
    params
        .map(|p| serde_json::to_string(p).unwrap_or_default())
        .unwrap_or_default()
}

/// Formats a server-initiated request as a user message for history injection.
///
/// Implements: TJ-SPEC-022 F-001
pub fn format_server_request_as_user_message(method: &str, params: &Option<Value>) -> String {
    let params_str = params
        .as_ref()
        .map(|p| serde_json::to_string(p).unwrap_or_default())
        .unwrap_or_default();
    match method {
        "elicitation/create" => {
            let message = params
                .as_ref()
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(&params_str);
            format!("[Server elicitation] {message}")
        }
        "sampling/createMessage" => {
            format!("[Server sampling request] {params_str}")
        }
        _ => format!("[Server request: {method}] {params_str}"),
    }
}

/// Extracts result content from a `JsonRpcMessage`, handling both success and error.
///
/// For MCP responses: returns `result` field as-is or `error` object.
/// For A2A responses: normalizes parts array into a single string.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_result_content(response: &JsonRpcMessage) -> Value {
    match response {
        JsonRpcMessage::Response(resp) => {
            if let Some(ref error) = resp.error {
                // Preserve error object as-is (including data field)
                let mut err_obj = json!({
                    "code": error.code,
                    "message": error.message,
                });
                if let Some(ref data) = error.data {
                    err_obj["data"] = data.clone();
                }
                json!({ "error": err_obj })
            } else if let Some(ref result) = resp.result {
                // Check for A2A response format (has message.parts)
                if let Some(message) = result.get("message")
                    && let Some(parts) = message.get("parts").and_then(Value::as_array)
                {
                    return normalize_a2a_parts(parts);
                }
                // MCP: return result as-is
                result.clone()
            } else {
                json!(null)
            }
        }
        _ => json!(null),
    }
}

/// Extracts the response ID from a `JsonRpcResponse` as a string.
#[must_use]
pub fn extract_response_id(resp: &JsonRpcResponse) -> String {
    match &resp.id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Normalizes A2A response parts into a single string value.
pub(super) fn normalize_a2a_parts(parts: &[Value]) -> Value {
    let mut segments = Vec::new();
    for part in parts {
        let kind = part.get("kind").and_then(Value::as_str).unwrap_or("text");
        if kind == "text" {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                segments.push(text.to_string());
            }
        } else {
            // Non-text parts serialized as JSON
            segments.push(format!(
                "[{kind}]: {}",
                serde_json::to_string(part).unwrap_or_default()
            ));
        }
    }
    Value::String(segments.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::transport::{JsonRpcRequest, JsonRpcResponse};

    use super::*;

    #[test]
    fn test_extract_result_content_success() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(json!({"content": [{"type": "text", "text": "hello"}]})),
            error: None,
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        assert_eq!(
            content,
            json!({"content": [{"type": "text", "text": "hello"}]})
        );
    }

    #[test]
    fn test_extract_result_content_error() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(crate::transport::JsonRpcError {
                code: -32601,
                message: "tool not found".into(),
                data: None,
            }),
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        assert_eq!(content["error"]["code"], -32601);
    }

    #[test]
    fn test_extract_result_content_a2a() {
        let resp = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(json!({
                "message": {
                    "parts": [
                        {"kind": "text", "text": "First"},
                        {"kind": "text", "text": "Second"},
                        {"kind": "file", "uri": "s3://bucket/file"}
                    ]
                }
            })),
            error: None,
            id: json!("1"),
        });
        let content = extract_result_content(&resp);
        let s = content.as_str().unwrap();
        assert!(s.starts_with("First\nSecond\n[file]:"));
    }

    #[test]
    fn test_extract_response_id() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!("abc-123"),
        };
        assert_eq!(extract_response_id(&resp), "abc-123");

        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!(42),
        };
        assert_eq!(extract_response_id(&resp), "42");
    }

    #[test]
    fn test_extract_run_agent_input_messages() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [
                    {"role": "system", "content": "You are helpful"},
                    {"role": "user", "content": "Hello"},
                    {"role": "assistant", "content": "Hi there"}
                ]
            })),
            id: json!("1"),
        });
        let messages = extract_run_agent_input_messages(&msg).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[0], ChatMessage::System(s) if s == "You are helpful"));
        assert!(matches!(&messages[1], ChatMessage::User(s) if s == "Hello"));
        assert!(matches!(&messages[2], ChatMessage::AssistantText(s) if s == "Hi there"));
    }

    #[test]
    fn test_extract_run_agent_input_messages_malformed() {
        let msg = JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: None,
            id: json!("1"),
        });
        assert!(extract_run_agent_input_messages(&msg).is_err());
    }

    #[test]
    fn test_extract_run_agent_input_messages_missing_role() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [{"content": "no role field"}]
            })),
            id: json!("1"),
        });
        let err = extract_run_agent_input_messages(&msg).unwrap_err();
        assert!(err.to_string().contains("missing or non-string 'role'"));
    }

    #[test]
    fn test_extract_run_agent_input_messages_missing_content() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [{"role": "user"}]
            })),
            id: json!("1"),
        });
        let err = extract_run_agent_input_messages(&msg).unwrap_err();
        assert!(err.to_string().contains("missing or non-string 'content'"));
    }

    #[test]
    fn test_extract_user_message() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "run_agent_input".into(),
            params: Some(json!({
                "messages": [
                    {"role": "system", "content": "system"},
                    {"role": "user", "content": "first"},
                    {"role": "user", "content": "second"}
                ]
            })),
            id: json!("1"),
        });
        assert_eq!(extract_user_message(&msg), "second");
    }

    #[test]
    fn test_format_server_request_elicitation() {
        let result = format_server_request_as_user_message(
            "elicitation/create",
            &Some(json!({"message": "Enter your name"})),
        );
        assert_eq!(result, "[Server elicitation] Enter your name");
    }

    #[test]
    fn test_format_server_request_sampling() {
        let result = format_server_request_as_user_message(
            "sampling/createMessage",
            &Some(json!({"messages": []})),
        );
        assert!(result.starts_with("[Server sampling request]"));
    }

    #[test]
    fn test_normalize_a2a_parts() {
        let parts = vec![
            json!({"kind": "text", "text": "Hello"}),
            json!({"kind": "text", "text": "World"}),
        ];
        let result = normalize_a2a_parts(&parts);
        assert_eq!(result, Value::String("Hello\nWorld".into()));
    }
}
