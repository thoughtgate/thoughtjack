//! Multi-actor orchestrator.
//!
//! `orchestrate()` spawns one task per actor, manages the readiness gate,
//! coordinates shutdown (client-done → grace → cancel servers), and
//! collects results into an `OrchestratorResult`.
//!
//! See TJ-SPEC-015 §3 for the orchestration lifecycle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::engine::trace::SharedTrace;
use crate::engine::types::{ActorResult, AwaitExtractor};
use crate::error::EngineError;
use crate::loader::LoadedDocument;
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::orchestration::gate::ReadinessGate;
use crate::orchestration::runner::{ActorConfig, run_actor};
use crate::orchestration::store::ExtractorStore;

/// Default timeout for waiting for server readiness (fallback if not configured).
const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

// ============================================================================
// Types
// ============================================================================

/// Result of a complete orchestration run.
///
/// Implements: TJ-SPEC-015 F-004
pub struct OrchestratorResult {
    /// Per-actor outcomes (completion order; use `actor_name()` to correlate).
    pub outcomes: Vec<ActorOutcome>,
    /// Shared trace with events from all actors.
    pub trace: SharedTrace,
}

/// Outcome of a single actor's execution.
///
/// Implements: TJ-SPEC-015 F-004
#[derive(Debug)]
pub enum ActorOutcome {
    /// Actor completed successfully.
    Success(ActorResult),
    /// Actor terminated with an error.
    Error {
        /// Actor name.
        actor_name: String,
        /// Error description.
        error: String,
    },
    /// Actor task panicked.
    Panic {
        /// Actor name.
        actor_name: String,
    },
}

impl ActorOutcome {
    /// Returns the actor name for this outcome.
    #[must_use]
    pub fn actor_name(&self) -> &str {
        match self {
            Self::Success(r) => &r.actor_name,
            Self::Error { actor_name, .. } | Self::Panic { actor_name } => actor_name,
        }
    }
}

/// Return value from a completed actor task inside a `JoinSet`.
///
/// Bundles the actor's metadata with its execution result so the
/// orchestrator can identify which actor finished.
struct ActorTaskResult {
    actor_name: String,
    is_server: bool,
    result: Result<ActorResult, EngineError>,
}

// ============================================================================
// orchestrate()
// ============================================================================

/// Runs a multi-actor orchestration to completion.
///
/// Startup sequence (TJ-SPEC-015 §3.1):
/// 1. Partition actors into servers and clients.
/// 2. Create shared state (`SharedTrace`, `ExtractorStore`).
/// 3. Create `ReadinessGate` (if any servers).
/// 4. Spawn all actor tasks.
/// 5. Wait for server readiness (if any).
/// 6. Wait for completion: all clients done → grace → cancel servers.
///
/// # Panics
///
/// Panics if the loaded document has no actors after normalization.
///
/// # Errors
///
/// Returns `EngineError` if a critical error prevents orchestration
/// (e.g., readiness gate timeout). Individual actor errors are
/// collected in `OrchestratorResult::outcomes`.
///
/// Implements: TJ-SPEC-015 F-004
pub async fn orchestrate(
    loaded: &LoadedDocument,
    config: &ActorConfig,
    events: &Arc<EventEmitter>,
    cancel: CancellationToken,
) -> Result<OrchestratorResult, EngineError> {
    let actors = loaded
        .document
        .attack
        .execution
        .actors
        .as_ref()
        .expect("document should have actors after normalization");

    // 1. Partition into servers and clients
    let mut server_count = 0;
    let mut client_count = 0;
    let mut server_names = Vec::new();

    for actor in actors {
        if actor.mode.contains("server") {
            server_count += 1;
            server_names.push(actor.name.clone());
        } else {
            client_count += 1;
        }
    }

    events.emit(ThoughtJackEvent::OrchestratorStarted {
        actor_count: actors.len(),
        server_count,
        client_count,
    });

    // 2. Create shared state
    let trace = SharedTrace::new();
    let extractor_store = ExtractorStore::new();

    // 3. Create ReadinessGate (only if servers exist)
    let (gate, ready_txs) = if server_names.is_empty() {
        (None, Vec::new())
    } else {
        let (gate, txs) = ReadinessGate::new(&server_names);
        (Some(gate), txs)
    };

    // Build a lookup from actor_name → ready_tx
    let mut ready_tx_map: HashMap<String, tokio::sync::oneshot::Sender<()>> =
        ready_txs.into_iter().collect();

    // 4. Spawn all actor tasks into a JoinSet
    let join_set = spawn_actor_tasks(
        loaded,
        config,
        &trace,
        &extractor_store,
        &cancel,
        events,
        gate.as_ref(),
        &mut ready_tx_map,
    );

    // 5. Wait for server readiness
    if let Some(gate) = gate {
        let start = std::time::Instant::now();
        let readiness_timeout = if config.readiness_timeout.is_zero() {
            DEFAULT_READINESS_TIMEOUT
        } else {
            config.readiness_timeout
        };
        match gate.wait_all_ready(readiness_timeout).await {
            Ok(()) => {
                events.emit(ThoughtJackEvent::ReadinessGateOpen {
                    server_count,
                    #[allow(clippy::cast_possible_truncation)]
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(gate_err) => {
                tracing::error!(%gate_err, "readiness gate failed");
                events.emit(ThoughtJackEvent::ReadinessGateTimeout {
                    not_ready: server_names.clone(),
                });
                cancel.cancel();
                return drain_join_set(join_set, trace, events).await;
            }
        }
    }

    // 6. Wait for completion with shutdown coordination
    wait_for_completion(join_set, &trace, config, &cancel, events, client_count).await
}

/// Spawns one task per actor into a `JoinSet`.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
fn spawn_actor_tasks(
    loaded: &LoadedDocument,
    config: &ActorConfig,
    trace: &SharedTrace,
    extractor_store: &ExtractorStore,
    cancel: &CancellationToken,
    events: &Arc<EventEmitter>,
    gate: Option<&ReadinessGate>,
    ready_tx_map: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
) -> JoinSet<ActorTaskResult> {
    let actors = loaded
        .document
        .attack
        .execution
        .actors
        .as_ref()
        .expect("document should have actors after normalization");

    let mut join_set: JoinSet<ActorTaskResult> = JoinSet::new();

    for (i, actor) in actors.iter().enumerate() {
        let is_server = actor.mode.contains("server");
        let actor_name = actor.name.clone();

        let await_cfg: HashMap<usize, Vec<AwaitExtractor>> = loaded
            .await_extractors
            .iter()
            .filter(|((name, _), _)| name == &actor_name)
            .map(|((_, phase_idx), specs)| (*phase_idx, specs.clone()))
            .collect();

        let ready_tx = ready_tx_map.remove(&actor_name);
        let gate_rx = if is_server {
            None
        } else {
            gate.map(ReadinessGate::subscribe)
        };

        let doc = loaded.document.clone();
        let cfg = config.clone();
        let tr = trace.clone();
        let es = extractor_store.clone();
        let actor_cancel = cancel.child_token();
        let task_events = Arc::clone(events);

        join_set.spawn(async move {
            let result = run_actor(
                i,
                doc,
                &cfg,
                tr,
                es,
                await_cfg,
                actor_cancel,
                ready_tx,
                gate_rx,
                &task_events,
            )
            .await;
            ActorTaskResult {
                actor_name,
                is_server,
                result,
            }
        });
    }

    join_set
}

/// Waits for all actor tasks to complete with proper shutdown coordination.
///
/// Shutdown rules (TJ-SPEC-015 §3.2):
/// - When all clients complete → start grace period → cancel servers
/// - Zero-client fallback: all servers done → start grace period
/// - Max-session timeout → cancel all
#[allow(clippy::cognitive_complexity)]
async fn wait_for_completion(
    mut join_set: JoinSet<ActorTaskResult>,
    trace: &SharedTrace,
    config: &ActorConfig,
    cancel: &CancellationToken,
    events: &EventEmitter,
    total_clients: usize,
) -> Result<OrchestratorResult, EngineError> {
    let mut outcomes: Vec<ActorOutcome> = Vec::with_capacity(join_set.len());
    let mut clients_done = 0;
    let max_session_deadline = tokio::time::Instant::now() + config.max_session;

    loop {
        if join_set.is_empty() {
            break;
        }

        tokio::select! {
            Some(join_result) = join_set.join_next() => {
                let (outcome, is_server) = unpack_join_result(join_result, events);

                if is_server == Some(false) {
                    clients_done += 1;
                }

                outcomes.push(outcome);

                // Check shutdown conditions
                if total_clients > 0 && clients_done >= total_clients {
                    tracing::info!("all client actors completed, starting grace period");
                    apply_grace_and_cancel(config, cancel, events).await;
                } else if total_clients == 0 && join_set.is_empty() {
                    tracing::info!("all server actors completed (zero-client mode)");
                    apply_grace_and_cancel(config, cancel, events).await;
                }
            }
            () = tokio::time::sleep_until(max_session_deadline) => {
                tracing::warn!("max session expired, cancelling all actors");
                events.emit(ThoughtJackEvent::OrchestratorShutdown {
                    reason: "max_session_expired".to_string(),
                });
                cancel.cancel();
            }
            () = cancel.cancelled() => {
                tracing::info!("orchestrator cancelled");
                break;
            }
        }
    }

    // Drain any remaining tasks (after cancel or max session)
    join_set.abort_all();
    while let Some(join_result) = join_set.join_next().await {
        let (outcome, _) = unpack_join_result(join_result, events);
        outcomes.push(outcome);
    }

    emit_completion_summary(&outcomes, events);

    Ok(OrchestratorResult {
        outcomes,
        trace: trace.clone(),
    })
}

/// Unpacks a `JoinSet` join result into an `ActorOutcome` and server flag.
///
/// Returns `(outcome, Some(is_server))` on normal completion, or
/// `(outcome, None)` when the task panicked (metadata lost).
fn unpack_join_result(
    join_result: Result<ActorTaskResult, tokio::task::JoinError>,
    events: &EventEmitter,
) -> (ActorOutcome, Option<bool>) {
    match join_result {
        Ok(task_result) => {
            let is_server = task_result.is_server;
            let outcome = match task_result.result {
                Ok(actor_result) => ActorOutcome::Success(actor_result),
                Err(err) => {
                    events.emit(ThoughtJackEvent::ActorError {
                        actor_name: task_result.actor_name.clone(),
                        error: err.to_string(),
                    });
                    ActorOutcome::Error {
                        actor_name: task_result.actor_name,
                        error: err.to_string(),
                    }
                }
            };
            (outcome, Some(is_server))
        }
        Err(join_err) => {
            let actor_name = "(unknown)".to_string();
            let msg = if join_err.is_panic() {
                "task panicked"
            } else {
                "task cancelled"
            };
            events.emit(ThoughtJackEvent::ActorError {
                actor_name: actor_name.clone(),
                error: msg.to_string(),
            });
            (ActorOutcome::Panic { actor_name }, None)
        }
    }
}

/// Emits the final orchestrator summary event.
fn emit_completion_summary(outcomes: &[ActorOutcome], events: &EventEmitter) {
    let succeeded = outcomes
        .iter()
        .filter(|o| matches!(o, ActorOutcome::Success(_)))
        .count();
    let failed = outcomes.len() - succeeded;

    events.emit(ThoughtJackEvent::OrchestratorCompleted {
        summary: format!(
            "{} actors: {} succeeded, {} failed",
            outcomes.len(),
            succeeded,
            failed
        ),
    });
}

/// Applies the configured grace period then cancels all remaining actors.
async fn apply_grace_and_cancel(
    config: &ActorConfig,
    cancel: &CancellationToken,
    events: &EventEmitter,
) {
    let grace = config.grace_period.unwrap_or(Duration::ZERO);
    #[allow(clippy::cast_possible_truncation)]
    let duration_seconds = grace.as_secs();
    events.emit(ThoughtJackEvent::GracePeriodStarted { duration_seconds });
    if !grace.is_zero() {
        tokio::time::sleep(grace).await;
    }
    events.emit(ThoughtJackEvent::GracePeriodExpired {
        messages_captured: 0,
    });
    cancel.cancel();
}

/// Drains all tasks from a `JoinSet` after an early abort (e.g., gate timeout).
async fn drain_join_set(
    mut join_set: JoinSet<ActorTaskResult>,
    trace: SharedTrace,
    events: &EventEmitter,
) -> Result<OrchestratorResult, EngineError> {
    let mut outcomes = Vec::with_capacity(join_set.len());
    while let Some(join_result) = join_set.join_next().await {
        let (outcome, _) = unpack_join_result(join_result, events);
        outcomes.push(outcome);
    }

    Ok(OrchestratorResult { outcomes, trace })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_outcome_name() {
        let success = ActorOutcome::Success(ActorResult {
            actor_name: "srv".to_string(),
            termination: crate::engine::types::TerminationReason::TerminalPhaseReached,
            phases_completed: 2,
            total_phases: 3,
            final_phase: Some("terminal".to_string()),
        });
        assert_eq!(success.actor_name(), "srv");

        let error = ActorOutcome::Error {
            actor_name: "cli".to_string(),
            error: "fail".to_string(),
        };
        assert_eq!(error.actor_name(), "cli");

        let panic_outcome = ActorOutcome::Panic {
            actor_name: "oops".to_string(),
        };
        assert_eq!(panic_outcome.actor_name(), "oops");
    }

    // ---- Helper for orchestrator integration tests ----

    fn default_actor_config(max_session: Duration) -> ActorConfig {
        ActorConfig {
            mcp_server_bind: None,
            agui_client_endpoint: None,
            a2a_server_bind: None,
            a2a_client_endpoint: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            headers: vec![],
            raw_synthesize: false,
            grace_period: None,
            max_session,
            readiness_timeout: Duration::from_secs(30),
        }
    }

    fn load_test_doc(yaml: &str) -> LoadedDocument {
        crate::loader::load_document(yaml).expect("test YAML should load")
    }

    // ---- Integration tests ----

    #[tokio::test]
    async fn orchestrate_single_server_completes() {
        let loaded = load_test_doc(
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

        let config = default_actor_config(Duration::from_secs(10));
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Cancel after short delay — server would block on stdio otherwise
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();
        assert_eq!(result.outcomes.len(), 1);
        // Should be success with Cancelled termination (cancel fired before terminal)
        match &result.outcomes[0] {
            ActorOutcome::Success(r) => {
                assert_eq!(r.actor_name, "default");
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn orchestrate_mixed_outcomes() {
        let loaded = load_test_doc(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: valid_server
        mode: mcp_server
        phases:
          - name: idle
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: bad_client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#,
        );

        // No --agui-client-endpoint, so ag_ui_client will fail
        let config = default_actor_config(Duration::from_secs(5));
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        assert_eq!(result.outcomes.len(), 2);

        // Check we have both a success and an error
        let has_error = result
            .outcomes
            .iter()
            .any(|o| matches!(o, ActorOutcome::Error { .. }));
        assert!(has_error, "Expected at least one Error outcome");
    }

    #[tokio::test]
    async fn max_session_timeout_cancels() {
        let loaded = load_test_doc(
            r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: long_running
        state:
          tools:
            - name: test_tool
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 999
      - name: terminal
"#,
        );

        // Very short max_session
        let config = default_actor_config(Duration::from_millis(50));
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        assert_eq!(result.outcomes.len(), 1);
        // Actor should be cancelled/aborted due to max_session expiry.
        // Depending on timing, it may be Success(Cancelled) or Panic (aborted).
        match &result.outcomes[0] {
            ActorOutcome::Success(r) => {
                assert_eq!(r.actor_name, "default");
            }
            ActorOutcome::Panic { .. } | ActorOutcome::Error { .. } => {
                // Task was aborted before graceful cancel — acceptable
            }
        }
    }

    #[tokio::test]
    async fn zero_client_shutdown() {
        // Only server actors — shutdown triggers after all servers complete or cancel
        let loaded = load_test_doc(
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

        let config = default_actor_config(Duration::from_secs(10));
        let events = Arc::new(EventEmitter::noop());
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .unwrap();

        // Single server actor should have an outcome
        assert_eq!(result.outcomes.len(), 1);
    }

    #[test]
    fn unpack_join_result_handles_success() {
        let events = EventEmitter::noop();
        let task_result = ActorTaskResult {
            actor_name: "test_actor".to_string(),
            is_server: true,
            result: Ok(ActorResult {
                actor_name: "test_actor".to_string(),
                termination: crate::engine::types::TerminationReason::TerminalPhaseReached,
                phases_completed: 2,
                total_phases: 3,
                final_phase: Some("terminal".to_string()),
            }),
        };

        let (outcome, is_server) = unpack_join_result(Ok(task_result), &events);
        assert_eq!(is_server, Some(true));
        assert_eq!(outcome.actor_name(), "test_actor");
        assert!(matches!(outcome, ActorOutcome::Success(_)));
    }
}
