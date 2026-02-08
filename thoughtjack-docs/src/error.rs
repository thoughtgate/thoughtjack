//! Error types for `ThoughtJack` documentation generation.

use thiserror::Error;

/// Errors that can occur during diagram generation.
///
/// Implements: TJ-SPEC-011 F-002
#[derive(Debug, Error)]
pub enum DiagramError {
    /// The scenario configuration has no renderable content.
    #[error("no renderable content in scenario: {0}")]
    EmptyScenario(String),

    /// A phase name could not be slugified to a valid Mermaid identifier.
    #[error("failed to slugify phase name: {0}")]
    InvalidPhaseName(String),
}

/// Errors that can occur during documentation generation.
///
/// Implements: TJ-SPEC-011 F-003
#[derive(Debug, Error)]
pub enum DocsError {
    /// Diagram generation failed.
    #[error(transparent)]
    Diagram(#[from] DiagramError),

    /// Registry parsing or validation failed.
    #[error("registry error: {0}")]
    Registry(String),

    /// Metadata validation failed.
    #[error("validation error: {0}")]
    Validation(String),

    /// I/O error during file operations.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// YAML parsing error.
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
