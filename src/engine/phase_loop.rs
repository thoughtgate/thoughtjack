//! Core execution loop for OATF phase-based attack scenarios.
//!
//! `PhaseLoop<D>` owns the common phase machinery — event processing,
//! extractor capture, trigger evaluation, phase advancement, trace
//! append, and cross-actor extractor awaiting. It is generic over the
//! protocol-specific `PhaseDriver`.
//!
//! `ExtractorStore` provides thread-safe cross-actor extractor storage.
//!
//! See TJ-SPEC-013 §8.4 for the phase loop specification.
#![allow(clippy::redundant_pub_crate)]

use std::collections::HashMap;

use oatf::enums::ExtractorSource;
use oatf::primitives::{
    evaluate_extractor, extract_protocol, parse_event_qualifier, resolve_event_qualifier,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;
use crate::orchestration::store::ExtractorStore;

use super::actions::{self, EntryActionSender};
use super::driver::PhaseDriver;
use super::phase::PhaseEngine;
use super::trace::SharedTrace;
use super::types::{
    ActorResult, AwaitExtractor, Direction, PhaseAction, ProtocolEvent, TerminationReason,
};

/// Maximum number of buffered protocol events per phase.
///
/// Provides backpressure: if the phase loop cannot drain events fast
/// enough, drivers block on `send().await`. The capacity is generous
/// enough that backpressure should never trigger under normal operation.
const EVENT_CHANNEL_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveLoopAction {
    Stay,
    Advance,
    TransportClosed,
}

// ============================================================================
// Free functions for event processing
// ============================================================================
//
// These are free functions (not methods on PhaseLoop) to allow the borrow
// checker to see disjoint field accesses when used inside tokio::select!
// alongside the driver future.

/// Shared immutable context passed to free event-processing functions.
///
/// Bundles references that are shared across event processing calls,
/// avoiding excessive argument counts on free functions.
struct EventContext<'a> {
    trace: &'a SharedTrace,
    extractor_store: &'a ExtractorStore,
    extractors_tx: &'a watch::Sender<HashMap<String, String>>,
    actor_name: &'a str,
    protocol: &'a str,
    is_server_mode: bool,
}

/// Process a single event: trace append, extractor capture, trigger check.
fn process_protocol_event(
    evt: ProtocolEvent,
    phase_engine: &mut PhaseEngine,
    ctx: &EventContext<'_>,
) -> PhaseAction {
    // 1. Append to trace
    ctx.trace.append(
        ctx.actor_name,
        phase_engine.current_phase_name(),
        evt.direction,
        &evt.method,
        &evt.content,
    );

    // 2. Run extractors
    run_extractors(
        &evt,
        phase_engine,
        ctx.extractor_store,
        ctx.actor_name,
        ctx.is_server_mode,
    );

    // Publish updated extractors
    let _ = ctx.extractors_tx.send(build_interpolation_extractors(
        phase_engine,
        ctx.extractor_store,
    ));

    // 3. Check trigger — build SDK ProtocolEvent
    let (base_event, _) = parse_event_qualifier(&evt.method);
    let qualifier = resolve_event_qualifier(ctx.protocol, base_event, &evt.content);
    let oatf_event = oatf::ProtocolEvent {
        event_type: evt.method,
        qualifier,
        content: evt.content,
    };
    phase_engine.process_event(&oatf_event)
}

/// Drain any remaining buffered events after driver completes.
///
/// Stops processing immediately after the first `Advance` to avoid
/// running extractors in the wrong phase context.
fn drain_events(
    event_rx: &mut mpsc::Receiver<ProtocolEvent>,
    phase_engine: &mut PhaseEngine,
    ctx: &EventContext<'_>,
) -> PhaseAction {
    while let Ok(evt) = event_rx.try_recv() {
        if process_protocol_event(evt, phase_engine, ctx) == PhaseAction::Advance {
            return PhaseAction::Advance;
        }
    }
    PhaseAction::Stay
}

/// Run extractors from the current phase against a protocol event.
///
/// The `is_server_mode` flag controls the `Direction` → `ExtractorSource`
/// mapping. In server mode, incoming messages are requests and outgoing
/// messages are responses. In client mode, the mapping is reversed.
fn run_extractors(
    event: &ProtocolEvent,
    phase_engine: &mut PhaseEngine,
    extractor_store: &ExtractorStore,
    actor_name: &str,
    is_server_mode: bool,
) {
    let current_phase = phase_engine.current_phase;
    let phase = phase_engine.get_phase(current_phase);

    // Clone extractors to release the borrow on phase_engine
    let Some(extractors) = phase.extractors.clone() else {
        return;
    };

    for extractor in &extractors {
        // Server mode: Incoming = Request, Outgoing = Response
        // Client mode: Incoming = Response, Outgoing = Request
        let source = match (event.direction, is_server_mode) {
            (Direction::Incoming, true) | (Direction::Outgoing, false) => ExtractorSource::Request,
            (Direction::Outgoing, true) | (Direction::Incoming, false) => ExtractorSource::Response,
        };

        if let Some(value) = evaluate_extractor(extractor, &event.content, source) {
            // Local scope
            phase_engine
                .extractor_values
                .insert(extractor.name.clone(), value.clone());
            // Shared scope
            extractor_store.set(actor_name, &extractor.name, value);
        }
    }
}

/// Build the extractors map for SDK interpolation (local + qualified).
fn build_interpolation_extractors(
    phase_engine: &PhaseEngine,
    extractor_store: &ExtractorStore,
) -> HashMap<String, String> {
    let mut map = phase_engine.extractor_values.clone();
    map.extend(extractor_store.all_qualified());
    map
}

// ============================================================================
// PhaseLoop
// ============================================================================

/// Configuration for constructing a `PhaseLoop`.
///
/// Bundles the non-driver, non-engine parameters that the `PhaseLoop`
/// needs to operate.
///
/// Implements: TJ-SPEC-013 F-001
pub struct PhaseLoopConfig {
    /// Shared trace buffer for protocol event recording.
    pub trace: SharedTrace,
    /// Cross-actor extractor storage.
    pub extractor_store: ExtractorStore,
    /// Name of the actor this loop drives.
    pub actor_name: String,
    /// Per-phase `await_extractors` configuration (keyed by phase index).
    pub await_extractors_config: HashMap<usize, Vec<AwaitExtractor>>,
    /// Cooperative cancellation token.
    pub cancel: CancellationToken,
    /// Optional sender for entry actions (notifications, elicitations).
    pub entry_action_sender: Option<Box<dyn EntryActionSender>>,
}

/// Core execution loop for a single actor.
///
/// Generic over the protocol-specific `PhaseDriver`. Owns the phase
/// engine, trace, extractor store, and watch channel for publishing
/// fresh extractor values to the driver.
///
/// Implements: TJ-SPEC-013 F-001
pub struct PhaseLoop<D: PhaseDriver> {
    driver: D,
    phase_engine: PhaseEngine,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    actor_name: String,
    protocol: String,
    is_server_mode: bool,
    await_extractors_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    extractors_tx: watch::Sender<HashMap<String, String>>,
    entry_action_sender: Option<Box<dyn EntryActionSender>>,
}

impl<D: PhaseDriver> PhaseLoop<D> {
    /// Creates a new `PhaseLoop` for the given driver, engine, and config.
    ///
    /// Derives the protocol string from the actor's mode using the SDK.
    ///
    /// Implements: TJ-SPEC-013 F-001
    #[must_use]
    pub fn new(driver: D, phase_engine: PhaseEngine, config: PhaseLoopConfig) -> Self {
        let mode = &phase_engine.actor().mode;
        let protocol = extract_protocol(mode).to_string();
        let is_server_mode = mode.ends_with("_server");
        let (extractors_tx, _) = watch::channel(HashMap::new());

        Self {
            driver,
            phase_engine,
            trace: config.trace,
            extractor_store: config.extractor_store,
            actor_name: config.actor_name,
            protocol,
            is_server_mode,
            await_extractors_config: config.await_extractors_config,
            cancel: config.cancel,
            extractors_tx,
            entry_action_sender: config.entry_action_sender,
        }
    }

    /// Runs the phase loop to completion.
    ///
    /// Iterates through phases: awaits extractors, executes entry actions,
    /// runs the driver concurrently with event processing, and handles
    /// phase transitions.
    ///
    /// # Errors
    ///
    /// Returns `EngineError` if the driver or event processing fails.
    ///
    /// Implements: TJ-SPEC-013 F-001
    pub async fn run(&mut self) -> Result<ActorResult, EngineError> {
        loop {
            let phase_index = self.phase_engine.current_phase;
            if self.prepare_phase(phase_index).await {
                return Ok(self.build_result(TerminationReason::Cancelled));
            }
            let effective_state = self.phase_engine.effective_state();
            let (event_tx, mut event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

            // Run driver and event consumer concurrently.
            // drive_fut is scoped so its mutable borrow drops before on_phase_advanced.
            let phase_cancel = self.cancel.child_token();
            let ctx = EventContext {
                trace: &self.trace,
                extractor_store: &self.extractor_store,
                extractors_tx: &self.extractors_tx,
                actor_name: &self.actor_name,
                protocol: &self.protocol,
                is_server_mode: self.is_server_mode,
            };
            let action = {
                let extractors_rx = self.extractors_tx.subscribe();

                let drive_fut = self.driver.drive_phase(
                    phase_index,
                    &effective_state,
                    extractors_rx,
                    event_tx,
                    phase_cancel.clone(),
                );

                tokio::pin!(drive_fut);

                let mut phase_advancing = false;
                loop {
                    tokio::select! {
                        result = &mut drive_fut => {
                            let drive_result = result?;
                            let drained_action = drain_events(&mut event_rx, &mut self.phase_engine, &ctx);
                            break match drive_result {
                                super::types::DriveResult::Complete => {
                                    if phase_advancing || drained_action == PhaseAction::Advance {
                                        DriveLoopAction::Advance
                                    } else {
                                        DriveLoopAction::Stay
                                    }
                                }
                                super::types::DriveResult::TransportClosed => {
                                    DriveLoopAction::TransportClosed
                                }
                            };
                        }
                        event = event_rx.recv() => {
                            match event {
                                Some(evt) => {
                                    if process_protocol_event(evt, &mut self.phase_engine, &ctx)
                                        == PhaseAction::Advance
                                    {
                                        phase_advancing = true;
                                        phase_cancel.cancel();
                                    }
                                }
                                None => {
                                    break if phase_advancing {
                                        DriveLoopAction::Advance
                                    } else {
                                        DriveLoopAction::Stay
                                    };
                                }
                            }
                        }
                        () = self.cancel.cancelled() => {
                            // Cannot call self.build_result() here because
                            // self.driver is mutably borrowed by drive_fut.
                            let actor = self.phase_engine.actor();
                            let current = self.phase_engine.current_phase;
                            return Ok(ActorResult {
                                actor_name: self.actor_name.clone(),
                                termination: TerminationReason::Cancelled,
                                phases_completed: current,
                                total_phases: actor.phases.len(),
                                final_phase: actor
                                    .phases
                                    .get(current)
                                    .and_then(|p| p.name.clone()),
                            });
                        }
                    }
                }
            };

            if action == DriveLoopAction::TransportClosed {
                return Ok(self.build_result(TerminationReason::TransportClosed));
            }

            if action == DriveLoopAction::Advance {
                let to = self.phase_engine.advance_phase();
                self.driver.on_phase_advanced(phase_index, to).await?;
            }
            if self.phase_engine.is_terminal() {
                return Ok(self.build_result(TerminationReason::TerminalPhaseReached));
            }
        }
    }

    /// Prepare a phase: await cross-actor extractors, publish initial
    /// extractor values, and execute `on_enter` actions.
    #[allow(clippy::needless_pass_by_ref_mut, clippy::cognitive_complexity)]
    async fn prepare_phase(&mut self, phase_index: usize) -> bool {
        if let Some(await_specs) = self.await_extractors_config.get(&phase_index) {
            for spec in await_specs {
                tracing::debug!(
                    actor = %spec.actor,
                    extractors = ?spec.extractors,
                    timeout = ?spec.timeout,
                    "await_extractors: waiting for cross-actor extractors"
                );

                let deadline = tokio::time::Instant::now() + spec.timeout;

                let mut version_rx = self.extractor_store.subscribe();
                for extractor_name in &spec.extractors {
                    loop {
                        if let Some(value) = self.extractor_store.get(&spec.actor, extractor_name) {
                            let qualified = format!("{}.{}", spec.actor, extractor_name);
                            self.phase_engine.extractor_values.insert(qualified, value);
                            tracing::debug!(
                                actor = %spec.actor,
                                extractor = %extractor_name,
                                "await_extractors: resolved"
                            );
                            break;
                        }

                        tokio::select! {
                            result = version_rx.changed() => {
                                if result.is_err() { break; }
                            }
                            () = tokio::time::sleep_until(deadline) => {
                                tracing::warn!(
                                    actor = %spec.actor,
                                    extractor = %extractor_name,
                                    "await_extractors: timed out, proceeding without value"
                                );
                                break;
                            }
                            () = self.cancel.cancelled() => {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        let interpolation_extractors =
            build_interpolation_extractors(&self.phase_engine, &self.extractor_store);
        let _ = self.extractors_tx.send(interpolation_extractors.clone());

        let phase = self.phase_engine.get_phase(phase_index);
        if let Some(on_enter) = &phase.on_enter {
            actions::execute_entry_actions(
                on_enter,
                &interpolation_extractors,
                self.entry_action_sender.as_deref(),
            )
            .await;
        }

        false
    }

    /// Build a completion result for this actor with the given termination reason.
    fn build_result(&self, termination: TerminationReason) -> ActorResult {
        let current = self.phase_engine.current_phase;
        let actor = self.phase_engine.actor();
        ActorResult {
            actor_name: self.actor_name.clone(),
            termination,
            phases_completed: current,
            total_phases: actor.phases.len(),
            final_phase: actor.phases.get(current).and_then(|p| p.name.clone()),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ---- PhaseLoop integration tests with MockDriver ----

    /// A mock driver that sends a fixed sequence of events then returns.
    struct MockDriver {
        events: Vec<ProtocolEvent>,
    }

    #[async_trait::async_trait]
    impl PhaseDriver for MockDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            _extractors: watch::Receiver<HashMap<String, String>>,
            event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            for event in self.events.drain(..) {
                let _ = event_tx.send(event).await;
            }
            Ok(super::super::types::DriveResult::Complete)
        }
    }

    /// A mock driver that captures the extractor map it receives via the watch channel.
    struct ExtractorCapturingDriver {
        captured: Arc<Mutex<HashMap<String, String>>>,
    }

    #[async_trait::async_trait]
    impl PhaseDriver for ExtractorCapturingDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            extractors: watch::Receiver<HashMap<String, String>>,
            _event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            let snapshot = extractors.borrow().clone();
            *self.captured.lock().unwrap() = snapshot;
            Ok(super::super::types::DriveResult::Complete)
        }
    }

    /// A mock driver that emits an event with an empty-string field,
    /// then captures the published extractors.
    struct EmptyFieldDriver {
        captured: Arc<Mutex<HashMap<String, String>>>,
    }

    #[async_trait::async_trait]
    impl PhaseDriver for EmptyFieldDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            extractors: watch::Receiver<HashMap<String, String>>,
            event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            let _ = event_tx.send(ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator", "empty_field": ""}),
            }).await;
            // Small yield to allow event processing
            tokio::task::yield_now().await;
            let snapshot = extractors.borrow().clone();
            *self.captured.lock().unwrap() = snapshot;
            Ok(super::super::types::DriveResult::Complete)
        }
    }

    /// A mock driver that panics inside `drive_phase()`.
    struct PanicDriver;

    #[async_trait::async_trait]
    impl PhaseDriver for PanicDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            _extractors: watch::Receiver<HashMap<String, String>>,
            _event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            panic!("driver crashed unexpectedly");
        }
    }

    /// A mock driver that always returns an error.
    struct ErrorDriver;

    #[async_trait::async_trait]
    impl PhaseDriver for ErrorDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            _extractors: watch::Receiver<HashMap<String, String>>,
            _event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            Err(EngineError::Driver("mock driver error".to_string()))
        }
    }

    /// A mock driver that records `on_phase_advanced` calls.
    struct AdvanceRecordingDriver {
        events: Vec<ProtocolEvent>,
        advanced_calls: Arc<Mutex<Vec<(usize, usize)>>>,
    }

    #[async_trait::async_trait]
    impl PhaseDriver for AdvanceRecordingDriver {
        async fn drive_phase(
            &mut self,
            _phase_index: usize,
            _state: &serde_json::Value,
            _extractors: watch::Receiver<HashMap<String, String>>,
            event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            for event in self.events.drain(..) {
                let _ = event_tx.send(event).await;
            }
            Ok(super::super::types::DriveResult::Complete)
        }

        async fn on_phase_advanced(&mut self, from: usize, to: usize) -> Result<(), EngineError> {
            self.advanced_calls.lock().unwrap().push((from, to));
            Ok(())
        }
    }

    fn load_test_document(yaml: &str) -> oatf::Document {
        oatf::load(yaml)
            .expect("test YAML should be valid")
            .document
    }

    fn test_config(trace: SharedTrace) -> PhaseLoopConfig {
        PhaseLoopConfig {
            trace,
            extractor_store: ExtractorStore::new(),
            actor_name: "default".to_string(),
            await_extractors_config: HashMap::new(),
            cancel: CancellationToken::new(),
            entry_action_sender: None,
        }
    }

    #[tokio::test]
    async fn phase_loop_terminal_phase_completes() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let driver = MockDriver { events: vec![] };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace));

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.actor_name, "default");
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
    }

    #[tokio::test]
    async fn phase_loop_advances_on_trigger() {
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
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let driver = MockDriver {
            events: vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "test"}),
            }],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace.clone());
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
        assert_eq!(result.phases_completed, 1);

        // Verify trace captured the event
        assert_eq!(trace.len(), 1);
    }

    #[tokio::test]
    async fn phase_loop_captures_trace_entries() {
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
          event: tools/call
          count: 2
      - name: phase_two
"#,
        );

        let driver = MockDriver {
            events: vec![
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "tools/call".to_string(),
                    content: serde_json::json!({"name": "a"}),
                },
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "tools/call".to_string(),
                    content: serde_json::json!({"name": "b"}),
                },
            ],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace.clone());
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        phase_loop.run().await.unwrap();

        let entries = trace.snapshot();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].method, "tools/call");
        assert_eq!(entries[1].method, "tools/call");
        assert_eq!(entries[0].phase, "phase_one");
    }

    #[tokio::test]
    async fn phase_loop_cancellation_returns_cancelled() {
        // Driver that waits for cancellation
        struct WaitDriver;
        #[async_trait::async_trait]
        impl PhaseDriver for WaitDriver {
            async fn drive_phase(
                &mut self,
                _phase_index: usize,
                _state: &serde_json::Value,
                _extractors: watch::Receiver<HashMap<String, String>>,
                _event_tx: mpsc::Sender<ProtocolEvent>,
                cancel: CancellationToken,
            ) -> Result<super::super::types::DriveResult, EngineError> {
                cancel.cancelled().await;
                Ok(super::super::types::DriveResult::Complete)
            }
        }

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
          event: tools/call
          count: 999
      - name: phase_two
"#,
        );

        let cancel = CancellationToken::new();
        let config = PhaseLoopConfig {
            trace: SharedTrace::new(),
            extractor_store: ExtractorStore::new(),
            actor_name: "default".to_string(),
            await_extractors_config: HashMap::new(),
            cancel: cancel.clone(),
            entry_action_sender: None,
        };

        let engine = PhaseEngine::new(doc, 0);
        let mut phase_loop = PhaseLoop::new(WaitDriver, engine, config);

        // Cancel after a short delay
        let cancel_handle = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            cancel_handle.cancel();
        });

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::Cancelled);
        assert_eq!(result.actor_name, "default");
    }

    // ---- New tests ----

    #[tokio::test]
    async fn extractor_capture_local_scope() {
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
        extractors:
          - name: tool_name
            source: request
            type: json_path
            selector: "$.name"
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let driver = MockDriver {
            events: vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator"}),
            }],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace);
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
        // Verify extractor was captured locally
        assert_eq!(
            phase_loop.phase_engine.extractor_values.get("tool_name"),
            Some(&"calculator".to_string())
        );
    }

    #[tokio::test]
    async fn extractor_capture_cross_actor() {
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
        extractors:
          - name: tool_name
            source: request
            type: json_path
            selector: "$.name"
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let driver = MockDriver {
            events: vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "my_tool"}),
            }],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let extractor_store = ExtractorStore::new();
        let store_handle = extractor_store.clone();
        let config = PhaseLoopConfig {
            trace,
            extractor_store,
            actor_name: "test_actor".to_string(),
            await_extractors_config: HashMap::new(),
            cancel: CancellationToken::new(),
            entry_action_sender: None,
        };
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        phase_loop.run().await.unwrap();
        // Verify extractor is in the shared store (cross-actor)
        assert_eq!(
            store_handle.get("test_actor", "tool_name"),
            Some("my_tool".to_string())
        );
    }

    #[tokio::test]
    async fn drain_events_after_driver_completes() {
        // MockDriver emits events synchronously then returns Complete.
        // All events should be drained and appear in trace.
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let driver = MockDriver {
            events: vec![
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "resources/read".to_string(),
                    content: serde_json::json!({"uri": "file:///a.txt"}),
                },
                ProtocolEvent {
                    direction: Direction::Outgoing,
                    method: "resources/read".to_string(),
                    content: serde_json::json!({"contents": []}),
                },
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "tools/list".to_string(),
                    content: serde_json::json!({}),
                },
            ],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace.clone());
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        phase_loop.run().await.unwrap();

        let entries = trace.snapshot();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].method, "resources/read");
        assert_eq!(entries[1].method, "resources/read");
        assert_eq!(entries[2].method, "tools/list");
    }

    #[tokio::test]
    async fn drain_events_stops_on_advance() {
        // Two-phase doc with count=1. Driver sends 2 events — first triggers
        // advance, drain should process the second but stop due to advance.
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
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let driver = MockDriver {
            events: vec![
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "tools/call".to_string(),
                    content: serde_json::json!({"name": "calculator"}),
                },
                ProtocolEvent {
                    direction: Direction::Incoming,
                    method: "tools/call".to_string(),
                    content: serde_json::json!({"name": "second"}),
                },
            ],
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace.clone());
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
        // Both events should appear in trace (drain processes all remaining)
        assert!(!trace.is_empty());
    }

    /// A mock driver that provides a fixed set of events per phase index.
    struct PerPhaseDriver {
        /// Maps `phase_index` → events to send during that phase.
        phase_events: HashMap<usize, Vec<ProtocolEvent>>,
    }

    #[async_trait::async_trait]
    impl PhaseDriver for PerPhaseDriver {
        async fn drive_phase(
            &mut self,
            phase_index: usize,
            _state: &serde_json::Value,
            _extractors: watch::Receiver<HashMap<String, String>>,
            event_tx: mpsc::Sender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            if let Some(events) = self.phase_events.get_mut(&phase_index) {
                for event in events.drain(..) {
                    let _ = event_tx.send(event).await;
                }
            }
            Ok(super::super::types::DriveResult::Complete)
        }
    }

    #[tokio::test]
    async fn multi_phase_full_lifecycle() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: trust_building
        state:
          tools:
            - name: calculator
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: exploit
        state:
          tools:
            - name: calculator
              description: "modified"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 1
      - name: terminal
"#,
        );

        let mut phase_events = HashMap::new();
        phase_events.insert(
            0,
            vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator"}),
            }],
        );
        phase_events.insert(
            1,
            vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator"}),
            }],
        );

        let driver = PerPhaseDriver { phase_events };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace.clone());
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
        assert_eq!(result.phases_completed, 2);

        // Verify trace has events from both phases
        let entries = trace.snapshot();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].phase, "trust_building");
        assert_eq!(entries[1].phase, "exploit");
    }

    #[tokio::test]
    async fn driver_error_propagates() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace);
        let mut phase_loop = PhaseLoop::new(ErrorDriver, engine, config);

        let result = phase_loop.run().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("mock driver error"),
            "Expected 'mock driver error', got: {err}"
        );
    }

    #[test]
    fn server_vs_client_extractor_source() {
        // Server mode: Incoming → Request, Outgoing → Response
        // Client mode: Incoming → Response, Outgoing → Request
        use oatf::enums::ExtractorSource;

        // Server mode: Incoming maps to Request
        let (source_server_in, source_server_out) = (
            match (Direction::Incoming, true) {
                (Direction::Incoming, true) | (Direction::Outgoing, false) => {
                    ExtractorSource::Request
                }
                _ => ExtractorSource::Response,
            },
            match (Direction::Outgoing, true) {
                (Direction::Outgoing, true) | (Direction::Incoming, false) => {
                    ExtractorSource::Response
                }
                _ => ExtractorSource::Request,
            },
        );
        assert_eq!(source_server_in, ExtractorSource::Request);
        assert_eq!(source_server_out, ExtractorSource::Response);

        // Client mode: Incoming maps to Response
        let (source_client_in, source_client_out) = (
            match (Direction::Incoming, false) {
                (Direction::Incoming, true) | (Direction::Outgoing, false) => {
                    ExtractorSource::Request
                }
                _ => ExtractorSource::Response,
            },
            match (Direction::Outgoing, false) {
                (Direction::Outgoing, true) | (Direction::Incoming, false) => {
                    ExtractorSource::Response
                }
                _ => ExtractorSource::Request,
            },
        );
        assert_eq!(source_client_in, ExtractorSource::Response);
        assert_eq!(source_client_out, ExtractorSource::Request);
    }

    #[test]
    fn build_interpolation_extractors_merges() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let mut engine = PhaseEngine::new(doc, 0);
        engine
            .extractor_values
            .insert("local_key".to_string(), "local_val".to_string());

        let store = ExtractorStore::new();
        store.set("other_actor", "token", "abc123".to_string());

        let merged = build_interpolation_extractors(&engine, &store);
        assert_eq!(merged.get("local_key"), Some(&"local_val".to_string()));
        assert_eq!(merged.get("other_actor.token"), Some(&"abc123".to_string()));
    }

    #[tokio::test]
    async fn on_phase_advanced_called() {
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
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let calls = Arc::new(Mutex::new(Vec::new()));
        let driver = AdvanceRecordingDriver {
            events: vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator"}),
            }],
            advanced_calls: Arc::clone(&calls),
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace);
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        phase_loop.run().await.unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], (0, 1));
    }

    // ---- Edge case tests (EC-OATF-007, EC-OATF-008, EC-OATF-014) ----

    /// EC-OATF-007: Phase with no `state:` key — effective state inherits
    /// from the previous phase, driver runs fine.
    #[tokio::test]
    async fn ec_oatf_007_no_state_phase() {
        // Second phase has no state — should inherit from phase_one
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
          event: tools/call
          count: 1
      - name: no_state_phase
"#,
        );

        let mut phase_events = HashMap::new();
        phase_events.insert(
            0,
            vec![ProtocolEvent {
                direction: Direction::Incoming,
                method: "tools/call".to_string(),
                content: serde_json::json!({"name": "calculator"}),
            }],
        );

        let driver = PerPhaseDriver { phase_events };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let mut phase_loop = PhaseLoop::new(driver, engine, test_config(trace));

        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);
        // Phase 1 has no trigger (terminal) — completes at phase index 1
        assert_eq!(result.phases_completed, 1);
    }

    /// EC-OATF-008: Extractor captures empty string `""` — published on watch
    /// channel, not silently dropped.
    #[tokio::test]
    async fn ec_oatf_008_empty_string_extractor() {
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
        extractors:
          - name: empty_val
            source: request
            type: json_path
            selector: "$.empty_field"
        trigger:
          event: tools/call
          count: 1
      - name: phase_two
"#,
        );

        let captured = Arc::new(Mutex::new(HashMap::new()));
        let captured_clone = Arc::clone(&captured);

        let driver = EmptyFieldDriver {
            captured: captured_clone,
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace);
        let mut phase_loop = PhaseLoop::new(driver, engine, config);

        phase_loop.run().await.unwrap();

        // Verify the empty string was captured (not dropped)
        assert_eq!(
            phase_loop.phase_engine.extractor_values.get("empty_val"),
            Some(&String::new()),
            "empty string extractor should be captured, not dropped"
        );
    }

    /// EC-OATF-014: Driver that panics inside `drive_phase()` — `run()` returns
    /// Err, does not propagate panic to caller.
    #[tokio::test]
    async fn ec_oatf_014_driver_panic() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let config = test_config(trace);

        // Spawn the phase loop in a task so the panic is caught by JoinHandle
        let result = tokio::spawn(async move {
            let mut phase_loop = PhaseLoop::new(PanicDriver, engine, config);
            phase_loop.run().await
        })
        .await;

        // The JoinHandle should capture the panic (JoinError::is_panic())
        assert!(
            result.is_err(),
            "spawn should return Err for a panicked task"
        );
        let join_err = result.unwrap_err();
        assert!(
            join_err.is_panic(),
            "error should be a panic, not cancellation"
        );
    }

    #[tokio::test]
    async fn await_extractors_resolves() {
        let doc = load_test_document(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#,
        );

        let captured = Arc::new(Mutex::new(HashMap::new()));
        let driver = ExtractorCapturingDriver {
            captured: Arc::clone(&captured),
        };
        let engine = PhaseEngine::new(doc, 0);
        let trace = SharedTrace::new();
        let extractor_store = ExtractorStore::new();
        // Pre-seed the store with the value that will be awaited
        extractor_store.set("other_actor", "session_id", "sess-42".to_string());

        let mut await_config: HashMap<usize, Vec<AwaitExtractor>> = HashMap::new();
        await_config.insert(
            0,
            vec![AwaitExtractor {
                actor: "other_actor".to_string(),
                extractors: vec!["session_id".to_string()],
                timeout: std::time::Duration::from_secs(5),
            }],
        );

        let config = PhaseLoopConfig {
            trace,
            extractor_store,
            actor_name: "default".to_string(),
            await_extractors_config: await_config,
            cancel: CancellationToken::new(),
            entry_action_sender: None,
        };

        let mut phase_loop = PhaseLoop::new(driver, engine, config);
        let result = phase_loop.run().await.unwrap();
        assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

        // Verify the awaited extractor was resolved and available
        assert_eq!(
            phase_loop
                .phase_engine
                .extractor_values
                .get("other_actor.session_id"),
            Some(&"sess-42".to_string())
        );
    }
}
