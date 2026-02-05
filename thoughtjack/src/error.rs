//! Error types for `ThoughtJack`
//!
//! This module provides a comprehensive error hierarchy matching TJ-SPEC-007
//! exit codes and TJ-SPEC-006 configuration error requirements.

use std::path::PathBuf;
use thiserror::Error;

// ============================================================================
// Exit Codes (TJ-SPEC-007)
// ============================================================================

/// Exit codes for `ThoughtJack` CLI operations.
///
/// These codes follow Unix conventions and TJ-SPEC-007 requirements.
///
/// Implements: TJ-SPEC-007 F-009
pub struct ExitCode;

impl ExitCode {
    /// Successful execution
    pub const SUCCESS: i32 = 0;

    /// General error
    pub const ERROR: i32 = 1;

    /// Configuration error (invalid YAML, validation failure)
    pub const CONFIG_ERROR: i32 = 2;

    /// I/O error (file not found, permission denied)
    pub const IO_ERROR: i32 = 3;

    /// Transport error (connection failed, protocol error)
    pub const TRANSPORT_ERROR: i32 = 4;

    /// Phase engine error (invalid transition, trigger error)
    pub const PHASE_ERROR: i32 = 5;

    /// Generator error (limit exceeded, generation failed)
    pub const GENERATOR_ERROR: i32 = 10;

    /// Usage error (invalid arguments, missing required options)
    pub const USAGE_ERROR: i32 = 64;

    /// Interrupted by SIGINT (Ctrl+C)
    pub const INTERRUPTED: i32 = 130;

    /// Terminated by SIGTERM
    pub const TERMINATED: i32 = 143;
}

// ============================================================================
// Top-Level Error
// ============================================================================

/// Top-level error type for `ThoughtJack` operations.
///
/// This enum aggregates all domain-specific errors and provides
/// a unified interface for error handling and exit code mapping.
///
/// Implements: TJ-SPEC-007 F-009
#[derive(Debug, Error)]
pub enum ThoughtJackError {
    /// Configuration loading or validation error
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Transport layer error
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// Phase engine error
    #[error(transparent)]
    Phase(#[from] PhaseError),

    /// Behavior execution error
    #[error(transparent)]
    Behavior(#[from] BehaviorError),

    /// Payload generator error
    #[error(transparent)]
    Generator(#[from] GeneratorError),

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

impl ThoughtJackError {
    /// Returns the appropriate exit code for this error.
    ///
    /// Maps each error variant to its corresponding exit code
    /// as defined in TJ-SPEC-007.
    ///
    /// Implements: TJ-SPEC-007 F-009
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) | Self::Json(_) | Self::Yaml(_) => ExitCode::CONFIG_ERROR,
            Self::Transport(_) => ExitCode::TRANSPORT_ERROR,
            Self::Phase(_) => ExitCode::PHASE_ERROR,
            Self::Generator(_) => ExitCode::GENERATOR_ERROR,
            Self::Behavior(_) => ExitCode::ERROR,
            Self::Io(_) => ExitCode::IO_ERROR,
        }
    }
}

// ============================================================================
// Configuration Errors (TJ-SPEC-006)
// ============================================================================

/// Configuration loading and validation errors.
///
/// These errors cover all failure modes during configuration parsing,
/// validation, and directive processing as specified in TJ-SPEC-006.
///
/// Implements: TJ-SPEC-006 F-008
#[derive(Debug, Error)]
pub enum ConfigError {
    /// YAML parsing failed
    #[error("parse error in {path}: {message}")]
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

    /// Circular include detected in configuration files
    #[error("circular include detected: {cycle:?}")]
    CircularInclude {
        /// The cycle of file paths that form the circular reference
        cycle: Vec<PathBuf>,
    },

    /// Referenced configuration file not found
    #[error("file not found: {path}")]
    MissingFile {
        /// Path to the missing file
        path: PathBuf,
    },

    /// Required field is missing from configuration
    #[error("missing required field '{field}' at {location}")]
    MissingRequired {
        /// Name of the missing field
        field: String,
        /// Location in the configuration (e.g., "phases[0].advance")
        location: String,
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

    /// Environment variable referenced in configuration is not set
    #[error("environment variable '{var}' not set (referenced at {location})")]
    EnvVarNotSet {
        /// Name of the environment variable
        var: String,
        /// Location in the configuration where it was referenced
        location: String,
    },
}

// ============================================================================
// Validation Types
// ============================================================================

/// A single validation issue found during configuration validation.
///
/// Implements: TJ-SPEC-006 F-008
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
/// Implements: TJ-SPEC-006 F-008
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Error - validation failure that prevents configuration from being used
    Error,
    /// Warning - potential issue that does not prevent configuration loading
    Warning,
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

    /// Protocol-level error (malformed JSON-RPC, invalid message format)
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Connection was closed unexpectedly
    #[error("connection closed: {0}")]
    ConnectionClosed(String),

    /// Read or write timeout
    #[error("timeout: {0}")]
    Timeout(String),

    /// Message exceeds size limit
    #[error("message too large: {size} bytes (limit: {limit})")]
    MessageTooLarge {
        /// Actual message size in bytes
        size: usize,
        /// Configured size limit in bytes
        limit: usize,
    },
}

// ============================================================================
// Phase Engine Errors
// ============================================================================

/// Phase engine state machine errors.
///
/// Implements: TJ-SPEC-003 F-001
#[derive(Debug, Error)]
pub enum PhaseError {
    /// Attempted invalid state transition
    #[error("invalid phase transition: {0}")]
    InvalidTransition(String),

    /// Referenced phase does not exist
    #[error("phase not found: {0}")]
    NotFound(String),

    /// Trigger evaluation failed
    #[error("trigger evaluation failed: {0}")]
    TriggerError(String),

    /// Entry action execution failed
    #[error("entry action failed: {0}")]
    EntryActionFailed(String),
}

// ============================================================================
// Behavior Errors
// ============================================================================

/// Behavior execution errors for delivery behaviors and side effects.
///
/// Implements: TJ-SPEC-004 F-001
#[derive(Debug, Error)]
pub enum BehaviorError {
    /// Behavior execution failed
    #[error("behavior execution failed: {0}")]
    ExecutionFailed(String),

    /// Side effect execution failed
    #[error("side effect failed: {0}")]
    SideEffectFailed(String),

    /// Invalid behavior configuration
    #[error("invalid behavior configuration: {0}")]
    InvalidConfig(String),
}

impl From<TransportError> for BehaviorError {
    fn from(err: TransportError) -> Self {
        Self::ExecutionFailed(err.to_string())
    }
}

impl From<serde_json::Error> for BehaviorError {
    fn from(err: serde_json::Error) -> Self {
        Self::ExecutionFailed(err.to_string())
    }
}

// ============================================================================
// Generator Errors
// ============================================================================

/// Payload generator errors.
///
/// Implements: TJ-SPEC-005 F-008
#[derive(Debug, Error)]
pub enum GeneratorError {
    /// General generation failure
    #[error("generation failed: {0}")]
    GenerationFailed(String),

    /// Size or depth limit exceeded
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// Invalid generator parameters
    #[error("invalid generator parameters: {0}")]
    InvalidParameters(String),

    /// Seed validation failed
    #[error("invalid seed: {0}")]
    InvalidSeed(String),
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
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(ExitCode::SUCCESS, 0);
        assert_eq!(ExitCode::ERROR, 1);
        assert_eq!(ExitCode::CONFIG_ERROR, 2);
        assert_eq!(ExitCode::IO_ERROR, 3);
        assert_eq!(ExitCode::TRANSPORT_ERROR, 4);
        assert_eq!(ExitCode::PHASE_ERROR, 5);
        assert_eq!(ExitCode::GENERATOR_ERROR, 10);
        assert_eq!(ExitCode::USAGE_ERROR, 64);
        assert_eq!(ExitCode::INTERRUPTED, 130);
        assert_eq!(ExitCode::TERMINATED, 143);
    }

    #[test]
    fn test_phase_error_exit_code() {
        let err: ThoughtJackError = PhaseError::InvalidTransition("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::PHASE_ERROR);
    }

    #[test]
    fn test_generator_error_exit_code() {
        let err: ThoughtJackError = GeneratorError::LimitExceeded("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::GENERATOR_ERROR);
    }

    #[test]
    fn test_config_error_exit_code() {
        let err: ThoughtJackError = ConfigError::MissingFile {
            path: PathBuf::from("/test"),
        }
        .into();
        assert_eq!(err.exit_code(), ExitCode::CONFIG_ERROR);
    }

    #[test]
    fn test_transport_error_exit_code() {
        let err: ThoughtJackError = TransportError::ConnectionFailed("test".to_string()).into();
        assert_eq!(err.exit_code(), ExitCode::TRANSPORT_ERROR);
    }

    #[test]
    fn test_io_error_exit_code() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err: ThoughtJackError = io_err.into();
        assert_eq!(err.exit_code(), ExitCode::IO_ERROR);
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
    fn test_config_error_display() {
        let err = ConfigError::ParseError {
            path: PathBuf::from("config.yaml"),
            line: Some(42),
            message: "unexpected token".to_string(),
        };
        assert!(err.to_string().contains("config.yaml"));
        assert!(err.to_string().contains("unexpected token"));
    }

    #[test]
    fn test_config_error_env_var_display() {
        let err = ConfigError::EnvVarNotSet {
            var: "API_KEY".to_string(),
            location: "server.auth.token".to_string(),
        };
        assert!(err.to_string().contains("API_KEY"));
        assert!(err.to_string().contains("server.auth.token"));
    }
}
