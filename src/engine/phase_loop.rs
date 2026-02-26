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

use dashmap::DashMap;
use oatf::enums::ExtractorSource;
use oatf::primitives::{
    evaluate_extractor, extract_protocol, parse_event_qualifier, resolve_event_qualifier,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;

use super::actions::{self, EntryActionSender};
use super::driver::PhaseDriver;
use super::phase::PhaseEngine;
use super::trace::SharedTrace;
use super::types::{
    ActorResult, AwaitExtractor, Direction, PhaseAction, ProtocolEvent, TerminationReason,
};

// ============================================================================
// ExtractorStore
// ============================================================================

/// Thread-safe cross-actor extractor storage.
///
/// Stores extractor values keyed by `(actor_name, extractor_name)`.
/// Used by the `PhaseLoop` to publish captured values and by other
/// actors to read them for cross-actor interpolation.
///
/// Implements: TJ-SPEC-015 F-001
#[derive(Clone, Default)]
pub struct ExtractorStore {
    store: std::sync::Arc<DashMap<(String, String), String>>,
}

impl ExtractorStore {
    /// Creates a new empty extractor store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an extractor value for a given actor.
    pub fn set(&self, actor: &str, name: &str, value: String) {
        self.store
            .insert((actor.to_string(), name.to_string()), value);
    }

    /// Gets an extractor value for a given actor.
    #[must_use]
    pub fn get(&self, actor: &str, name: &str) -> Option<String> {
        self.store
            .get(&(actor.to_string(), name.to_string()))
            .map(|v| v.value().clone())
    }

    /// Returns all extractors as qualified `actor_name.extractor_name` keys.
    ///
    /// Used to build the interpolation extractors map per SDK §5.5.
    #[must_use]
    pub fn all_qualified(&self) -> HashMap<String, String> {
        self.store
            .iter()
            .map(|entry| {
                let (actor, name) = entry.key();
                (format!("{actor}.{name}"), entry.value().clone())
            })
            .collect()
    }
}

impl std::fmt::Debug for ExtractorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractorStore")
            .field("entries", &self.store.len())
            .finish()
    }
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
    event_rx: &mut mpsc::UnboundedReceiver<ProtocolEvent>,
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
        let is_server_mode = mode.contains("server");
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
            self.prepare_phase(phase_index).await;
            let effective_state = self.phase_engine.effective_state();
            let (event_tx, mut event_rx) = mpsc::unbounded_channel();

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

                loop {
                    tokio::select! {
                        result = &mut drive_fut => {
                            result?;
                            break drain_events(&mut event_rx, &mut self.phase_engine, &ctx);
                        }
                        event = event_rx.recv() => {
                            match event {
                                Some(evt) => {
                                    if process_protocol_event(evt, &mut self.phase_engine, &ctx)
                                        == PhaseAction::Advance
                                    {
                                        phase_cancel.cancel();
                                        break PhaseAction::Advance;
                                    }
                                }
                                None => {
                                    break PhaseAction::Stay;
                                }
                            }
                        }
                        () = self.cancel.cancelled() => {
                            return Ok(ActorResult {
                                actor_name: self.actor_name.clone(),
                                termination: TerminationReason::Cancelled,
                                phases_completed: self.phase_engine.current_phase,
                            });
                        }
                    }
                }
            };

            if action == PhaseAction::Advance {
                let to = self.phase_engine.advance_phase();
                self.driver.on_phase_advanced(phase_index, to).await?;
            }
            if self.phase_engine.is_terminal() {
                return Ok(self.terminal_result());
            }
        }
    }

    /// Prepare a phase: await cross-actor extractors, publish initial
    /// extractor values, and execute `on_enter` actions.
    #[allow(clippy::needless_pass_by_ref_mut)] // &mut self needed for Send (D not Sync)
    async fn prepare_phase(&mut self, phase_index: usize) {
        if let Some(await_specs) = self.await_extractors_config.get(&phase_index) {
            for spec in await_specs {
                tracing::debug!(
                    actor = %spec.actor,
                    extractors = ?spec.extractors,
                    timeout = ?spec.timeout,
                    "await_extractors: waiting for cross-actor extractors (placeholder)"
                );
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
    }

    /// Build the terminal completion result for this actor.
    fn terminal_result(&self) -> ActorResult {
        ActorResult {
            actor_name: self.actor_name.clone(),
            termination: TerminationReason::TerminalPhaseReached,
            phases_completed: self.phase_engine.current_phase,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ExtractorStore tests ----

    #[test]
    fn extractor_store_set_and_get() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "abc123".to_string());
        assert_eq!(store.get("actor1", "token"), Some("abc123".to_string()));
    }

    #[test]
    fn extractor_store_get_missing() {
        let store = ExtractorStore::new();
        assert_eq!(store.get("actor1", "token"), None);
    }

    #[test]
    fn extractor_store_overwrite() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "old".to_string());
        store.set("actor1", "token", "new".to_string());
        assert_eq!(store.get("actor1", "token"), Some("new".to_string()));
    }

    #[test]
    fn extractor_store_all_qualified() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "abc".to_string());
        store.set("actor2", "session", "xyz".to_string());

        let qualified = store.all_qualified();
        assert_eq!(qualified.get("actor1.token"), Some(&"abc".to_string()));
        assert_eq!(qualified.get("actor2.session"), Some(&"xyz".to_string()));
        assert_eq!(qualified.len(), 2);
    }

    #[test]
    fn extractor_store_clone_shares_data() {
        let store = ExtractorStore::new();
        let store2 = store.clone();

        store.set("actor1", "token", "abc".to_string());
        assert_eq!(store2.get("actor1", "token"), Some("abc".to_string()));
    }

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
            event_tx: mpsc::UnboundedSender<ProtocolEvent>,
            _cancel: CancellationToken,
        ) -> Result<super::super::types::DriveResult, EngineError> {
            for event in self.events.drain(..) {
                let _ = event_tx.send(event);
            }
            Ok(super::super::types::DriveResult::Complete)
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

        // Driver that waits for cancellation
        struct WaitDriver;
        #[async_trait::async_trait]
        impl PhaseDriver for WaitDriver {
            async fn drive_phase(
                &mut self,
                _phase_index: usize,
                _state: &serde_json::Value,
                _extractors: watch::Receiver<HashMap<String, String>>,
                _event_tx: mpsc::UnboundedSender<ProtocolEvent>,
                cancel: CancellationToken,
            ) -> Result<super::super::types::DriveResult, EngineError> {
                cancel.cancelled().await;
                Ok(super::super::types::DriveResult::Complete)
            }
        }

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
}
