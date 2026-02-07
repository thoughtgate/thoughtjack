//! Observability module (TJ-SPEC-008)
//!
//! Logging, metrics, and structured event infrastructure for monitoring
//! `ThoughtJack` operations during security tests.

pub mod events;
pub mod logging;
pub mod metrics;

pub use events::{Event, EventEmitter, RunSummary, StopReason, TriggerInfo};
pub use logging::{LogFormat, init_logging};
pub use metrics::init_metrics;
