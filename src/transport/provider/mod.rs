//! LLM provider implementations for context-mode.
//!
//! Provides `OpenAiCompatibleProvider` (covers `OpenAI`, `Groq`, `Together`,
//! `DeepSeek`, `Ollama`, `vLLM`) and `AnthropicProvider` (Messages API).
//!
//! See TJ-SPEC-022 §3 for the provider specification.

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiCompatibleProvider;

use crate::error::EngineError;
use crate::transport::context::LlmProvider;

/// Configuration for constructing an LLM provider.
///
/// Implements: TJ-SPEC-022 F-001
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider type: "openai" or "anthropic".
    pub provider_type: String,
    /// API key.
    pub api_key: String,
    /// Model identifier.
    pub model: String,
    /// Base URL override (None uses provider default).
    pub base_url: Option<String>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Max tokens per response.
    pub max_tokens: Option<u32>,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
}

/// Creates an `LlmProvider` from configuration.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the provider type is unknown.
///
/// Implements: TJ-SPEC-022 F-001
pub fn create_provider(config: &ProviderConfig) -> Result<Box<dyn LlmProvider>, EngineError> {
    match config.provider_type.as_str() {
        "openai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            config.api_key.clone(),
            config.model.clone(),
            config.base_url.clone(),
            config.temperature,
            config.max_tokens,
            config.timeout_secs,
        ))),
        "anthropic" => Ok(Box::new(AnthropicProvider::new(
            config.api_key.clone(),
            config.model.clone(),
            config.temperature,
            config.max_tokens.unwrap_or(4096),
            config.timeout_secs,
        ))),
        other => Err(EngineError::Driver(format!(
            "unknown context provider type: '{other}' (expected 'openai' or 'anthropic')"
        ))),
    }
}
