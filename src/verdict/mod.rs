//! Verdict evaluation and output pipeline.
//!
//! This module implements TJ-SPEC-014: grace period observation,
//! indicator evaluation against the protocol trace, attack-level
//! verdict computation, and structured output formatting.
//!
//! # Architecture
//!
//! ```text
//! Terminal Phase → GracePeriod → Trace Snapshot
//!                                     ↓
//!                     ┌───────────────────────────────┐
//!                     │  Evaluation Pipeline           │
//!                     │  for each indicator:           │
//!                     │    1. filter_trace_for_indicator│
//!                     │    2. evaluate_indicator (SDK)  │
//!                     │    3. aggregate per-indicator   │
//!                     │  compute_verdict (SDK)          │
//!                     └───────────────┬───────────────┘
//!                                     ↓
//!                     ┌───────────────────────────────┐
//!                     │  Output                        │
//!                     │  • JSON verdict (--output)     │
//!                     │  • Human summary (stderr)      │
//!                     │  • Exit code                   │
//!                     └───────────────────────────────┘
//! ```

pub mod evaluation;
pub mod grace;
pub mod output;

// Re-export primary types for convenience.
pub use evaluation::{ActorInfo, EvaluationConfig, evaluate_verdict, extract_protocol};
pub use grace::{GracePeriodState, resolve_grace_period};
pub use output::{
    ActorStatus, ExecutionSummary, VerdictOutput, build_verdict_output, print_human_summary,
    verdict_exit_code, write_json_verdict,
};
