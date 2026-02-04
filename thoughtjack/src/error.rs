//! Error types for ThoughtJack

use thiserror::Error;

/// Top-level error type for ThoughtJack operations
#[derive(Debug, Error)]
pub enum Error {
    /// Configuration error
    #[error("configuration error: {0}")]
    Config(String),

    /// Transport error
    #[error("transport error: {0}")]
    Transport(String),

    /// Phase execution error
    #[error("phase error: {0}")]
    Phase(String),

    /// Behavior error
    #[error("behavior error: {0}")]
    Behavior(String),

    /// Generator error
    #[error("generator error: {0}")]
    Generator(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML parsing error
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Result type alias for ThoughtJack operations
pub type Result<T> = std::result::Result<T, Error>;
