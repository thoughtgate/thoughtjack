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

use oatf::primitives::{compute_effective_state, evaluate_trigger};
use oatf::{Document, Phase, TriggerState};

use crate::loader::document_actors;

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
        let actors = document_actors(&document);
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
        // Use document_actors() instead of self.actors_slice() to allow
        // disjoint borrow of self.trigger_state alongside self.document.
        let actors = document_actors(&self.document);
        let phase = &actors[self.actor_index].phases[self.current_phase];

        let Some(trigger) = &phase.trigger else {
            return PhaseAction::Stay; // Terminal phase — no trigger
        };

        let elapsed = self.phase_start_time.elapsed();
        let result = evaluate_trigger(trigger, Some(event), elapsed, &mut self.trigger_state);

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
        let phases = &self.actor().phases;
        if self.current_phase >= phases.len() {
            return true;
        }
        phases[self.current_phase].trigger.is_none()
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
        self.actor()
            .phases
            .get(self.current_phase)
            .and_then(|p| p.name.as_deref())
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
        document_actors(&self.document)
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

    // ---- New tests ----

    #[test]
    fn effective_state_merges_across_phases() {
        // In OATF, state and phases are mutually exclusive at the execution level,
        // but each phase can have its own state. effective_state() uses
        // compute_effective_state which walks phases 0..=current.
        let doc = load_test_document(
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
            - name: base_tool
              description: "from phase one"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
        state:
          tools:
            - name: override_tool
              description: "from phase two"
              inputSchema:
                type: object
"#,
        );

        // At phase 0, state should be from phase_one
        let engine = PhaseEngine::new(doc, 0);
        let state = engine.effective_state();
        assert!(state.is_object());
        let tools = state.get("tools").expect("state should have tools");
        let tool_name = tools[0]
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        assert_eq!(tool_name, "base_tool");
    }

    #[test]
    fn qualified_event_matches_trigger() {
        // Qualifier separator is ':' (not brackets) — e.g., "tools/call:calculator"
        let doc = load_test_document(
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
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: "tools/call:calculator"
          count: 1
      - name: phase_two
"#,
        );

        let mut engine = PhaseEngine::new(doc, 0);

        // Non-matching qualifier should stay
        let non_match = oatf::ProtocolEvent {
            event_type: "tools/call:other_tool".to_string(),
            content: serde_json::json!({"name": "other_tool"}),
        };
        assert_eq!(engine.process_event(&non_match), PhaseAction::Stay);

        // Matching qualifier should advance
        let matching = oatf::ProtocolEvent {
            event_type: "tools/call:calculator".to_string(),
            content: serde_json::json!({"name": "calculator"}),
        };
        assert_eq!(engine.process_event(&matching), PhaseAction::Advance);
    }

    #[test]
    fn count_threshold_exact() {
        let doc = load_test_document(
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
            - name: test_tool
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 3
      - name: phase_two
"#,
        );

        let mut engine = PhaseEngine::new(doc, 0);
        let event = oatf::ProtocolEvent {
            event_type: "tools/call".to_string(),
            content: serde_json::json!({}),
        };

        // Events 1 and 2 should stay
        assert_eq!(engine.process_event(&event), PhaseAction::Stay);
        assert_eq!(engine.process_event(&event), PhaseAction::Stay);
        // Event 3 should advance
        assert_eq!(engine.process_event(&event), PhaseAction::Advance);
    }

    #[test]
    fn advance_beyond_last_phase_marks_terminal() {
        let doc = load_test_document(
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
            - name: test_tool
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let mut engine = PhaseEngine::new(doc, 0);
        // Advance to phase_two (terminal)
        let new_idx = engine.advance_phase();
        assert_eq!(new_idx, 1);
        assert!(engine.is_terminal());

        // Terminal phase — process_event should Stay (no trigger)
        let event = oatf::ProtocolEvent {
            event_type: "tools/call".to_string(),
            content: serde_json::json!({}),
        };
        assert_eq!(engine.process_event(&event), PhaseAction::Stay);

        // Advancing beyond last phase should be treated as terminal completion
        // and must not panic in is_terminal().
        let beyond = engine.advance_phase();
        assert_eq!(beyond, 2);
        assert!(engine.is_terminal());
    }

    #[test]
    fn effective_state_chain_three_phases() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: phase_zero
        state:
          tools:
            - name: tool_0
              description: "phase zero tool"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: phase_one
        state:
          tools:
            - name: tool_1
              description: "phase one tool"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
        state:
          tools:
            - name: tool_2
              description: "phase two tool"
              inputSchema:
                type: object
"#,
        );

        let mut engine = PhaseEngine::new(doc, 0);

        // Phase 0 state
        let state0 = engine.effective_state();
        let tools0 = state0.get("tools").unwrap();
        assert_eq!(tools0[0]["name"], "tool_0");

        // Advance to phase 1
        engine.advance_phase();
        let state1 = engine.effective_state();
        let tools1 = state1.get("tools").unwrap();
        assert_eq!(tools1[0]["name"], "tool_1");

        // Advance to phase 2
        engine.advance_phase();
        let state2 = engine.effective_state();
        let tools2 = state2.get("tools").unwrap();
        assert_eq!(tools2[0]["name"], "tool_2");
    }
}
