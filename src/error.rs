//! Error types for `ThoughtJack`
//!
//! This module provides a comprehensive error hierarchy matching TJ-SPEC-007
//! exit codes and supporting configuration, transport, and runtime errors.

use std::path::PathBuf;
use thiserror::Error;

// ============================================================================
// Configuration Errors
// ============================================================================

/// Configuration loading and validation errors.
///
/// These errors cover failure modes during OATF document parsing
/// and validation.
///
/// Implements: TJ-SPEC-007 F-007
#[derive(Debug, Error)]
pub enum ConfigError {
    /// YAML parsing failed
    #[error("parse error in {path}{}: {message}", line.map_or_else(String::new, |l| format!(" (line {l})")))]
    ParseError {
        /// Path to the configuration file
        path: PathBuf,
        /// Line number where the error occurred (if available)
        line: Option<usize>,
        /// Error message from the parser
        message: String,
    },

    /// Configuration validation failed
    #[error("validation failed for {path}")]
    ValidationError {
        /// Path to the configuration file
        path: String,
        /// List of validation issues found
        errors: Vec<ValidationIssue>,
    },

    /// Referenced configuration file not found
    #[error("file not found: {path}")]
    MissingFile {
        /// Path to the missing file
        path: PathBuf,
    },

    /// Field has an invalid value
    #[error("invalid value for '{field}': got '{value}', expected {expected}")]
    InvalidValue {
        /// Name of the field with invalid value
        field: String,
        /// The actual value provided
        value: String,
        /// Description of what was expected
        expected: String,
    },

    /// One or more configuration files failed validation.
    ///
    /// Implements: TJ-SPEC-007 EC-CLI-016
    #[error("{count} file(s) failed validation")]
    ValidationFailed {
        /// Number of files that failed validation.
        count: usize,
    },
}

// ============================================================================
// Validation Types
// ============================================================================

/// A single validation issue found during configuration validation.
///
/// Implements: TJ-SPEC-007 F-007
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    /// JSON path to the problematic field (e.g., "phases[2].advance.trigger")
    pub path: String,
    /// Description of the validation issue
    pub message: String,
    /// Severity level of the issue
    pub severity: Severity,
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{}: {} at {}", prefix, self.message, self.path)
    }
}

/// Severity level for validation issues.
///
/// Implements: TJ-SPEC-007 F-007
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Error - validation failure that prevents configuration from being used
    Error,
    /// Warning - potential issue that does not prevent configuration loading
    Warning,
}

// ============================================================================
// Exit Codes (TJ-SPEC-007 v2 §5)
// ============================================================================

/// Exit codes for `ThoughtJack` CLI operations.
///
/// These codes follow Unix conventions and TJ-SPEC-007 v2 requirements.
///
/// Implements: TJ-SPEC-007 F-006
pub struct ExitCode;

impl ExitCode {
    /// Scenario completed, agent was NOT exploited (verdict: `not_exploited`).
    pub const NOT_EXPLOITED: i32 = 0;

    /// Alias for `NOT_EXPLOITED` — used in `main.rs` for general success.
    pub const SUCCESS: i32 = Self::NOT_EXPLOITED;

    /// Scenario completed, agent WAS exploited (verdict: exploited).
    pub const EXPLOITED: i32 = 1;

    /// Scenario completed with errors during execution.
    pub const ERROR: i32 = 2;

    /// Scenario completed, partial exploitation (verdict: partial).
    pub const PARTIAL: i32 = 3;

    /// Runtime error (transport failure, I/O error, etc.).
    pub const RUNTIME_ERROR: i32 = 10;

    /// Usage error (invalid arguments, missing required options).
    pub const USAGE_ERROR: i32 = 64;

    /// Interrupted by SIGINT (Ctrl+C).
    pub const INTERRUPTED: i32 = 130;

    /// Terminated by SIGTERM.
    pub const TERMINATED: i32 = 143;
}

// ============================================================================
// Engine Errors
// ============================================================================

/// Errors from the v0.5 execution engine.
///
/// Covers phase state machine, driver execution, extractor capture,
/// entry action execution, synthesize validation, and SDK interaction.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Error)]
pub enum EngineError {
    /// Phase state machine error
    #[error("phase error: {0}")]
    Phase(String),

    /// Protocol driver error
    #[error("driver error: {0}")]
    Driver(String),

    /// Extractor capture error
    #[error("extractor error: {0}")]
    Extractor(String),

    /// Entry action execution error
    #[error("entry action error: {0}")]
    EntryAction(String),

    /// Synthesize output validation error
    #[error("synthesize validation error: {0}")]
    SynthesizeValidation(String),

    /// OATF SDK error
    #[error("OATF SDK error: {0}")]
    Oatf(String),
}

// ============================================================================
// Loader Errors
// ============================================================================

/// Errors from the OATF document loader.
///
/// Covers YAML pre-processing and SDK document loading.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Error)]
pub enum LoaderError {
    /// OATF SDK loading error
    #[error("OATF load error: {0}")]
    OatfLoad(String),

    /// YAML pre-processing error
    #[error("preprocess error: {0}")]
    Preprocess(String),

    /// Circular `await_extractors` dependency (EC-ORCH-003)
    #[error("circular await_extractors dependency: {0}")]
    CyclicDependency(String),
}

// ============================================================================
// Top-Level Error
// ============================================================================

/// Top-level error type for `ThoughtJack` operations.
///
/// This enum aggregates all domain-specific errors and provides
/// a unified interface for error handling and exit code mapping.
///
/// Implements: TJ-SPEC-007 F-006, F-007
#[derive(Debug, Error)]
pub enum ThoughtJackError {
    /// Configuration loading or validation error
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Transport layer error
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML parsing error
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Usage error (invalid arguments, missing required options)
    #[error("{0}")]
    Usage(String),

    /// Engine error
    #[error(transparent)]
    Engine(#[from] EngineError),

    /// Loader error
    #[error(transparent)]
    Loader(#[from] LoaderError),

    /// Orchestration error
    #[error("orchestration error: {0}")]
    Orchestration(String),

    /// Verdict result with a non-zero exit code.
    ///
    /// Propagates verdict outcomes (exploited, partial, error) through the
    /// `Result` chain so `main.rs` can set the correct process exit code.
    ///
    /// Implements: TJ-SPEC-014 F-009
    #[error("{message}")]
    Verdict {
        /// Human-readable verdict result string.
        message: String,
        /// Process exit code to use.
        code: i32,
    },
}

impl ThoughtJackError {
    /// Returns the appropriate exit code for this error.
    ///
    /// Maps each error variant to its corresponding exit code
    /// as defined in TJ-SPEC-007 v2.
    ///
    /// Implements: TJ-SPEC-007 F-006
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => ExitCode::USAGE_ERROR,
            Self::Verdict { code, .. } => *code,
            Self::Config(_)
            | Self::Transport(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Yaml(_)
            | Self::Engine(_)
            | Self::Loader(_)
            | Self::Orchestration(_) => ExitCode::RUNTIME_ERROR,
        }
    }
}

// ============================================================================
// Transport Errors
// ============================================================================

/// Transport layer errors for stdio and HTTP transports.
///
/// Implements: TJ-SPEC-002 F-007
#[derive(Debug, Error)]
pub enum TransportError {
    /// I/O error during transport operations
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Failed to establish connection
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Connection was closed unexpectedly
    #[error("connection closed: {0}")]
    ConnectionClosed(String),

    /// Internal transport error (e.g. poisoned mutex)
    #[error("internal transport error: {0}")]
    InternalError(String),
}

// ============================================================================
// Result Type Alias
// ============================================================================

/// Result type alias for `ThoughtJack` operations.
pub type Result<T> = std::result::Result<T, ThoughtJackError>;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(ExitCode::NOT_EXPLOITED, 0);
        assert_eq!(ExitCode::SUCCESS, 0);
        assert_eq!(ExitCode::EXPLOITED, 1);
        assert_eq!(ExitCode::ERROR, 2);
        assert_eq!(ExitCode::PARTIAL, 3);
        assert_eq!(ExitCode::RUNTIME_ERROR, 10);
        assert_eq!(ExitCode::USAGE_ERROR, 64);
        assert_eq!(ExitCode::INTERRUPTED, 130);
        assert_eq!(ExitCode::TERMINATED, 143);
    }

    #[test]
    fn test_config_error_exit_code() {
        let err: ThoughtJackError = ConfigError::MissingFile {
            path: PathBuf::from("/test"),
        }
        .into();
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_transport_error_exit_code() {
        let err: ThoughtJackError = TransportError::ConnectionFailed("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_io_error_exit_code() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err: ThoughtJackError = io_err.into();
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_usage_error_exit_code() {
        let err = ThoughtJackError::Usage("bad args".to_string());
        assert_eq!(err.exit_code(), ExitCode::USAGE_ERROR);
    }

    #[test]
    fn test_validation_issue_display() {
        let issue = ValidationIssue {
            path: "phases[0].advance".to_string(),
            message: "missing trigger".to_string(),
            severity: Severity::Error,
        };
        assert_eq!(
            issue.to_string(),
            "error: missing trigger at phases[0].advance"
        );
    }

    #[test]
    fn test_validation_issue_warning_display() {
        let issue = ValidationIssue {
            path: "server.name".to_string(),
            message: "name is empty".to_string(),
            severity: Severity::Warning,
        };
        assert_eq!(issue.to_string(), "warning: name is empty at server.name");
    }

    #[test]
    fn test_engine_error_exit_code() {
        let err: ThoughtJackError = EngineError::Phase("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_loader_error_exit_code() {
        let err: ThoughtJackError = LoaderError::OatfLoad("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_orchestration_error_exit_code() {
        let err = ThoughtJackError::Orchestration("actor failed".to_string());
        assert_eq!(err.exit_code(), ExitCode::RUNTIME_ERROR);
    }

    #[test]
    fn test_verdict_error_exit_code() {
        let err = ThoughtJackError::Verdict {
            message: "exploited".to_string(),
            code: ExitCode::EXPLOITED,
        };
        assert_eq!(err.exit_code(), ExitCode::EXPLOITED);

        let err = ThoughtJackError::Verdict {
            message: "partial".to_string(),
            code: ExitCode::PARTIAL,
        };
        assert_eq!(err.exit_code(), ExitCode::PARTIAL);
    }

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::ParseError {
            path: PathBuf::from("config.yaml"),
            line: Some(42),
            message: "unexpected token".to_string(),
        };
        assert!(err.to_string().contains("config.yaml"));
        assert!(err.to_string().contains("unexpected token"));
    }
}
