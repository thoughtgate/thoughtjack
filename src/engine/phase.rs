//! Phase engine state machine.
//!
//! `PhaseEngine` wraps the OATF SDK's trigger evaluation and state
//! computation primitives to drive the phase state machine for a
//! single actor. It owns the current phase index, trigger state,
//! phase start time, and local extractor values.
//!
//! See TJ-SPEC-013 §8.1 for the state machine specification.

use std::collections::HashMap;
use std::time::Instant;

use oatf::primitives::{compute_effective_state, evaluate_trigger, extract_protocol};
use oatf::{Document, Phase, TriggerState};

use super::types::PhaseAction;

// ============================================================================
// PhaseEngine
// ============================================================================

/// Phase state machine for a single actor in an OATF document.
///
/// Manages phase advancement by delegating trigger evaluation to the
/// OATF SDK. The `PhaseLoop` calls `process_event()` for trigger
/// checks and `run_extractors()` separately for extractor capture.
///
/// Implements: TJ-SPEC-013 F-001
pub struct PhaseEngine {
    /// The loaded OATF document.
    pub(crate) document: Document,
    /// Index into `document.attack.execution.actors[]`.
    pub(crate) actor_index: usize,
    /// Current phase index within the actor's phases.
    pub(crate) current_phase: usize,
    /// Per-phase trigger state (count tracking), reset on advance.
    pub(crate) trigger_state: TriggerState,
    /// When the current phase was entered (for `after:` trigger).
    pub(crate) phase_start_time: Instant,
    /// Local extractor values captured during execution.
    pub(crate) extractor_values: HashMap<String, String>,
}

impl PhaseEngine {
    /// Creates a new `PhaseEngine` for the given actor in the document.
    ///
    /// # Panics
    ///
    /// Panics if `actor_index` is out of bounds for the document's actors,
    /// or if the actor has no phases.
    #[must_use]
    pub fn new(document: Document, actor_index: usize) -> Self {
        let actors = document
            .attack
            .execution
            .actors
            .as_ref()
            .expect("document should have actors after normalization");
        assert!(
            actor_index < actors.len(),
            "actor_index {actor_index} out of bounds (have {} actors)",
            actors.len()
        );
        assert!(
            !actors[actor_index].phases.is_empty(),
            "actor at index {actor_index} has no phases"
        );

        Self {
            document,
            actor_index,
            current_phase: 0,
            trigger_state: TriggerState::default(),
            phase_start_time: Instant::now(),
            extractor_values: HashMap::new(),
        }
    }

    /// Evaluates a protocol event against the current phase trigger.
    ///
    /// Returns `PhaseAction::Advance` if the trigger fires, or
    /// `PhaseAction::Stay` if it does not. Does **not** advance the
    /// phase — the caller (`PhaseLoop`) is responsible for calling
    /// `advance_phase()`.
    ///
    /// The trigger evaluation is fully delegated to the OATF SDK:
    /// event type matching, qualifier comparison, predicate evaluation,
    /// count tracking, and threshold checking.
    ///
    /// # Panics
    ///
    /// Panics if the document has no actors after normalization.
    ///
    /// Implements: TJ-SPEC-013 F-001
    pub fn process_event(&mut self, event: &oatf::ProtocolEvent) -> PhaseAction {
        // Access document fields directly (not via method) to allow
        // disjoint borrow of self.trigger_state alongside self.document.
        let actors = self
            .document
            .attack
            .execution
            .actors
            .as_deref()
            .expect("document should have actors after normalization");
        let phase = &actors[self.actor_index].phases[self.current_phase];

        let Some(trigger) = &phase.trigger else {
            return PhaseAction::Stay; // Terminal phase — no trigger
        };

        let protocol = extract_protocol(&actors[self.actor_index].mode);
        let elapsed = self.phase_start_time.elapsed();
        let result = evaluate_trigger(
            trigger,
            Some(event),
            elapsed,
            &mut self.trigger_state,
            protocol,
        );

        match result {
            oatf::TriggerResult::Advanced { .. } => PhaseAction::Advance,
            oatf::TriggerResult::NotAdvanced => PhaseAction::Stay,
        }
    }

    /// Computes the effective state at the current phase.
    ///
    /// Delegates to the SDK's `compute_effective_state()` which applies
    /// state inheritance by walking phases 0 through the current index.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn effective_state(&self) -> serde_json::Value {
        compute_effective_state(&self.actor().phases, self.current_phase)
    }

    /// Advances to the next phase, resetting per-phase state.
    ///
    /// Resets trigger count tracking and the phase elapsed timer.
    /// Returns the new phase index.
    ///
    /// Implements: TJ-SPEC-013 F-001
    pub fn advance_phase(&mut self) -> usize {
        self.current_phase += 1;
        self.trigger_state = TriggerState::default();
        self.phase_start_time = Instant::now();
        self.current_phase
    }

    /// Returns `true` when the current phase has no trigger (terminal phase).
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.get_phase(self.current_phase).trigger.is_none()
    }

    /// Returns a reference to the phase at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds.
    #[must_use]
    pub fn get_phase(&self, index: usize) -> &Phase {
        &self.actor().phases[index]
    }

    /// Returns the current phase index.
    #[must_use]
    pub const fn current_phase(&self) -> usize {
        self.current_phase
    }

    /// Returns the name of the current phase, or `"unnamed"` if none.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn current_phase_name(&self) -> &str {
        self.get_phase(self.current_phase)
            .name
            .as_deref()
            .unwrap_or("unnamed")
    }

    /// Returns a reference to the actor this engine operates on.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn actor(&self) -> &oatf::Actor {
        &self.actors_slice()[self.actor_index]
    }

    /// Returns the actors slice from the document.
    ///
    /// This method borrows only `self.document`, allowing callers
    /// to hold other disjoint borrows on `self` fields.
    fn actors_slice(&self) -> &[oatf::Actor] {
        self.document
            .attack
            .execution
            .actors
            .as_deref()
            .expect("document should have actors after normalization")
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to load a minimal OATF document for testing.
    fn load_test_document(yaml: &str) -> Document {
        oatf::load(yaml)
            .expect("test YAML should be valid")
            .document
    }

    fn two_phase_document() -> Document {
        load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: phase_one
        state:
          tools:
            - name: calculator
              description: "test tool"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 2
      - name: phase_two
"#,
        )
    }

    fn single_phase_terminal_document() -> Document {
        load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "A test tool"
          inputSchema:
            type: object
"#,
        )
    }

    #[test]
    fn new_engine_starts_at_phase_zero() {
        let doc = two_phase_document();
        let engine = PhaseEngine::new(doc, 0);
        assert_eq!(engine.current_phase(), 0);
        assert_eq!(engine.current_phase_name(), "phase_one");
    }

    #[test]
    fn is_terminal_on_triggerless_phase() {
        let doc = two_phase_document();
        let mut engine = PhaseEngine::new(doc, 0);
        assert!(!engine.is_terminal());

        engine.advance_phase();
        assert!(engine.is_terminal());
    }

    #[test]
    fn single_phase_document_is_terminal() {
        let doc = single_phase_terminal_document();
        let engine = PhaseEngine::new(doc, 0);
        assert!(engine.is_terminal());
    }

    #[test]
    fn advance_resets_trigger_state() {
        let doc = two_phase_document();
        let mut engine = PhaseEngine::new(doc, 0);

        // Manually set trigger state to simulate counts
        engine.trigger_state.event_count = 5;

        let new_index = engine.advance_phase();
        assert_eq!(new_index, 1);
        assert_eq!(engine.current_phase(), 1);
        assert_eq!(engine.trigger_state.event_count, 0);
    }

    #[test]
    fn effective_state_returns_value() {
        let doc = two_phase_document();
        let engine = PhaseEngine::new(doc, 0);
        let state = engine.effective_state();
        // State should be a JSON value (may be null for phases without state)
        assert!(state.is_null() || state.is_object());
    }

    #[test]
    fn process_event_stays_on_no_match() {
        let doc = two_phase_document();
        let mut engine = PhaseEngine::new(doc, 0);

        // Send an event that doesn't match the trigger
        let event = oatf::ProtocolEvent {
            event_type: "resources/read".to_string(),
            qualifier: None,
            content: serde_json::json!({}),
        };

        let action = engine.process_event(&event);
        assert_eq!(action, PhaseAction::Stay);
    }

    #[test]
    fn process_event_advances_after_count() {
        let doc = two_phase_document();
        let mut engine = PhaseEngine::new(doc, 0);

        let event = oatf::ProtocolEvent {
            event_type: "tools/call".to_string(),
            qualifier: None,
            content: serde_json::json!({}),
        };

        // First call — count=1 of 2
        let action = engine.process_event(&event);
        assert_eq!(action, PhaseAction::Stay);

        // Second call — count=2 of 2, should advance
        let action = engine.process_event(&event);
        assert_eq!(action, PhaseAction::Advance);
    }

    #[test]
    fn process_event_stays_on_terminal_phase() {
        let doc = single_phase_terminal_document();
        let mut engine = PhaseEngine::new(doc, 0);

        let event = oatf::ProtocolEvent {
            event_type: "tools/call".to_string(),
            qualifier: None,
            content: serde_json::json!({}),
        };

        // Terminal phase has no trigger — always stays
        let action = engine.process_event(&event);
        assert_eq!(action, PhaseAction::Stay);
    }

    #[test]
    fn actor_returns_correct_actor() {
        let doc = two_phase_document();
        let engine = PhaseEngine::new(doc, 0);
        let actor = engine.actor();
        assert_eq!(actor.name, "default");
    }
}
