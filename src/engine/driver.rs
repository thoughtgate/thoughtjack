//! `PhaseDriver` trait for protocol-specific execution.
//!
//! Each protocol mode (MCP server, MCP client, A2A server, A2A client,
//! AG-UI client) implements this trait to define how it sends requests,
//! opens streams, or listens for connections during a phase.
//!
//! See TJ-SPEC-013 §8.4 for the trait specification.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;

use super::types::DriveResult;
use super::types::ProtocolEvent;

// ============================================================================
// PhaseDriver
// ============================================================================

/// Protocol-specific driver for phase execution.
///
/// Defines how to execute a phase's protocol-specific work (send
/// requests, open streams, listen for connections) and how to deliver
/// protocol events to the `PhaseLoop`.
///
/// # Driver Implementation Pattern
///
/// 1. In `drive_phase()`: perform protocol I/O, emit events on `event_tx`,
///    respect `cancel` token.
/// 2. Server-mode: `extractors.borrow().clone()` per request for fresh values.
/// 3. Client-mode: `extractors.borrow().clone()` once at start.
/// 4. Response dispatch uses `oatf::select_response()` for ordered matching.
/// 5. Template interpolation uses `oatf::interpolate_template()` /
///    `oatf::interpolate_value()`.
///
/// # Errors
///
/// Implementations should return `EngineError::Driver` for protocol-level
/// failures. The `PhaseLoop` propagates these to the caller.
///
/// Implements: TJ-SPEC-013 F-001
#[async_trait]
pub trait PhaseDriver: Send {
    /// Execute the phase's protocol-specific work.
    ///
    /// Called once when a phase is entered. The driver sends requests,
    /// opens streams, or waits for connections depending on the protocol.
    /// Each protocol event (incoming or outgoing) is sent on `event_tx`.
    ///
    /// `extractors` is a watch channel receiver that provides the latest
    /// interpolation extractors map. The `PhaseLoop` publishes an updated
    /// map after each event's extractor capture.
    ///
    /// Returns `DriveResult::Complete` when the phase's protocol work is
    /// finished. Server-mode drivers run until `cancel` fires.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on protocol-level failures.
    async fn drive_phase(
        &mut self,
        phase_index: usize,
        state: &serde_json::Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError>;

    /// Called when a phase advances.
    ///
    /// Protocol-specific cleanup: close streams, update exposed state, etc.
    /// The default implementation is a no-op.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` if cleanup fails.
    async fn on_phase_advanced(&mut self, _from: usize, _to: usize) -> Result<(), EngineError> {
        Ok(())
    }
}
