//! LLM provider implementations for context-mode.
//!
//! Provides `OpenAiCompatibleProvider` (covers `OpenAI`, `Groq`, `Together`,
//! `DeepSeek`, `Ollama`, `vLLM`) and `AnthropicProvider` (Messages API).
//!
//! See TJ-SPEC-022 §3 for the provider specification.

pub mod anthropic;
pub mod openai;
pub mod retry;

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
            config.base_url.clone(),
            config.temperature,
            config.max_tokens.unwrap_or(4096),
            config.timeout_secs,
        ))),
        other => Err(EngineError::Driver(format!(
            "unknown context provider type: '{other}' (expected 'openai' or 'anthropic')"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(provider_type: &str) -> ProviderConfig {
        ProviderConfig {
            provider_type: provider_type.to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            base_url: None,
            temperature: 0.0,
            max_tokens: None,
            timeout_secs: 30,
        }
    }

    #[test]
    fn create_openai_provider() {
        let provider = create_provider(&test_config("openai")).unwrap();
        assert_eq!(provider.provider_name(), "openai");
        assert_eq!(provider.model_name(), "test-model");
    }

    #[test]
    fn create_anthropic_provider() {
        let provider = create_provider(&test_config("anthropic")).unwrap();
        assert_eq!(provider.provider_name(), "anthropic");
        assert_eq!(provider.model_name(), "test-model");
    }

    #[test]
    fn create_unknown_provider_errors() {
        let result = create_provider(&test_config("llama"));
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("unknown context provider type"),
            "got: {err}"
        );
    }

    #[test]
    fn openai_with_base_url_and_max_tokens() {
        let mut config = test_config("openai");
        config.base_url = Some("https://custom.api.com/v1".to_string());
        config.max_tokens = Some(1024);
        let provider = create_provider(&config).unwrap();
        assert_eq!(provider.provider_name(), "openai");
    }

    #[test]
    fn provider_config_debug_impl() {
        let config = test_config("openai");
        let debug = format!("{config:?}");
        assert!(debug.contains("openai"));
        assert!(debug.contains("test-model"));
    }
}
