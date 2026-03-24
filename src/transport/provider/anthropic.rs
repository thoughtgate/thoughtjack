//! Anthropic Messages API provider.
//!
//! See TJ-SPEC-022 §3.2 for the provider specification.

use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};

use crate::transport::context::{
    ChatMessage, LlmProvider, LlmResponse, ProviderError, TextResponse, ToolCall, ToolDefinition,
};

/// Default base URL for the Anthropic API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic API version header.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Messages API provider.
///
/// Implements: TJ-SPEC-022 F-001
pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
}

impl AnthropicProvider {
    /// Creates a new provider.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::Parse` if the HTTP client cannot be built
    /// (e.g. TLS backend failure in constrained environments).
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        temperature: f32,
        max_tokens: u32,
        timeout_secs: u64,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(ProviderError::Request)?;

        Ok(Self {
            client,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            api_key,
            model,
            temperature,
            max_tokens,
        })
    }

    /// Extracts system messages and serializes remaining messages to Anthropic format.
    ///
    /// Anthropic requires system prompt as a top-level parameter, not in messages.
    /// `ChatMessage::System` entries are extracted and concatenated.
    fn prepare_request(history: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut system_parts = Vec::new();
        let mut messages = Vec::new();

        for msg in history {
            match msg {
                ChatMessage::System(text) => {
                    system_parts.push(text.clone());
                }
                ChatMessage::User(text) => {
                    messages.push(json!({"role": "user", "content": text}));
                }
                ChatMessage::AssistantText(text) => {
                    messages.push(json!({"role": "assistant", "content": text}));
                }
                ChatMessage::AssistantToolUse { tool_calls } => {
                    let content: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.arguments,
                            })
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "content": content}));
                }
                ChatMessage::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    let content_str = match content {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content_str,
                        }]
                    }));
                }
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        (system, messages)
    }

    /// Serializes tool definitions to Anthropic format.
    fn serialize_tools(tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }
}

#[async_trait::async_trait]
#[allow(clippy::too_many_lines)]
impl LlmProvider for AnthropicProvider {
    async fn chat_completion(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        let (system, messages) = Self::prepare_request(history);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });

        if let Some(system_text) = system {
            body["system"] = json!(system_text);
        }

        if !tools.is_empty() {
            body["tools"] = json!(Self::serialize_tools(tools));
        }

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/v1/messages");

        let resp_body = super::retry::send_with_retry(|| {
            self.client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .send()
        })
        .await?;

        let stop_reason = resp_body
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn");

        let content = resp_body
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Parse("no content in response".into()))?;

        // Check for tool_use content blocks
        let tool_uses: Vec<&Value> = content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("tool_use"))
            .collect();

        if !tool_uses.is_empty() && stop_reason != "max_tokens" {
            let calls: Vec<ToolCall> = tool_uses
                .iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = tc.get("name")?.as_str()?.to_string();
                    let arguments = tc.get("input")?.clone();
                    Some(ToolCall {
                        id,
                        name,
                        arguments,
                        provider_metadata: None,
                    })
                })
                .collect();
            if !calls.is_empty() {
                return Ok(LlmResponse::ToolUse(calls));
            }
        }

        // Text response: concatenate all text blocks
        let text: String = content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");

        let is_truncated = stop_reason == "max_tokens";
        if is_truncated {
            tracing::warn!(
                text_len = text.len(),
                "response truncated (stop_reason=max_tokens)"
            );
        }

        Ok(LlmResponse::Text(TextResponse { text, is_truncated }))
    }

    fn provider_name(&self) -> &'static str {
        "anthropic"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_request_system_extraction() {
        let history = vec![
            ChatMessage::System("System 1".into()),
            ChatMessage::User("Hello".into()),
            ChatMessage::System("System 2".into()),
            ChatMessage::AssistantText("Hi".into()),
        ];
        let (system, messages) = AnthropicProvider::prepare_request(&history);
        assert_eq!(system.unwrap(), "System 1\n\nSystem 2");
        assert_eq!(messages.len(), 2); // user + assistant (no system)
    }

    #[test]
    fn test_prepare_request_tool_result() {
        let history = vec![
            ChatMessage::User("Hello".into()),
            ChatMessage::AssistantToolUse {
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "search".into(),
                    arguments: json!({"q": "test"}),
                    provider_metadata: None,
                }],
            },
            ChatMessage::ToolResult {
                tool_call_id: "tc1".into(),
                content: json!("search result"),
            },
        ];
        let (system, messages) = AnthropicProvider::prepare_request(&history);
        assert!(system.is_none());
        assert_eq!(messages.len(), 3);
        // Tool result is inside a user message
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "tc1");
    }

    #[test]
    fn test_prepare_request_no_system() {
        let history = vec![ChatMessage::User("Hello".into())];
        let (system, messages) = AnthropicProvider::prepare_request(&history);
        assert!(system.is_none());
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_serialize_tools() {
        let tools = vec![ToolDefinition {
            name: "search".into(),
            description: "Search".into(),
            parameters: json!({"type": "object"}),
        }];
        let serialized = AnthropicProvider::serialize_tools(&tools);
        assert_eq!(serialized[0]["name"], "search");
        assert_eq!(serialized[0]["input_schema"]["type"], "object");
    }
}
