//! Core execution engine for OATF-based attack scenarios.
//!
//! This module implements TJ-SPEC-013: the `PhaseEngine` state machine,
//! `PhaseLoop` event loop, `PhaseDriver` trait for protocol drivers,
//! and supporting types for trace capture, entry actions, and
//! synthesize validation.
//!
//! # Architecture
//!
//! ```text
//! PhaseLoop<D: PhaseDriver>
//! ├── PhaseEngine          — state machine (trigger eval, phase advance)
//! ├── SharedTrace          — append-only event trace
//! ├── ExtractorStore       — cross-actor extractor values
//! ├── watch::Sender        — publishes fresh extractors to driver
//! └── D                    — protocol-specific driver
//! ```
//!
//! Each protocol mode (MCP server/client, A2A, AG-UI) implements
//! `PhaseDriver`. The `PhaseLoop` provides the common event loop.

pub mod actions;
pub mod driver;
pub mod generation;
pub mod mcp_server;
pub mod phase;
pub mod phase_loop;
pub mod trace;
pub mod types;

// Re-export primary types for convenience.
pub use actions::EntryActionSender;
pub use driver::PhaseDriver;
pub use generation::validate_synthesized_output;
pub use mcp_server::{McpServerDriver, McpTransportEntryActionSender};
pub use phase::PhaseEngine;
pub use phase_loop::{ExtractorStore, PhaseLoop, PhaseLoopConfig};
pub use trace::{SharedTrace, TraceEntry};
pub use types::{
    ActorResult, AwaitExtractor, Direction, DriveResult, PhaseAction, ProtocolEvent,
    TerminationReason,
};
