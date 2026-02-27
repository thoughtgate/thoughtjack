//! Multi-actor orchestration layer.
//!
//! Manages concurrent actor lifecycle, server readiness gating,
//! cross-actor extractor sharing, and coordinated shutdown.
//!
//! See TJ-SPEC-015 for the full orchestration specification.

pub mod gate;
pub mod orchestrator;
pub mod runner;
pub mod store;

// Re-export primary types.
pub use gate::ReadinessGate;
pub use orchestrator::{ActorOutcome, OrchestratorResult, orchestrate};
pub use runner::{ActorConfig, build_actor_config, run_actor};
pub use store::ExtractorStore;
