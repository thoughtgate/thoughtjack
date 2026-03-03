//! Grace period timer for post-terminal-phase observation.
//!
//! After all terminal phases complete, the grace period keeps the transport
//! open so delayed effects (e.g., exfiltration on the next agent action)
//! are captured in the protocol trace before verdict evaluation.
//!
//! See TJ-SPEC-014 §2 for grace period semantics.

use std::time::{Duration, Instant};

// ============================================================================
// GracePeriodState
// ============================================================================

/// Tracks the grace period observation window.
///
/// The grace period begins when the terminal phase is entered and lasts
/// for the configured duration. During this time, the transport remains
/// open and trace continues capturing messages.
///
/// Implements: TJ-SPEC-014 F-001
#[derive(Debug)]
pub struct GracePeriodState {
    /// Configured grace period duration.
    duration: Duration,
    /// When the grace period started (`None` if not yet started).
    started_at: Option<Instant>,
    /// Trace entry count at the moment the terminal phase was entered.
    trace_snapshot_at_terminal: usize,
}

impl GracePeriodState {
    /// Creates a new grace period state with the given duration.
    ///
    /// Implements: TJ-SPEC-014 F-001
    #[must_use]
    pub const fn new(duration: Duration) -> Self {
        Self {
            duration,
            started_at: None,
            trace_snapshot_at_terminal: 0,
        }
    }

    /// Returns the configured grace period duration.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Starts the grace period timer and records the trace snapshot index.
    ///
    /// Implements: TJ-SPEC-014 F-001
    pub fn start(&mut self, trace_len: usize) {
        self.started_at = Some(Instant::now());
        self.trace_snapshot_at_terminal = trace_len;
    }

    /// Returns `true` if the grace period has been started.
    #[must_use]
    pub const fn is_started(&self) -> bool {
        self.started_at.is_some()
    }

    /// Returns `true` if the grace period has expired.
    ///
    /// Returns `false` if the grace period has not been started.
    ///
    /// Implements: TJ-SPEC-014 F-001
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.started_at
            .is_some_and(|start| start.elapsed() >= self.duration)
    }

    /// Returns the remaining duration, or `Duration::ZERO` if expired or not started.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.started_at.map_or(Duration::ZERO, |start| {
            self.duration.saturating_sub(start.elapsed())
        })
    }

    /// Returns the trace entry count at the moment the terminal phase was entered.
    #[must_use]
    pub const fn trace_snapshot_at_terminal(&self) -> usize {
        self.trace_snapshot_at_terminal
    }

    /// Returns the elapsed time since the grace period started, or `None`.
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        self.started_at.map(|start| start.elapsed())
    }
}

// ============================================================================
// Resolve grace period duration
// ============================================================================

/// Resolves the effective grace period duration from CLI and document sources.
///
/// Priority: CLI `--grace-period` > document `attack.grace_period` > default (0s).
///
/// # Errors
///
/// Returns `None` for unparseable document duration strings (logged as warning).
///
/// Implements: TJ-SPEC-014 F-001
#[must_use]
pub fn resolve_grace_period(
    cli_override: Option<Duration>,
    document_value: Option<&str>,
) -> Duration {
    if let Some(cli) = cli_override {
        return cli;
    }

    if let Some(doc_str) = document_value {
        if let Ok(d) = humantime::parse_duration(doc_str) {
            return d;
        }
        tracing::warn!(
            duration = doc_str,
            "could not parse document grace_period, using 0s"
        );
    }

    Duration::ZERO
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn new_state_not_started() {
        let state = GracePeriodState::new(Duration::from_secs(30));
        assert!(!state.is_started());
        assert!(!state.is_expired());
        assert_eq!(state.remaining(), Duration::ZERO);
        assert_eq!(state.trace_snapshot_at_terminal(), 0);
        assert!(state.elapsed().is_none());
    }

    #[test]
    fn start_records_snapshot_and_time() {
        let mut state = GracePeriodState::new(Duration::from_secs(30));
        state.start(42);
        assert!(state.is_started());
        assert!(!state.is_expired());
        assert_eq!(state.trace_snapshot_at_terminal(), 42);
        assert!(state.elapsed().is_some());
    }

    #[test]
    fn zero_duration_expires_immediately() {
        let mut state = GracePeriodState::new(Duration::ZERO);
        state.start(0);
        assert!(state.is_expired());
        assert_eq!(state.remaining(), Duration::ZERO);
    }

    #[test]
    fn expires_after_duration() {
        let mut state = GracePeriodState::new(Duration::from_millis(10));
        state.start(5);
        assert!(!state.is_expired());
        thread::sleep(Duration::from_millis(15));
        assert!(state.is_expired());
    }

    #[test]
    fn remaining_decreases() {
        let mut state = GracePeriodState::new(Duration::from_secs(10));
        state.start(0);
        let r1 = state.remaining();
        thread::sleep(Duration::from_millis(5));
        let r2 = state.remaining();
        assert!(r2 <= r1);
    }

    #[test]
    fn duration_accessor() {
        let state = GracePeriodState::new(Duration::from_secs(42));
        assert_eq!(state.duration(), Duration::from_secs(42));
    }

    // ── resolve_grace_period tests ──────────────────────────────────────

    #[test]
    fn cli_override_takes_precedence() {
        let result = resolve_grace_period(Some(Duration::from_secs(60)), Some("30s"));
        assert_eq!(result, Duration::from_secs(60));
    }

    #[test]
    fn document_value_used_when_no_cli() {
        let result = resolve_grace_period(None, Some("30s"));
        assert_eq!(result, Duration::from_secs(30));
    }

    #[test]
    fn default_zero_when_nothing_specified() {
        let result = resolve_grace_period(None, None);
        assert_eq!(result, Duration::ZERO);
    }

    #[test]
    fn unparseable_document_value_falls_back_to_zero() {
        let result = resolve_grace_period(None, Some("not-a-duration"));
        assert_eq!(result, Duration::ZERO);
    }

    #[test]
    fn humantime_formats_accepted() {
        assert_eq!(
            resolve_grace_period(None, Some("2m")),
            Duration::from_secs(120)
        );
        assert_eq!(
            resolve_grace_period(None, Some("1h")),
            Duration::from_secs(3600)
        );
        assert_eq!(
            resolve_grace_period(None, Some("500ms")),
            Duration::from_millis(500)
        );
    }
}
