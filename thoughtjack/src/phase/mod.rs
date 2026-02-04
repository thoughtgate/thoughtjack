//! Phase engine (TJ-SPEC-003)
//!
//! Implements the attack phase state machine for orchestrating
//! multi-stage adversarial scenarios. The engine manages linear
//! phase transitions driven by events, timers, and content matching.
//!
//! # Architecture
//!
//! - [`PhaseState`] — Lock-free atomic state (current phase, event counters, timing)
//! - [`PhaseEngine`] — Orchestrator (event recording, trigger evaluation, transitions)
//! - [`EffectiveState`] — Computed server state (baseline + current phase diff)
//! - [`trigger`] — Trigger evaluation and duration parsing

pub mod effective;
pub mod engine;
pub mod state;
pub mod trigger;

pub use effective::EffectiveState;
pub use engine::PhaseEngine;
pub use state::{EventType, PhaseState, PhaseTransition};
pub use trigger::TriggerResult;
