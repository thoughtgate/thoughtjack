//! Foundation types for the v0.5 execution engine.
//!
//! These types have no engine-internal dependencies and form the
//! leaf layer that all other engine modules build on.
//!
//! See TJ-SPEC-013 §8.4 for `ProtocolEvent` and `DriveResult`.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ============================================================================
// Direction
// ============================================================================

/// Direction of a protocol message relative to the agent under test.
///
/// - `Incoming`: message *from* the agent (request in server mode, response in client mode)
/// - `Outgoing`: message *to* the agent (response in server mode, request in client mode)
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Message from the agent under test.
    Incoming,
    /// Message to the agent under test.
    Outgoing,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incoming => write!(f, "incoming"),
            Self::Outgoing => write!(f, "outgoing"),
        }
    }
}

// ============================================================================
// ProtocolEvent
// ============================================================================

/// A protocol event emitted by a driver and consumed by the `PhaseLoop`.
///
/// Drivers emit one `ProtocolEvent` for every protocol message they
/// send or receive. The `PhaseLoop` uses these for trace capture,
/// extractor evaluation, and trigger checking.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone)]
pub struct ProtocolEvent {
    /// Whether the message is incoming (from agent) or outgoing (to agent).
    pub direction: Direction,
    /// Wire method name (e.g., `"tools/call"`, `"message/send"`, `"RUN_FINISHED"`).
    pub method: String,
    /// Message content as a JSON value.
    pub content: serde_json::Value,
}

// ============================================================================
// PhaseAction
// ============================================================================

/// Result of processing a protocol event against the current phase trigger.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseAction {
    /// Trigger did not fire — remain in the current phase.
    Stay,
    /// Trigger fired — advance to the next phase.
    Advance,
}

// ============================================================================
// DriveResult
// ============================================================================

/// Result returned by a `PhaseDriver` when its phase work completes.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone)]
pub enum DriveResult {
    /// Phase work complete: all actions sent (client), stream closed (client),
    /// or cancel token fired (server). The `PhaseLoop` drains any remaining
    /// buffered events after this returns.
    Complete,
}

// ============================================================================
// TerminationReason
// ============================================================================

/// Reason why an actor's execution terminated.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    /// The terminal (trigger-less) phase was reached.
    TerminalPhaseReached,
    /// Execution was cancelled via cancellation token.
    Cancelled,
    /// The maximum session duration expired.
    MaxSessionExpired,
}

impl fmt::Display for TerminationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TerminalPhaseReached => write!(f, "terminal phase reached"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::MaxSessionExpired => write!(f, "max session expired"),
        }
    }
}

// ============================================================================
// ActorResult
// ============================================================================

/// Result of a single actor's execution.
///
/// Returned by `PhaseLoop::run()` when the actor finishes.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone)]
pub struct ActorResult {
    /// Name of the actor that completed.
    pub actor_name: String,
    /// Why execution terminated.
    pub termination: TerminationReason,
    /// Number of phases completed (advanced through) before termination.
    pub phases_completed: usize,
}

// ============================================================================
// AwaitExtractor
// ============================================================================

/// Cross-actor extractor synchronization specification.
///
/// Parsed from the ThoughtJack-specific `await_extractors` YAML key
/// during pre-processing (see TJ-SPEC-015 §4.2). Specifies that a
/// phase should wait for extractors from another actor before
/// proceeding.
///
/// Implements: TJ-SPEC-015 F-001
#[derive(Debug, Clone)]
pub struct AwaitExtractor {
    /// Name of the actor to wait for.
    pub actor: String,
    /// Extractor names that must be populated before proceeding.
    pub extractors: Vec<String>,
    /// Maximum time to wait before proceeding anyway.
    pub timeout: Duration,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_display() {
        assert_eq!(Direction::Incoming.to_string(), "incoming");
        assert_eq!(Direction::Outgoing.to_string(), "outgoing");
    }

    #[test]
    fn direction_equality() {
        assert_eq!(Direction::Incoming, Direction::Incoming);
        assert_ne!(Direction::Incoming, Direction::Outgoing);
    }

    #[test]
    fn phase_action_equality() {
        assert_eq!(PhaseAction::Stay, PhaseAction::Stay);
        assert_eq!(PhaseAction::Advance, PhaseAction::Advance);
        assert_ne!(PhaseAction::Stay, PhaseAction::Advance);
    }

    #[test]
    fn termination_reason_display() {
        assert_eq!(
            TerminationReason::TerminalPhaseReached.to_string(),
            "terminal phase reached"
        );
        assert_eq!(TerminationReason::Cancelled.to_string(), "cancelled");
        assert_eq!(
            TerminationReason::MaxSessionExpired.to_string(),
            "max session expired"
        );
    }

    #[test]
    fn protocol_event_construction() {
        let event = ProtocolEvent {
            direction: Direction::Incoming,
            method: "tools/call".to_string(),
            content: serde_json::json!({"name": "calculator"}),
        };
        assert_eq!(event.direction, Direction::Incoming);
        assert_eq!(event.method, "tools/call");
    }

    #[test]
    fn actor_result_construction() {
        let result = ActorResult {
            actor_name: "mcp_poison".to_string(),
            termination: TerminationReason::TerminalPhaseReached,
            phases_completed: 2,
        };
        assert_eq!(result.actor_name, "mcp_poison");
        assert_eq!(result.phases_completed, 2);
    }

    #[test]
    fn await_extractor_construction() {
        let spec = AwaitExtractor {
            actor: "other_actor".to_string(),
            extractors: vec!["token".to_string(), "session_id".to_string()],
            timeout: Duration::from_secs(30),
        };
        assert_eq!(spec.actor, "other_actor");
        assert_eq!(spec.extractors.len(), 2);
        assert_eq!(spec.timeout, Duration::from_secs(30));
    }

    #[test]
    fn direction_serialization() {
        let json = serde_json::to_string(&Direction::Incoming).unwrap();
        assert_eq!(json, "\"incoming\"");

        let deserialized: Direction = serde_json::from_str("\"outgoing\"").unwrap();
        assert_eq!(deserialized, Direction::Outgoing);
    }

    #[test]
    fn termination_reason_serialization() {
        let json = serde_json::to_string(&TerminationReason::Cancelled).unwrap();
        assert_eq!(json, "\"cancelled\"");
    }
}
