//! Multi-actor orchestrator.
//!
//! `orchestrate()` spawns one task per actor, manages the readiness gate,
//! coordinates shutdown (client-done → grace → cancel servers), and
//! collects results into an `OrchestratorResult`.
//!
//! See TJ-SPEC-015 §3 for the orchestration lifecycle.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::task::{Id as TaskId, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::engine::trace::SharedTrace;
use crate::engine::types::{ActorResult, AwaitExtractor};
use crate::error::EngineError;
use crate::loader::{LoadedDocument, document_actors};
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::orchestration::gate::{GateError, ReadinessGate};
use crate::orchestration::runner::{ActorConfig, run_actor};
use crate::orchestration::store::ExtractorStore;

/// Default timeout for waiting for server readiness (fallback if not configured).
const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time to wait for cooperative shutdown before force-aborting tasks.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Actor task was force-aborted before it returned an `ActorResult`.
    Aborted {
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
            Self::Error { actor_name, .. }
            | Self::Panic { actor_name }
            | Self::Aborted { actor_name } => actor_name,
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

/// Maps tokio task IDs to `(actor_name, is_server)` for panic recovery.
///
/// When a task panics, `JoinError` does not carry the return value, so
/// the `ActorTaskResult` (and its `actor_name`) is lost. This map allows
/// `unpack_join_result` to recover the identity via `JoinError::id()`.
type TaskMetaMap = HashMap<TaskId, (String, bool)>;

/// Log actor configuration at startup for visibility.
fn log_actor_configuration(actors: &[oatf::Actor], config: &ActorConfig, context_mode: bool) {
    let mode_label = if context_mode { "context" } else { "traffic" };
    for actor in actors {
        let transport: String = if context_mode
            && matches!(
                actor.mode.as_str(),
                "mcp_server" | "a2a_server" | "ag_ui_client"
            ) {
            "context-mode proxy".into()
        } else {
            match actor.mode.as_str() {
                "mcp_server" => config
                    .mcp_server_bind
                    .as_deref()
                    .map_or_else(|| "stdio".into(), |a| format!("http://{a}/mcp")),
                "a2a_server" => config
                    .a2a_server_bind
                    .as_deref()
                    .map_or_else(|| "(not configured)".into(), |a| format!("http://{a}")),
                "ag_ui_client" => config
                    .agui_client_endpoint
                    .as_deref()
                    .unwrap_or("(not configured)")
                    .into(),
                "a2a_client" => config
                    .a2a_client_endpoint
                    .as_deref()
                    .unwrap_or("(not configured)")
                    .into(),
                "mcp_client" => config
                    .mcp_client_endpoint
                    .as_deref()
                    .or(config.mcp_client_command.as_deref())
                    .unwrap_or("(not configured)")
                    .into(),
                _ => "unknown".into(),
            }
        };
        tracing::info!(
            actor = %actor.name,
            mode = %actor.mode,
            transport = %transport,
            execution = %mode_label,
            "actor configured"
        );
    }
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
#[allow(clippy::too_many_lines)]
pub async fn orchestrate(
    loaded: &LoadedDocument,
    config: &ActorConfig,
    events: &Arc<EventEmitter>,
    cancel: CancellationToken,
) -> Result<OrchestratorResult, EngineError> {
    let actors = document_actors(&loaded.document);

    // 1. Partition into servers and clients
    let mut server_count = 0;
    let mut client_count = 0;
    let mut server_names = Vec::new();

    for actor in actors {
        if actor.mode.ends_with("_server") {
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

    // 1b. Validate actor configuration and warn on missing transport binds
    let has_remote_clients = actors.iter().any(|a| {
        matches!(
            a.mode.as_str(),
            "ag_ui_client" | "a2a_client" | "mcp_client"
        )
    });
    for actor in actors {
        match actor.mode.as_str() {
            "mcp_server" if config.mcp_server_bind.is_none() && has_remote_clients => {
                tracing::warn!(
                    actor = %actor.name,
                    "MCP server actor will use stdio — unreachable by remote clients. \
                     Pass --mcp-server <ADDR:PORT> for HTTP."
                );
            }
            "a2a_server" if config.a2a_server_bind.is_none() => {
                tracing::warn!(
                    actor = %actor.name,
                    "A2A server actor has no --a2a-server bind address. \
                     Pass --a2a-server <ADDR:PORT> to enable."
                );
            }
            "ag_ui_client" if config.agui_client_endpoint.is_none() => {
                tracing::warn!(
                    actor = %actor.name,
                    "AG-UI client actor has no --agui-client-endpoint. \
                     The actor cannot connect to an agent."
                );
            }
            "a2a_client" if config.a2a_client_endpoint.is_none() => {
                tracing::warn!(
                    actor = %actor.name,
                    "A2A client actor has no --a2a-client-endpoint. \
                     The actor cannot connect to a remote agent."
                );
            }
            "mcp_client"
                if config.mcp_client_command.is_none() && config.mcp_client_endpoint.is_none() =>
            {
                tracing::warn!(
                    actor = %actor.name,
                    "MCP client actor has no --mcp-client-command or --mcp-client-endpoint. \
                     The actor cannot connect to a server."
                );
            }
            _ => {}
        }
    }

    log_actor_configuration(actors, config, false);

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
    let (join_set, task_meta) = spawn_actor_tasks(
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
        match tokio::select! {
            result = gate.wait_all_ready(readiness_timeout) => result,
            () = cancel.cancelled() => {
                events.emit(ThoughtJackEvent::OrchestratorShutdown {
                    reason: "cancelled_during_startup".to_string(),
                });
                let outcomes =
                    drain_join_set_with_timeout(join_set, &task_meta, events, SHUTDOWN_DRAIN_TIMEOUT)
                        .await;
                emit_completion_summary(&outcomes, events);
                return Ok(OrchestratorResult {
                    outcomes,
                    trace,
                });
            }
        } {
            Ok(()) => {
                events.emit(ThoughtJackEvent::ReadinessGateOpen {
                    server_count,
                    #[allow(clippy::cast_possible_truncation)]
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(gate_err) => {
                tracing::error!(%gate_err, "readiness gate failed");
                match &gate_err {
                    GateError::Timeout { not_ready } => {
                        events.emit(ThoughtJackEvent::ReadinessGateTimeout {
                            not_ready: not_ready.clone(),
                        });
                    }
                    GateError::ServerFailed { actor } => {
                        events.emit(ThoughtJackEvent::ReadinessGateServerFailed {
                            actor: actor.clone(),
                        });
                    }
                }
                cancel.cancel();
                drain_join_set_with_timeout(join_set, &task_meta, events, SHUTDOWN_DRAIN_TIMEOUT)
                    .await;
                return Err(EngineError::Phase(format!(
                    "readiness gate failed: {gate_err}"
                )));
            }
        }
    }

    // 6. Wait for completion with shutdown coordination
    wait_for_completion(
        join_set,
        &task_meta,
        &trace,
        config,
        &cancel,
        events,
        client_count,
    )
    .await
}

/// Increment the port in a `"host:port"` bind address string.
///
/// Returns the original string unchanged if parsing fails or the port
/// is 0 (OS-assigned). Used to auto-assign unique ports when multiple
/// actors of the same server mode share a single CLI bind address.
fn increment_bind_port(addr: &str, offset: u16) -> String {
    if offset == 0 {
        return addr.to_string();
    }
    let Ok(socket) = addr.parse::<SocketAddr>() else {
        return addr.to_string();
    };
    if socket.port() == 0 {
        // Port 0 = OS-assigned; no need to increment
        return addr.to_string();
    }
    let new_port = socket.port().wrapping_add(offset);
    SocketAddr::new(socket.ip(), new_port).to_string()
}

/// Spawns one task per actor into a `JoinSet`.
///
/// Returns the `JoinSet` and a `TaskMetaMap` that maps each spawned
/// tokio task ID to `(actor_name, is_server)`. The map is used by
/// `unpack_join_result` to recover actor identity when a task panics.
///
/// When multiple actors share the same server mode (e.g., two `a2a_server`
/// actors), each actor beyond the first gets an auto-incremented port
/// to avoid bind conflicts.
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
) -> (JoinSet<ActorTaskResult>, TaskMetaMap) {
    let actors = document_actors(&loaded.document);

    let mut join_set: JoinSet<ActorTaskResult> = JoinSet::new();
    let mut task_meta = TaskMetaMap::new();

    // Track how many actors of each server mode we've seen so far,
    // so the Nth actor of the same mode gets port + (N-1).
    let mut mode_server_count: HashMap<String, u16> = HashMap::new();

    for (i, actor) in actors.iter().enumerate() {
        let is_server = actor.mode.ends_with("_server");
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
        let mut cfg = config.clone();

        // Auto-increment port for duplicate server modes
        if is_server {
            let count = mode_server_count.entry(actor.mode.clone()).or_insert(0);
            let offset = *count;
            *count += 1;

            match actor.mode.as_str() {
                "mcp_server" => {
                    if let Some(ref addr) = cfg.mcp_server_bind {
                        cfg.mcp_server_bind = Some(increment_bind_port(addr, offset));
                    }
                }
                "a2a_server" => {
                    if let Some(ref addr) = cfg.a2a_server_bind {
                        cfg.a2a_server_bind = Some(increment_bind_port(addr, offset));
                    }
                }
                _ => {}
            }
        }
        let tr = trace.clone();
        let es = extractor_store.clone();
        let actor_cancel = cancel.child_token();
        let task_events = Arc::clone(events);

        let abort_handle = join_set.spawn(async move {
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
        task_meta.insert(abort_handle.id(), (actor.name.clone(), is_server));
    }

    (join_set, task_meta)
}

/// Waits for all actor tasks to complete with proper shutdown coordination.
///
/// Shutdown rules (TJ-SPEC-015 §3.2):
/// - When all clients complete → start grace period → cancel servers
/// - Zero-client fallback: all servers done → start grace period
/// - Max-session timeout → cancel all
// Complexity: shutdown coordination select loop with grace period, max-session, and cancel
#[allow(clippy::cognitive_complexity)]
async fn wait_for_completion(
    mut join_set: JoinSet<ActorTaskResult>,
    task_meta: &TaskMetaMap,
    trace: &SharedTrace,
    config: &ActorConfig,
    cancel: &CancellationToken,
    events: &Arc<EventEmitter>,
    total_clients: usize,
) -> Result<OrchestratorResult, EngineError> {
    let mut outcomes: Vec<ActorOutcome> = Vec::with_capacity(join_set.len());
    let mut clients_done = 0;
    let mut grace_started = false;
    let mut shutdown_requested = false;
    let max_session_sleep =
        tokio::time::sleep_until(tokio::time::Instant::now() + config.max_session);
    tokio::pin!(max_session_sleep);

    let cancelled = cancel.cancelled();
    tokio::pin!(cancelled);

    loop {
        if join_set.is_empty() {
            break;
        }

        tokio::select! {
            Some(join_result) = join_set.join_next() => {
                let (outcome, is_server) = unpack_join_result(join_result, task_meta, events);

                if is_server == Some(false) {
                    clients_done += 1;
                }

                outcomes.push(outcome);

                // Check shutdown conditions (grace period fires at most once)
                let should_grace = total_clients > 0 && clients_done >= total_clients;

                if should_grace && !grace_started {
                    grace_started = true;
                    tracing::info!("starting grace period");
                    spawn_grace_task(config, cancel, events);
                }
            }
            () = &mut max_session_sleep => {
                tracing::warn!("max session expired, cancelling all actors");
                events.emit(ThoughtJackEvent::OrchestratorShutdown {
                    reason: "max_session_expired".to_string(),
                });
                cancel.cancel();
                shutdown_requested = true;
                break;
            }
            () = &mut cancelled => {
                tracing::info!("orchestrator cancelled");
                shutdown_requested = true;
                break;
            }
        }
    }

    if shutdown_requested && !join_set.is_empty() {
        outcomes.extend(
            drain_join_set_with_timeout(join_set, task_meta, events, SHUTDOWN_DRAIN_TIMEOUT).await,
        );
    } else {
        while let Some(join_result) = join_set.join_next().await {
            let (outcome, _) = unpack_join_result(join_result, task_meta, events);
            outcomes.push(outcome);
        }
    }

    emit_completion_summary(&outcomes, events);

    Ok(OrchestratorResult {
        outcomes,
        trace: trace.clone(),
    })
}

/// Unpacks a `JoinSet` join result into an `ActorOutcome` and server flag.
///
/// On normal completion, identity comes from the `ActorTaskResult` payload.
/// On panic/cancel, the payload is lost — identity is recovered from the
/// `TaskMetaMap` using the tokio task ID.
fn unpack_join_result(
    join_result: Result<ActorTaskResult, tokio::task::JoinError>,
    task_meta: &TaskMetaMap,
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
            let (actor_name, is_server) = task_meta.get(&join_err.id()).map_or_else(
                || ("(unknown)".to_string(), None),
                |(name, server)| (name.clone(), Some(*server)),
            );

            if join_err.is_cancelled() {
                events.emit(ThoughtJackEvent::ActorError {
                    actor_name: actor_name.clone(),
                    error: "task aborted before cooperative shutdown completed".to_string(),
                });
                let outcome = ActorOutcome::Aborted { actor_name };
                (outcome, is_server)
            } else {
                // Task panicked — genuine failure.
                events.emit(ThoughtJackEvent::ActorError {
                    actor_name: actor_name.clone(),
                    error: "task panicked".to_string(),
                });
                (ActorOutcome::Panic { actor_name }, is_server)
            }
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

/// Spawns a background task that waits for the grace period then cancels.
///
/// This must not be `.await`ed inline inside `tokio::select!` because that
/// would block the event loop, preventing `max_session` and external
/// cancellation from firing during the grace period.
fn spawn_grace_task(config: &ActorConfig, cancel: &CancellationToken, events: &Arc<EventEmitter>) {
    let grace = config.grace_period.unwrap_or(Duration::ZERO);
    #[allow(clippy::cast_possible_truncation)]
    let duration_seconds = grace.as_secs();
    events.emit(ThoughtJackEvent::GracePeriodStarted { duration_seconds });

    if grace.is_zero() {
        // No delay needed — cancel immediately and skip the spawn
        events.emit(ThoughtJackEvent::GracePeriodExpired {
            messages_captured: 0,
        });
        cancel.cancel();
        return;
    }

    let cancel = cancel.clone();
    let events = Arc::clone(events);
    tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        events.emit(ThoughtJackEvent::GracePeriodExpired {
            messages_captured: 0,
        });
        cancel.cancel();
    });
}

/// Drains tasks cooperatively for a bounded interval, then force-aborts any stragglers.
async fn drain_join_set_with_timeout(
    mut join_set: JoinSet<ActorTaskResult>,
    task_meta: &TaskMetaMap,
    events: &EventEmitter,
    timeout: Duration,
) -> Vec<ActorOutcome> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut outcomes = Vec::with_capacity(join_set.len());

    while !join_set.is_empty() {
        match tokio::time::timeout_at(deadline, join_set.join_next()).await {
            Ok(Some(join_result)) => {
                let (outcome, _) = unpack_join_result(join_result, task_meta, events);
                outcomes.push(outcome);
            }
            Ok(None) => break,
            Err(_) => {
                tracing::warn!("shutdown drain timed out; aborting remaining actor tasks");
                join_set.abort_all();
                break;
            }
        }
    }

    while let Some(join_result) = join_set.join_next().await {
        let (outcome, _) = unpack_join_result(join_result, task_meta, events);
        outcomes.push(outcome);
    }

    outcomes
}

// ============================================================================
// Context-Mode Orchestration (TJ-SPEC-022)
// ============================================================================

/// Runs context-mode orchestration: LLM API-backed conversation with channel handles.
///
/// Constructs the channel topology per TJ-SPEC-022 §2.4:
/// 1. Validates actors (requires AG-UI client, rejects MCP/A2A client).
/// 2. Constructs per-actor channels, handles, and watch channels.
/// 3. Creates `ContextTransport` with LLM provider.
/// 4. Spawns server actors, AG-UI actor, and the drive loop.
///
/// # Errors
///
/// Returns `EngineError` if actors are invalid, provider creation fails,
/// or an actor fails critically.
///
/// Implements: TJ-SPEC-022 F-001
#[allow(clippy::too_many_lines)]
pub async fn orchestrate_context(
    loaded: &LoadedDocument,
    config: &ActorConfig,
    events: &Arc<EventEmitter>,
    cancel: CancellationToken,
) -> Result<OrchestratorResult, EngineError> {
    use crate::engine::phase::PhaseEngine;
    use crate::engine::phase_loop::{PhaseLoop, PhaseLoopConfig};
    use crate::protocol::context_agui::ContextAgUiDriver;
    use crate::transport::context::{
        AgUiHandle, ContextTransport, ServerActorEntry, ServerHandle,
        extract_tool_definitions_for_actor,
    };
    use crate::transport::provider::create_provider;
    use std::sync::Arc as StdArc;
    use tokio::task::JoinSet;

    let actors = document_actors(&loaded.document);
    log_actor_configuration(actors, config, true);

    // 1. Validate actors
    let mut agui_index = None;
    let mut server_indices = Vec::new();
    for (i, actor) in actors.iter().enumerate() {
        match actor.mode.as_str() {
            "ag_ui_client" => {
                if agui_index.is_some() {
                    return Err(EngineError::Driver(
                        "context-mode supports at most one ag_ui_client actor".into(),
                    ));
                }
                agui_index = Some(i);
            }
            "mcp_server" | "a2a_server" => server_indices.push(i),
            "mcp_client" | "a2a_client" => {
                return Err(EngineError::Driver(format!(
                    "context-mode does not support {} actors",
                    actor.mode
                )));
            }
            _ => {
                return Err(EngineError::Driver(format!(
                    "unsupported actor mode in context-mode: {}",
                    actor.mode
                )));
            }
        }
    }
    let agui_actor_index = agui_index.ok_or_else(|| {
        EngineError::Driver(
            "context-mode requires an ag_ui_client actor (hint: add an actor \
             with mode: ag_ui_client to your OATF document)"
                .into(),
        )
    })?;

    events.emit(ThoughtJackEvent::OrchestratorStarted {
        actor_count: actors.len(),
        server_count: server_indices.len(),
        client_count: 1,
    });

    // 2. Shared state
    let trace = SharedTrace::new();
    let extractor_store = ExtractorStore::new();
    let thread_id = uuid::Uuid::new_v4().to_string();

    // 3. AG-UI channels — unbounded to prevent deadlock between drive
    // loop and AG-UI actor (bounded channels risk circular wait when both
    // sides block on send simultaneously).
    let (agui_tx, agui_rx) = tokio::sync::mpsc::unbounded_channel();
    let (agui_response_tx, agui_response_rx) = tokio::sync::mpsc::unbounded_channel();

    // 4. Shared result/request channels
    let (tool_result_tx, tool_result_rx) = tokio::sync::mpsc::channel(16);
    let (server_request_tx, server_request_rx) = tokio::sync::mpsc::channel(16);

    // 5. Per-server-actor setup (OATF document order)
    let mut server_actor_entries: HashMap<String, ServerActorEntry> = HashMap::new();
    let mut server_tool_watches = Vec::new();
    let mut server_handles: Vec<(usize, String, StdArc<dyn crate::transport::Transport>)> =
        Vec::new();
    let mut tool_watch_txs: HashMap<
        String,
        tokio::sync::watch::Sender<Vec<crate::transport::context::ToolDefinition>>,
    > = HashMap::new();
    // Per-A2A-actor watch for the current default skill name, updated on
    // phase advance so the drive loop dispatches to the right skill.
    let mut a2a_skill_txs: HashMap<String, tokio::sync::watch::Sender<Option<String>>> =
        HashMap::new();

    for &idx in &server_indices {
        let actor = &actors[idx];
        let actor_name = actor.name.clone();

        let (server_tx, server_rx) = tokio::sync::mpsc::channel(16);

        // Extract initial tool definitions from phase 0 (mode-aware for A2A)
        let engine_tmp = PhaseEngine::new(loaded.document.clone(), idx);
        let effective_state = engine_tmp.effective_state();
        let initial_tools =
            extract_tool_definitions_for_actor(&effective_state, &actor_name, &actor.mode);

        // For A2A actors, create a watch channel for the current first skill
        let a2a_skill_rx = if actor.mode == "a2a_server" {
            let initial_skill =
                crate::engine::mcp_server::helpers::a2a_skill_array(&effective_state)
                    .and_then(|arr| arr.first())
                    .and_then(|s| crate::engine::mcp_server::helpers::a2a_skill_name(s))
                    .map(String::from);
            let (skill_tx, skill_rx) = tokio::sync::watch::channel(initial_skill);
            a2a_skill_txs.insert(actor_name.clone(), skill_tx);
            Some(skill_rx)
        } else {
            None
        };

        let (tool_watch_tx, tool_watch_rx) = tokio::sync::watch::channel(initial_tools);

        let handle = ServerHandle::new(
            server_rx,
            tool_result_tx.clone(),
            server_request_tx.clone(),
            actor_name.clone(),
        );

        server_actor_entries.insert(
            actor_name.clone(),
            ServerActorEntry {
                tx: server_tx,
                mode: actor.mode.clone(),
                a2a_skill_rx,
            },
        );
        server_tool_watches.push((actor_name.clone(), tool_watch_rx));
        server_handles.push((idx, actor_name.clone(), StdArc::new(handle)));
        tool_watch_txs.insert(actor_name, tool_watch_tx);
    }

    // 6. Create LLM provider
    let provider_config = config.context_provider_config.as_ref().ok_or_else(|| {
        EngineError::Driver("context-mode requires provider configuration".into())
    })?;
    let provider = create_provider(provider_config)?;

    let max_turns = config.max_turns.unwrap_or(20);

    // Grace period warning
    if config.grace_period.is_some_and(|g| !g.is_zero()) {
        tracing::warn!(
            "grace period is not applicable in context-mode (no open transport to observe), ignoring"
        );
    }

    // 7. Construct ContextTransport
    // Build A2A system context roster (R2) — populated in Phase C
    let a2a_system_context = build_a2a_system_context(actors, &server_indices, &loaded.document);
    // Build MCP resource context — injects resource content from MCP server
    // actors into the LLM context, matching how real agent frameworks
    // present available resources.
    let resource_context = build_resource_context(actors, &server_indices, &loaded.document);

    let context_transport = ContextTransport::new(
        provider,
        config.context_system_prompt.clone(),
        a2a_system_context,
        resource_context,
        max_turns,
        agui_tx,
        agui_response_rx,
        thread_id.clone(),
        server_actor_entries,
        server_tool_watches,
        tool_result_rx,
        server_request_rx,
    );

    // 8. Construct AgUiHandle
    let agui_handle = AgUiHandle::new(agui_rx, agui_response_tx);

    // 9. Spawn actors
    let mut join_set: JoinSet<ActorTaskResult> = JoinSet::new();
    let mut task_meta: TaskMetaMap = HashMap::new();

    // 9a. AG-UI actor — spawn PhaseLoop directly
    {
        let driver = ContextAgUiDriver::new(Box::new(agui_handle), thread_id.clone());
        let engine = PhaseEngine::new(loaded.document.clone(), agui_actor_index);
        let agui_actor_name = actors[agui_actor_index].name.clone();

        let agui_await_cfg: HashMap<usize, Vec<AwaitExtractor>> = loaded
            .await_extractors
            .iter()
            .filter(|((name, _), _)| name == &agui_actor_name)
            .map(|((_, phase_idx), specs)| (*phase_idx, specs.clone()))
            .collect();

        let phase_count = engine.actor().phases.len();
        events.emit(ThoughtJackEvent::ActorInit {
            actor_name: agui_actor_name.clone(),
            mode: "ag_ui_client".to_string(),
        });
        events.emit(ThoughtJackEvent::ActorReady {
            actor_name: agui_actor_name.clone(),
            bind_address: "context".to_string(),
        });
        events.emit(ThoughtJackEvent::ActorStarted {
            actor_name: agui_actor_name.clone(),
            phase_count,
        });

        let loop_config = PhaseLoopConfig {
            trace: trace.clone(),
            extractor_store: extractor_store.clone(),
            actor_name: agui_actor_name.clone(),
            await_extractors_config: agui_await_cfg,
            cancel: cancel.child_token(),
            entry_action_sender: None,
            events: StdArc::clone(events),
            tool_watch_tx: None,
            a2a_skill_tx: None,
            context_mode: true,
        };

        let actor_name_owned = agui_actor_name.clone();
        let abort_handle = join_set.spawn(async move {
            let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
            let result = phase_loop.run().await;
            ActorTaskResult {
                actor_name: actor_name_owned,
                is_server: false,
                result,
            }
        });
        task_meta.insert(abort_handle.id(), (agui_actor_name, false));
    }

    // 9b. Server actors — use ServerHandle as transport
    for (idx, actor_name, handle) in server_handles {
        let actor = &actors[idx];
        let doc = loaded.document.clone();
        let tr = trace.clone();
        let es = extractor_store.clone();
        let actor_cancel = cancel.child_token();
        let task_events = StdArc::clone(events);
        let actor_name_clone = actor_name.clone();
        let raw_synthesize = config.raw_synthesize;
        let tool_watch_tx = tool_watch_txs.remove(&actor_name);
        let a2a_skill_tx = a2a_skill_txs.remove(&actor_name);

        let await_cfg: HashMap<usize, Vec<AwaitExtractor>> = loaded
            .await_extractors
            .iter()
            .filter(|((name, _), _)| name == &actor_name)
            .map(|((_, phase_idx), specs)| (*phase_idx, specs.clone()))
            .collect();

        let mode = actor.mode.clone();
        let abort_handle = join_set.spawn(async move {
            let server_cfg = ContextServerActorConfig {
                actor_index: idx,
                document: doc,
                transport: handle,
                raw_synthesize,
                tool_watch_tx,
                a2a_skill_tx,
                trace: tr,
                extractor_store: es,
                await_config: await_cfg,
                cancel: actor_cancel,
                mode,
                actor_name: actor_name_clone.clone(),
            };
            let result = run_context_server_actor(server_cfg, &task_events).await;
            ActorTaskResult {
                actor_name: actor_name_clone,
                is_server: true,
                result,
            }
        });
        task_meta.insert(abort_handle.id(), (actor_name.clone(), true));
    }

    // 10. Spawn drive loop
    let drive_cancel = cancel.child_token();
    let drive_handle = context_transport.spawn_drive_loop(drive_cancel);

    // 11. Wait for completion
    let mut outcomes = Vec::new();
    let max_session_sleep =
        tokio::time::sleep_until(tokio::time::Instant::now() + config.max_session);
    tokio::pin!(max_session_sleep);
    let cancelled = cancel.cancelled();
    tokio::pin!(cancelled);

    loop {
        tokio::select! {
            Some(join_result) = join_set.join_next() => {
                match join_result {
                    Ok(task_result) => {
                        let actor_name = task_result.actor_name.clone();
                        match task_result.result {
                            Ok(result) => {
                                events.emit(ThoughtJackEvent::ActorCompleted {
                                    actor_name: actor_name.clone(),
                                    reason: result.termination.to_string(),
                                    phases_completed: result.phases_completed,
                                });
                                outcomes.push(ActorOutcome::Success(result));
                            }
                            Err(e) => {
                                events.emit(ThoughtJackEvent::ActorError {
                                    actor_name: actor_name.clone(),
                                    error: e.to_string(),
                                });
                                outcomes.push(ActorOutcome::Error {
                                    actor_name,
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                    Err(join_err) => {
                        let actor_name = task_meta
                            .get(&join_err.id())
                            .map_or_else(|| "(unknown)".to_string(), |(name, _)| name.clone());
                        if join_err.is_panic() {
                            outcomes.push(ActorOutcome::Panic { actor_name });
                        } else {
                            outcomes.push(ActorOutcome::Aborted { actor_name });
                        }
                    }
                }
                if join_set.is_empty() {
                    break;
                }
            }
            () = &mut max_session_sleep => {
                tracing::warn!("max session timeout reached, cancelling");
                cancel.cancel();
            }
            () = &mut cancelled => {
                // Wait a short drain period then abort
                join_set.abort_all();
                while let Some(join_result) = join_set.join_next().await {
                    if let Ok(task_result) = join_result {
                        match task_result.result {
                            Ok(result) => outcomes.push(ActorOutcome::Success(result)),
                            Err(e) => outcomes.push(ActorOutcome::Error {
                                actor_name: task_result.actor_name,
                                error: e.to_string(),
                            }),
                        }
                    }
                }
                break;
            }
        }
    }

    // Wait for drive loop to finish and propagate errors.
    // JoinError (panic/cancel) is logged; EngineError is surfaced as a
    // synthetic actor outcome so the verdict pipeline can see it.
    match drive_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(engine_err)) => {
            tracing::error!(error = %engine_err, "context drive loop failed");
            outcomes.push(ActorOutcome::Error {
                actor_name: "(drive_loop)".to_string(),
                error: engine_err.to_string(),
            });
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "context drive loop task failed");
            outcomes.push(ActorOutcome::Panic {
                actor_name: "(drive_loop)".to_string(),
            });
        }
    }

    emit_completion_summary(&outcomes, events);
    Ok(OrchestratorResult { outcomes, trace })
}

/// Builds the A2A agent roster for system prompt injection.
///
/// Extracts Agent Card metadata from all `a2a_server` actors and formats
/// a structured text section. Returns `None` if no A2A actors exist.
///
/// Implements: TJ-SPEC-022 F-001
fn build_a2a_system_context(
    actors: &[oatf::Actor],
    server_indices: &[usize],
    document: &oatf::Document,
) -> Option<String> {
    use std::fmt::Write;

    use crate::engine::PhaseEngine;
    use crate::engine::mcp_server::helpers::{a2a_skill_array, a2a_skill_name};
    use serde_json::Value;

    let a2a_indices: Vec<usize> = server_indices
        .iter()
        .copied()
        .filter(|&idx| actors[idx].mode == "a2a_server")
        .collect();

    if a2a_indices.is_empty() {
        return None;
    }

    let mut text = String::from("## Available A2A Agents\n");

    for idx in a2a_indices {
        let actor = &actors[idx];
        let engine_tmp = PhaseEngine::new(document.clone(), idx);
        let state = engine_tmp.effective_state();
        let card = state.get("agent_card").unwrap_or(&state);

        let agent_name = card["name"].as_str().unwrap_or(&actor.name);
        let description = card["description"].as_str().unwrap_or("");
        let url = card["url"].as_str().unwrap_or("");
        let version = card["version"].as_str().unwrap_or("");

        let _ = write!(text, "\n### {}\n- Agent: {agent_name}", actor.name);
        if !version.is_empty() {
            let _ = write!(text, " (v{version})");
        }
        text.push('\n');
        if !url.is_empty() {
            let _ = writeln!(text, "- URL: {url}");
        }
        if !description.is_empty() {
            let _ = writeln!(text, "- Description: {description}");
        }

        // Capabilities
        if let Some(caps) = card.get("capabilities") {
            let streaming = caps["streaming"].as_bool().unwrap_or(false);
            let push = caps["pushNotifications"].as_bool().unwrap_or(false);
            let _ = writeln!(
                text,
                "- Capabilities: streaming={streaming}, pushNotifications={push}"
            );
        }

        // Authentication
        if let Some(auth) = card.get("authentication")
            && let Some(schemes) = auth["schemes"].as_array()
        {
            let scheme_strs: Vec<&str> = schemes.iter().filter_map(Value::as_str).collect();
            if !scheme_strs.is_empty() {
                let _ = writeln!(text, "- Authentication: {}", scheme_strs.join(", "));
            }
        }

        // Webhook
        if let Some(wh_url) = state
            .pointer("/webhook_registration/url")
            .and_then(Value::as_str)
        {
            let _ = writeln!(text, "- Webhook URL: {wh_url}");
        }

        // Skills
        if let Some(skills) = a2a_skill_array(&state)
            && !skills.is_empty()
        {
            text.push_str("- Skills:\n");
            for skill in skills {
                let skill_id = a2a_skill_name(skill).unwrap_or("unknown");
                let skill_desc = skill["description"].as_str().unwrap_or("");
                let _ = writeln!(text, "  - {skill_id}: {skill_desc}");

                if let Some(examples) = skill["examples"].as_array() {
                    let ex_strs: Vec<&str> = examples.iter().filter_map(Value::as_str).collect();
                    if !ex_strs.is_empty() {
                        let quoted: Vec<String> =
                            ex_strs.iter().map(|e| format!("\"{e}\"")).collect();
                        let _ = writeln!(text, "    Examples: {}", quoted.join(", "));
                    }
                }
            }
        }
    }

    text.push_str("\nTo interact with an A2A agent, call its tool with a message parameter.\n");
    Some(text)
}

/// Builds MCP resource content for system prompt injection.
///
/// Extracts resource URIs and text content from all `mcp_server` actors
/// and formats them as a structured text section. Returns `None` if no
/// resources are defined. This replicates how real agent frameworks
/// include available resource content in the LLM context.
fn build_resource_context(
    actors: &[oatf::Actor],
    server_indices: &[usize],
    document: &oatf::Document,
) -> Option<String> {
    use std::fmt::Write;

    use crate::engine::PhaseEngine;
    use serde_json::Value;

    let mut text = String::new();
    let mut found_any = false;

    for &idx in server_indices {
        let actor = &actors[idx];
        if actor.mode != "mcp_server" {
            continue;
        }
        let engine_tmp = PhaseEngine::new(document.clone(), idx);
        let state = engine_tmp.effective_state();

        let Some(resources) = state.get("resources").and_then(Value::as_array) else {
            continue;
        };

        for resource in resources {
            let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
            let name = resource.get("name").and_then(Value::as_str).unwrap_or(uri);

            // Extract text content from the resource.
            // Supports both `content` as a string and `content` as an object
            // with a `text` field (matching MCP resource content format).
            let content_text = resource.get("content").and_then(|c| {
                c.as_str()
                    .map(String::from)
                    .or_else(|| c.get("text").and_then(Value::as_str).map(String::from))
            });

            if let Some(content) = content_text {
                if !found_any {
                    text.push_str("## Available Resources\n\n");
                    found_any = true;
                }
                let _ = writeln!(text, "### {name}");
                if !uri.is_empty() {
                    let _ = writeln!(text, "URI: {uri}");
                }
                let _ = writeln!(text, "\n{content}");
            }
        }
    }

    if found_any { Some(text) } else { None }
}

/// Configuration for running a server actor in context-mode.
struct ContextServerActorConfig {
    actor_index: usize,
    document: oatf::Document,
    transport: std::sync::Arc<dyn crate::transport::Transport>,
    raw_synthesize: bool,
    tool_watch_tx:
        Option<tokio::sync::watch::Sender<Vec<crate::transport::context::ToolDefinition>>>,
    /// For A2A actors: sender to publish updated default skill on phase advance.
    a2a_skill_tx: Option<tokio::sync::watch::Sender<Option<String>>>,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    mode: String,
    actor_name: String,
}

/// Runs a server actor in context-mode with a pre-built transport handle.
async fn run_context_server_actor(
    cfg: ContextServerActorConfig,
    events: &std::sync::Arc<EventEmitter>,
) -> Result<crate::engine::types::ActorResult, EngineError> {
    use crate::engine::mcp_server::McpServerDriver;
    use crate::engine::phase::PhaseEngine;
    use crate::engine::phase_loop::{PhaseLoop, PhaseLoopConfig};

    events.emit(ThoughtJackEvent::ActorInit {
        actor_name: cfg.actor_name.clone(),
        mode: cfg.mode.clone(),
    });

    // Server actors are immediately ready in context-mode (no port binding)
    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: cfg.actor_name.clone(),
        bind_address: "context".to_string(),
    });

    let engine = PhaseEngine::new(cfg.document, cfg.actor_index);
    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: cfg.actor_name.clone(),
        phase_count,
    });

    // Both MCP and A2A server actors use McpServerDriver in context-mode.
    // This works because McpServerDriver is transport-agnostic (receive
    // request → dispatch against OATF state → send response) and both
    // protocols share the same tool state schema (name, inputSchema,
    // responses). A2aServerDriver can't be used here because it bypasses
    // the Transport trait and binds its own axum HTTP server.
    //
    // TODO: Refactor A2aServerDriver onto the Transport trait so it works
    // with any transport (channels, stdio, HTTP) — then context-mode can
    // use the real A2A driver and get full A2A fidelity (Agent Card,
    // task lifecycle, SSE streaming). Track as separate PR.
    if cfg.mode != "mcp_server" && cfg.mode != "a2a_server" {
        return Err(EngineError::Driver(format!(
            "unsupported server mode in context-mode: {}",
            cfg.mode
        )));
    }

    let mut driver = McpServerDriver::new(cfg.transport.clone(), cfg.raw_synthesize);
    // Context-mode skips the MCP `initialize` handshake, so pre-populate
    // client capabilities to enable elicitation and sampling.
    driver.set_client_capabilities(json!({
        "sampling": {},
        "elicitation": {},
    }));
    let entry_action_sender = driver.entry_action_sender();
    let loop_config = PhaseLoopConfig {
        trace: cfg.trace,
        extractor_store: cfg.extractor_store,
        actor_name: cfg.actor_name.clone(),
        await_extractors_config: cfg.await_config,
        cancel: cfg.cancel,
        entry_action_sender: Some(Box::new(entry_action_sender)),
        events: std::sync::Arc::clone(events),
        tool_watch_tx: cfg.tool_watch_tx,
        a2a_skill_tx: cfg.a2a_skill_tx,
        context_mode: true,
    };
    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let result = phase_loop.run().await?;
    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: cfg.actor_name,
        reason: result.termination.to_string(),
        phases_completed: result.phases_completed,
    });
    Ok(result)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::TerminationReason;

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
            context_mode: false,
            context_provider_config: None,
            max_turns: None,
            context_system_prompt: None,
            transport_factory: Some(crate::orchestration::runner::null_transport_factory()),
        }
    }

    fn load_test_doc(yaml: &str) -> LoadedDocument {
        crate::loader::load_document(yaml).expect("test YAML should load")
    }

    // ---- Integration tests ----

    #[tokio::test]
    async fn orchestrate_single_server_completes() {
        tokio::time::timeout(Duration::from_secs(15), async {
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
                tokio::time::sleep(Duration::from_millis(500)).await;
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
        })
        .await
        .expect("test timed out after 15s");
    }

    #[tokio::test]
    async fn orchestrate_mixed_outcomes() {
        tokio::time::timeout(Duration::from_secs(15), async {
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

            let server_outcome = result
                .outcomes
                .iter()
                .find(|o| o.actor_name() == "valid_server")
                .expect("missing outcome for valid_server");
            let client_outcome = result
                .outcomes
                .iter()
                .find(|o| o.actor_name() == "bad_client")
                .expect("missing outcome for bad_client");

            assert!(
                matches!(server_outcome, ActorOutcome::Success(_)),
                "valid_server should succeed, got: {server_outcome:?}"
            );
            assert!(
                matches!(client_outcome, ActorOutcome::Error { .. }),
                "bad_client should fail, got: {client_outcome:?}"
            );
        })
        .await
        .expect("test timed out after 15s");
    }

    #[tokio::test]
    async fn max_session_timeout_cancels() {
        tokio::time::timeout(Duration::from_secs(15), async {
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

            let config = default_actor_config(Duration::from_millis(500));
            let events = Arc::new(EventEmitter::noop());
            let cancel = CancellationToken::new();

            let result = orchestrate(&loaded, &config, &events, cancel)
                .await
                .unwrap();

            assert_eq!(result.outcomes.len(), 1);
            match &result.outcomes[0] {
                ActorOutcome::Success(r) => {
                    assert_eq!(r.actor_name, "default");
                    assert_eq!(r.termination, TerminationReason::Cancelled);
                }
                other => panic!("Expected cancelled success outcome, got: {other:?}"),
            }
        })
        .await
        .expect("test timed out after 15s");
    }

    #[tokio::test]
    async fn zero_client_shutdown() {
        tokio::time::timeout(Duration::from_secs(15), async {
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
                tokio::time::sleep(Duration::from_millis(500)).await;
                cancel_clone.cancel();
            });

            let result = orchestrate(&loaded, &config, &events, cancel)
                .await
                .unwrap();

            // Single server actor should have an outcome
            assert_eq!(result.outcomes.len(), 1);
        })
        .await
        .expect("test timed out after 15s");
    }

    #[tokio::test]
    async fn readiness_gate_failure_returns_error() {
        tokio::time::timeout(Duration::from_secs(15), async {
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

            let mut config = default_actor_config(Duration::from_secs(5));
            config.mcp_server_bind = Some("invalid-bind-address".to_string());
            config.readiness_timeout = Duration::from_millis(250);

            let events = Arc::new(EventEmitter::noop());
            let cancel = CancellationToken::new();

            let result = orchestrate(&loaded, &config, &events, cancel).await;
            let err_text = match result {
                Ok(_) => panic!("expected readiness gate failure to be fatal"),
                Err(err) => err.to_string(),
            };
            assert!(
                err_text.contains("readiness gate failed"),
                "unexpected error text: {err_text}"
            );
        })
        .await
        .expect("test timed out after 15s");
    }

    #[tokio::test]
    async fn readiness_gate_server_failure_emits_server_failed_event() {
        tokio::time::timeout(Duration::from_secs(15), async {
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

            let mut config = default_actor_config(Duration::from_secs(5));
            config.mcp_server_bind = Some("invalid-bind-address".to_string());
            config.readiness_timeout = Duration::from_millis(250);

            let tempdir = tempfile::tempdir().expect("tempdir should be created");
            let events_path = tempdir.path().join("events.jsonl");
            let events = Arc::new(
                EventEmitter::from_file(&events_path)
                    .expect("event emitter file should be created"),
            );
            let cancel = CancellationToken::new();

            let result = orchestrate(&loaded, &config, &events, cancel).await;
            assert!(result.is_err(), "expected readiness gate failure");
            events.flush();

            let content = std::fs::read_to_string(&events_path)
                .expect("should be able to read emitted events");
            let event_types: Vec<String> = content
                .lines()
                .map(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .expect("event line should be valid json")
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();

            assert!(
                event_types.iter().any(|t| t == "ReadinessGateServerFailed"),
                "expected ReadinessGateServerFailed event, got {event_types:?}"
            );
            assert!(
                !event_types.iter().any(|t| t == "ReadinessGateTimeout"),
                "timeout event should not be emitted for server failure path: {event_types:?}"
            );
        })
        .await
        .expect("test timed out after 15s");
    }

    // ---- Edge case tests (EC-ORCH-*) ----

    /// EC-ORCH-002: Actor with `await_extractors` pointing to never-set key
    /// + short timeout → times out gracefully without blocking forever.
    #[tokio::test]
    async fn ec_orch_002_await_extractor_timeout() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let loaded = load_test_doc(
                r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: producer
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: consumer
        mode: mcp_server
        phases:
          - name: wait_phase
            await_extractors:
              - actor: producer
                extractors:
                  - never_set_key
                timeout: "100ms"
            state:
              tools:
                - name: consumer_tool
                  description: "test"
                  inputSchema:
                    type: object
"#,
            );

            let config = default_actor_config(Duration::from_secs(5));
            let events = Arc::new(EventEmitter::noop());
            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();

            // Cancel after the await_extractors timeout has had time to fire
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                cancel_clone.cancel();
            });

            let result = orchestrate(&loaded, &config, &events, cancel)
                .await
                .unwrap();

            // Both actors should complete (consumer timed out but proceeded)
            assert_eq!(result.outcomes.len(), 2);
        })
        .await
        .expect("test timed out after 15s");
    }

    /// EC-ORCH-010: Two actors with the same `name:` — rejected at load time
    /// with a descriptive error (no panic, graceful handling).
    #[test]
    fn ec_orch_010_duplicate_actor_name() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: tool_a
                  description: "test"
                  inputSchema:
                    type: object
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: tool_b
                  description: "test"
                  inputSchema:
                    type: object
"#;

        let result = crate::loader::load_document(yaml);
        assert!(result.is_err(), "duplicate actor names should be rejected");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate") || msg.contains("actor name"),
            "error should mention duplicate actor name, got: {msg}"
        );
    }

    /// EC-ORCH-011: 50 concurrent tasks writing/reading `ExtractorStore` →
    /// no data races, all values retrievable.
    #[tokio::test]
    async fn ec_orch_011_high_contention_store() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let store = ExtractorStore::new();
            let mut handles = Vec::new();

            for i in 0..50 {
                let store_clone = store.clone();
                handles.push(tokio::spawn(async move {
                    let actor = format!("actor_{i}");
                    let value = format!("value_{i}");
                    store_clone.set(&actor, "token", value.clone());
                    // Read back immediately
                    let got = store_clone.get(&actor, "token");
                    assert_eq!(got, Some(value));
                }));
            }

            for handle in handles {
                handle.await.unwrap();
            }

            // Verify all 50 values are present
            let all = store.all_qualified();
            assert_eq!(all.len(), 50);
            for i in 0..50 {
                let key = format!("actor_{i}.token");
                assert_eq!(
                    all.get(&key),
                    Some(&format!("value_{i}")),
                    "missing or wrong value for {key}"
                );
            }
        })
        .await
        .expect("test timed out after 15s");
    }

    /// EC-ORCH-012: Multiple actors emitting events → merged trace preserves
    /// per-actor ordering via monotonic sequence numbers.
    #[tokio::test]
    async fn ec_orch_012_trace_ordering() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let trace = SharedTrace::new();
            let mut handles = Vec::new();

            // 5 actors, each emitting 10 events
            for actor_id in 0..5 {
                let trace_clone = trace.clone();
                handles.push(tokio::spawn(async move {
                    for seq in 0..10 {
                        trace_clone.append(
                            &format!("actor_{actor_id}"),
                            "phase_1",
                            crate::engine::types::Direction::Incoming,
                            &format!("event_{seq}"),
                            &serde_json::json!({"actor": actor_id, "seq": seq}),
                        );
                    }
                }));
            }

            for handle in handles {
                handle.await.unwrap();
            }

            let entries = trace.snapshot();
            assert_eq!(entries.len(), 50);

            // Verify: per-actor events maintain their relative ordering
            for actor_id in 0..5 {
                let actor_name = format!("actor_{actor_id}");
                let actor_entries: Vec<_> =
                    entries.iter().filter(|e| e.actor == actor_name).collect();
                assert_eq!(actor_entries.len(), 10);

                // Sequence numbers should be monotonically increasing for each actor
                for window in actor_entries.windows(2) {
                    assert!(
                        window[0].seq < window[1].seq,
                        "per-actor events should maintain ordering: {} < {}",
                        window[0].seq,
                        window[1].seq
                    );
                }
            }
        })
        .await
        .expect("test timed out after 15s");
    }

    /// EC-ORCH-014: `grace_period: 0` → shutdown proceeds immediately, no delay.
    #[tokio::test]
    async fn ec_orch_014_zero_grace_period() {
        tokio::time::timeout(Duration::from_secs(15), async {
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

            let mut config = default_actor_config(Duration::from_secs(10));
            config.grace_period = Some(Duration::ZERO);
            let events = Arc::new(EventEmitter::noop());
            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();

            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                cancel_clone.cancel();
            });

            let result = orchestrate(&loaded, &config, &events, cancel)
                .await
                .unwrap();

            assert_eq!(result.outcomes.len(), 1);
        })
        .await
        .expect("test timed out after 15s");
    }

    /// EC-ORCH-016: Only server actors, no clients → servers complete,
    /// orchestrator shuts down cleanly.
    #[tokio::test]
    async fn ec_orch_016_zero_client_shutdown() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let loaded = load_test_doc(
                r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: server1
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: tool_a
                  description: "test"
                  inputSchema:
                    type: object
      - name: server2
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: tool_b
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
                tokio::time::sleep(Duration::from_millis(500)).await;
                cancel_clone.cancel();
            });

            let result = orchestrate(&loaded, &config, &events, cancel)
                .await
                .unwrap();

            // Both servers should have outcomes
            assert_eq!(result.outcomes.len(), 2);

            for outcome in &result.outcomes {
                match outcome {
                    ActorOutcome::Success(r) => {
                        assert!(
                            r.actor_name == "server1" || r.actor_name == "server2",
                            "unexpected actor: {}",
                            r.actor_name
                        );
                        assert!(
                            matches!(
                                r.termination,
                                TerminationReason::Cancelled
                                    | TerminationReason::TerminalPhaseReached
                            ),
                            "unexpected termination for {}: {:?}",
                            r.actor_name,
                            r.termination
                        );
                    }
                    other => {
                        panic!("Expected server cancellation success, got: {other:?}");
                    }
                }
            }
        })
        .await
        .expect("test timed out after 15s");
    }

    #[test]
    fn unpack_join_result_handles_success() {
        let events = EventEmitter::noop();
        let task_meta = TaskMetaMap::new();
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

        let (outcome, is_server) = unpack_join_result(Ok(task_result), &task_meta, &events);
        assert_eq!(is_server, Some(true));
        assert_eq!(outcome.actor_name(), "test_actor");
        assert!(matches!(outcome, ActorOutcome::Success(_)));
    }

    // ---- increment_bind_port tests ----

    #[test]
    fn increment_bind_port_zero_offset() {
        assert_eq!(increment_bind_port("127.0.0.1:9090", 0), "127.0.0.1:9090");
    }

    #[test]
    fn increment_bind_port_basic() {
        assert_eq!(increment_bind_port("127.0.0.1:9090", 1), "127.0.0.1:9091");
        assert_eq!(increment_bind_port("127.0.0.1:9090", 3), "127.0.0.1:9093");
    }

    #[test]
    fn increment_bind_port_ipv6() {
        assert_eq!(increment_bind_port("[::1]:8080", 2), "[::1]:8082");
    }

    #[test]
    fn increment_bind_port_zero_port_unchanged() {
        // Port 0 = OS-assigned; don't increment
        assert_eq!(increment_bind_port("127.0.0.1:0", 5), "127.0.0.1:0");
    }

    #[test]
    fn increment_bind_port_unparseable_unchanged() {
        assert_eq!(
            increment_bind_port("not-a-socket-addr", 1),
            "not-a-socket-addr"
        );
    }
}
