//! OpenAI-compatible LLM provider.
//!
//! Covers `OpenAI`, `Groq`, `Together`, `DeepSeek`, `Ollama`, `vLLM`, and any
//! provider implementing the `OpenAI` Chat Completions API.
//!
//! See TJ-SPEC-022 §3.2 for the provider specification.

use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};

use crate::transport::context::{
    ChatMessage, LlmProvider, LlmResponse, ProviderError, TextResponse, ToolCall, ToolDefinition,
};

/// Default base URL for the `OpenAI` API.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// `OpenAI`-compatible LLM provider.
///
/// Implements: TJ-SPEC-022 F-001
pub struct OpenAiCompatibleProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: f32,
    max_tokens: Option<u32>,
}

impl OpenAiCompatibleProvider {
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
        max_tokens: Option<u32>,
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

    /// Serializes conversation history to `OpenAI` message format.
    fn serialize_messages(history: &[ChatMessage]) -> Vec<Value> {
        let mut messages = Vec::with_capacity(history.len());
        for msg in history {
            match msg {
                ChatMessage::System(text) => {
                    messages.push(json!({"role": "system", "content": text}));
                }
                ChatMessage::User(text) => {
                    messages.push(json!({"role": "user", "content": text}));
                }
                ChatMessage::AssistantText(text) => {
                    messages.push(json!({"role": "assistant", "content": text}));
                }
                ChatMessage::AssistantToolUse { tool_calls } => {
                    let tc: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            let mut obj = json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments)
                                        .unwrap_or_default(),
                                }
                            });
                            // Re-attach provider-specific metadata (e.g. Gemini 3.1+
                            // thought_signature) so the provider can correlate results.
                            if let Some(meta) = &tc.provider_metadata
                                && let Some(map) = obj.as_object_mut()
                            {
                                for (k, v) in meta {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                            obj
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "tool_calls": tc}));
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
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content_str,
                    }));
                }
            }
        }
        messages
    }

    /// Serializes tool definitions to `OpenAI` function calling format.
    fn serialize_tools(tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect()
    }
}

#[async_trait::async_trait]
#[allow(clippy::too_many_lines)]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn chat_completion(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError> {
        let messages = Self::serialize_messages(history);
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
        });

        if let Some(max_tokens) = self.max_tokens {
            // Newer OpenAI models (o1, gpt-4.1+) require max_completion_tokens;
            // older models use max_tokens. Send both — the API ignores unknown fields.
            body["max_completion_tokens"] = json!(max_tokens);
        }

        if !tools.is_empty() {
            body["tools"] = json!(Self::serialize_tools(tools));
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp_body = super::retry::send_with_retry(|| {
            self.client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
        })
        .await?;

        // Extract the first choice
        let choice = resp_body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .ok_or_else(|| ProviderError::Parse("no choices in response".into()))?;

        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop");

        let message = choice
            .get("message")
            .ok_or_else(|| ProviderError::Parse("no message in choice".into()))?;

        // Check for tool calls
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array)
            && !tool_calls.is_empty()
            && finish_reason != "length"
        {
            let calls: Vec<ToolCall> = tool_calls
                .iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let function = tc.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let args_str = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    let arguments: Value = serde_json::from_str(args_str).unwrap_or_else(|e| {
                        tracing::warn!(args_str, error = %e, "malformed tool call arguments from LLM provider, defaulting to {{}}");
                        json!({})
                    });

                    // Preserve provider-specific metadata (e.g. Gemini 3.1+
                    // thought_signature) that must be echoed back in
                    // subsequent API calls.
                    let known_keys: &[&str] = &["id", "type", "function", "index"];
                    let metadata: serde_json::Map<String, Value> = tc
                        .as_object()
                        .map(|obj| {
                            obj.iter()
                                .filter(|(k, _)| !known_keys.contains(&k.as_str()))
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect()
                        })
                        .unwrap_or_default();

                    Some(ToolCall {
                        id,
                        name,
                        arguments,
                        provider_metadata: if metadata.is_empty() {
                            None
                        } else {
                            Some(metadata)
                        },
                    })
                })
                .collect();
            if !calls.is_empty() {
                return Ok(LlmResponse::ToolUse(calls));
            }
        }

        // Text response
        let text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let is_truncated = finish_reason == "length";

        if is_truncated {
            tracing::warn!(
                text_len = text.len(),
                "response truncated (finish_reason=length)"
            );
        }

        Ok(LlmResponse::Text(TextResponse { text, is_truncated }))
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_messages() {
        let history = vec![
            ChatMessage::System("You are helpful".into()),
            ChatMessage::User("Hello".into()),
            ChatMessage::AssistantText("Hi".into()),
        ];
        let messages = OpenAiCompatibleProvider::serialize_messages(&history);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn test_serialize_messages_tool_use() {
        let history = vec![
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
                content: json!("result text"),
            },
        ];
        let messages = OpenAiCompatibleProvider::serialize_messages(&history);
        assert_eq!(messages.len(), 2);
        assert!(messages[0]["tool_calls"].is_array());
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "tc1");
    }

    #[test]
    fn test_serialize_preserves_provider_metadata() {
        let mut meta = serde_json::Map::new();
        meta.insert("thought_signature".to_string(), json!("encrypted_sig_xyz"));
        let history = vec![ChatMessage::AssistantToolUse {
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "calc".into(),
                arguments: json!({"x": 1}),
                provider_metadata: Some(meta),
            }],
        }];
        let messages = OpenAiCompatibleProvider::serialize_messages(&history);
        let tc = &messages[0]["tool_calls"][0];
        assert_eq!(tc["thought_signature"], "encrypted_sig_xyz");
        assert_eq!(tc["id"], "tc1");
        assert_eq!(tc["type"], "function");
    }

    #[test]
    fn test_serialize_tools() {
        let tools = vec![ToolDefinition {
            name: "search".into(),
            description: "Search the web".into(),
            parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }];
        let serialized = OpenAiCompatibleProvider::serialize_tools(&tools);
        assert_eq!(serialized.len(), 1);
        assert_eq!(serialized[0]["type"], "function");
        assert_eq!(serialized[0]["function"]["name"], "search");
    }
}
