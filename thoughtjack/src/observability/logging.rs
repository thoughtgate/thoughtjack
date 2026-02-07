//! Logging initialization for `ThoughtJack` (TJ-SPEC-008).
//!
//! Provides structured logging via `tracing` with human-readable and
//! JSON output formats, configurable verbosity, and environment-based
//! override via `THOUGHTJACK_LOG_LEVEL`.

use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

use crate::cli::args::ColorChoice;

/// Log output format.
///
/// Controls how log messages are rendered to stderr.
///
/// Implements: TJ-SPEC-008 F-002, F-003
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable format with optional ANSI colors.
    #[default]
    Human,
    /// Newline-delimited JSON for machine consumption.
    Json,
}

/// Maps a verbosity level to a tracing directive string.
///
/// - 0 → `"warn"`
/// - 1 → `"info"`
/// - 2 → `"debug"`
/// - 3+ → `"trace"` (saturates)
///
/// Implements: TJ-SPEC-008 F-001
#[must_use]
pub const fn verbosity_to_directive(verbosity: u8) -> &'static str {
    match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    }
}

/// Initializes the global tracing subscriber.
///
/// Verbosity mapping (when `THOUGHTJACK_LOG_LEVEL` is not set):
/// - 0 → warn
/// - 1 → info
/// - 2 → debug
/// - 3+ → trace
///
/// If `THOUGHTJACK_LOG_LEVEL` is set it takes precedence over `verbosity`.
///
/// Uses `try_init()` so calling this more than once (e.g. in tests) is safe.
///
/// Implements: TJ-SPEC-008 F-001
pub fn init_logging(format: LogFormat, verbosity: u8, color: ColorChoice) {
    let default_directive = verbosity_to_directive(verbosity);

    let filter = EnvFilter::try_from_env("THOUGHTJACK_LOG_LEVEL")
        .unwrap_or_else(|_| EnvFilter::new(default_directive));

    let show_target = verbosity >= 2;

    let use_ansi = match color {
        ColorChoice::Auto => {
            std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
        }
        ColorChoice::Always => true,
        ColorChoice::Never => false,
    };

    match format {
        LogFormat::Human => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(use_ansi)
                .with_target(show_target)
                .with_writer(std::io::stderr)
                .try_init();
        }
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_target(show_target)
                .with_writer(std::io::stderr)
                .try_init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_default_is_human() {
        assert_eq!(LogFormat::default(), LogFormat::Human);
    }

    #[test]
    fn log_format_clone_copy_eq() {
        let a = LogFormat::Json;
        let b = a; // Copy
        let c = a; // Copy (also tests Clone)
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_ne!(a, LogFormat::Human);
    }

    #[test]
    fn init_logging_does_not_panic() {
        // try_init is idempotent — repeated calls simply return Err and are ignored
        init_logging(LogFormat::Human, 0, ColorChoice::Auto);
        init_logging(LogFormat::Json, 3, ColorChoice::Never);
    }

    #[test]
    fn verbosity_0_is_warn() {
        assert_eq!(verbosity_to_directive(0), "warn");
    }

    #[test]
    fn verbosity_1_is_info() {
        assert_eq!(verbosity_to_directive(1), "info");
    }

    #[test]
    fn verbosity_2_is_debug() {
        assert_eq!(verbosity_to_directive(2), "debug");
    }

    #[test]
    fn verbosity_3_is_trace() {
        assert_eq!(verbosity_to_directive(3), "trace");
    }

    #[test]
    fn verbosity_255_is_trace() {
        assert_eq!(verbosity_to_directive(255), "trace");
    }
}
