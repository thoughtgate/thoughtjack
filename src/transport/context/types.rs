use serde_json::Value;

use crate::transport::JsonRpcMessage;

use super::extraction::extract_result_content;

/// Conversation message for the LLM API.
///
/// Serialized differently per provider (`OpenAI` vs `Anthropic`).
/// The enum is provider-agnostic — each `LlmProvider` implementation
/// handles serialisation internally.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// System instruction message.
    System(String),
    /// User message.
    User(String),
    /// Text-only assistant response.
    AssistantText(String),
    /// Assistant response requesting tool calls.
    AssistantToolUse {
        /// Tool calls requested by the assistant.
        tool_calls: Vec<ToolCall>,
    },
    /// Result of a tool call, keyed by `tool_call_id`.
    ToolResult {
        /// The `tool_call_id` this result corresponds to.
        tool_call_id: String,
        /// The result content (JSON value).
        content: Value,
    },
}

impl ChatMessage {
    /// Creates an `AssistantText` message.
    #[must_use]
    pub fn assistant_text(text: &str) -> Self {
        Self::AssistantText(text.to_string())
    }

    /// Creates an `AssistantToolUse` message.
    #[must_use]
    pub fn assistant_tool_use(calls: &[ToolCall]) -> Self {
        Self::AssistantToolUse {
            tool_calls: calls.to_vec(),
        }
    }

    /// Creates a `ToolResult` message from a JSON-RPC response.
    #[must_use]
    pub fn tool_result(tool_call_id: &str, response: &JsonRpcMessage) -> Self {
        Self::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: extract_result_content(response),
        }
    }

    /// Creates a `ToolResult` with an error message.
    #[must_use]
    pub fn tool_error(tool_call_id: &str, error_msg: &str) -> Self {
        Self::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: serde_json::json!({"error": error_msg}),
        }
    }

    /// Creates a `User` message.
    #[must_use]
    pub fn user(text: &str) -> Self {
        Self::User(text.to_string())
    }
}

/// Tool definition in LLM-native format (`OpenAI` function calling schema).
///
/// Converted from OATF tool state by `extract_tool_definitions()`.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for tool parameters.
    pub parameters: Value,
}

/// A tool call from the LLM response.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Provider-assigned tool call ID.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool call arguments (JSON object).
    pub arguments: Value,
    /// Provider-specific metadata that must be echoed back in subsequent
    /// API calls (e.g. Gemini 3.1+ `thought_signature`).
    pub provider_metadata: Option<serde_json::Map<String, Value>>,
}

/// Response from an LLM API call.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug)]
pub enum LlmResponse {
    /// Text response (may be truncated).
    Text(TextResponse),
    /// Tool use response with one or more tool calls.
    ToolUse(Vec<ToolCall>),
}

/// Text content from an LLM response.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug)]
pub struct TextResponse {
    /// The text content.
    pub text: String,
    /// True when `finish_reason` was `length` / `stop_reason` was `max_tokens`.
    pub is_truncated: bool,
}

/// Errors from LLM provider operations.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// HTTP error response.
    #[error("HTTP error: {status} {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },
    /// Request transport error.
    #[error("Request error: {0}")]
    Request(#[from] reqwest::Error),
    /// Response parse error.
    #[error("Parse error: {0}")]
    Parse(String),
    /// Rate limited after retries.
    #[error("Rate limited after {retries} retries")]
    RateLimited {
        /// Number of retries attempted.
        retries: u32,
    },
    /// Request timeout.
    #[error("Timeout after {seconds}s")]
    Timeout {
        /// Timeout duration in seconds.
        seconds: u64,
    },
}

/// Async trait for LLM API providers.
///
/// Implementations handle provider-specific serialization (`OpenAI` vs `Anthropic`)
/// and rate limiting. System messages, tool definitions, and conversation
/// history are passed in provider-agnostic form.
///
/// Implements: TJ-SPEC-022 F-001
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Performs a chat completion API call.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` on HTTP, parsing, rate-limiting, or timeout errors.
    async fn chat_completion(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ProviderError>;

    /// Returns the provider name (e.g. "openai", "anthropic").
    fn provider_name(&self) -> &'static str;

    /// Returns the model identifier.
    fn model_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let msg = ChatMessage::user("hello");
        assert!(matches!(msg, ChatMessage::User(s) if s == "hello"));

        let msg = ChatMessage::assistant_text("world");
        assert!(matches!(msg, ChatMessage::AssistantText(s) if s == "world"));

        let calls = vec![ToolCall {
            id: "tc1".into(),
            name: "search".into(),
            arguments: json!({"q": "test"}),
            provider_metadata: None,
        }];
        let msg = ChatMessage::assistant_tool_use(&calls);
        assert!(
            matches!(msg, ChatMessage::AssistantToolUse { tool_calls } if tool_calls.len() == 1)
        );
    }
}
