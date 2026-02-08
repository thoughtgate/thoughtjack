//! Core error types for `ThoughtJack`
//!
//! Configuration and validation error types shared across the workspace.

use std::path::PathBuf;
use thiserror::Error;

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

#[cfg(test)]
mod tests {
    use super::*;

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
