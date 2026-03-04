//! Actor runner: creates transport, driver, and `PhaseLoop` per actor.
//!
//! The `run_actor()` function is the single entry point for executing an
//! actor's phase loop. It pattern-matches on the actor's mode to create
//! the appropriate transport and driver, then delegates to `PhaseLoop::run()`.
//!
//! See TJ-SPEC-015 §5 for the actor runner specification.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::cli::args::RunArgs;
use crate::engine::mcp_server::McpServerDriver;
use crate::engine::phase::PhaseEngine;
use crate::engine::phase_loop::{PhaseLoop, PhaseLoopConfig};
use crate::engine::trace::SharedTrace;
use crate::engine::types::{ActorResult, AwaitExtractor};
use crate::error::EngineError;
use crate::loader::document_actors;
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::orchestration::store::ExtractorStore;
use crate::protocol::{a2a_client, a2a_server, agui, mcp_client};
use crate::transport::http::HttpConfig;
use crate::transport::{HttpTransport, StdioTransport};

// ============================================================================
// TransportFactory (test-only)
// ============================================================================

/// Wrapper around a transport factory closure for test injection.
///
/// Allows tests to inject a `NullTransport` (or other mock) instead of
/// real stdio, preventing hangs from uncancellable blocking reads.
///
/// Implements: TJ-SPEC-015 F-003
#[cfg(test)]
#[derive(Clone)]
pub struct TransportFactory(
    pub Arc<dyn Fn() -> Arc<dyn crate::transport::Transport> + Send + Sync>,
);

#[cfg(test)]
impl std::fmt::Debug for TransportFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TransportFactory(<fn>)")
    }
}

/// Creates a `TransportFactory` that produces `NullTransport` instances.
#[cfg(test)]
#[must_use]
pub fn null_transport_factory() -> TransportFactory {
    TransportFactory(Arc::new(|| Arc::new(crate::transport::NullTransport)))
}

// ============================================================================
// ActorConfig
// ============================================================================

/// Runtime configuration for actor execution, derived from CLI flags.
///
/// Implements: TJ-SPEC-015 F-003
#[derive(Debug, Clone)]
pub struct ActorConfig {
    /// Bind address for MCP server mode (HTTP). `None` → use stdio.
    pub mcp_server_bind: Option<String>,
    /// Endpoint for AG-UI client mode.
    pub agui_client_endpoint: Option<String>,
    /// Bind address for A2A server mode.
    pub a2a_server_bind: Option<String>,
    /// Endpoint for A2A client mode.
    pub a2a_client_endpoint: Option<String>,
    /// Command to spawn for MCP client mode.
    pub mcp_client_command: Option<String>,
    /// Extra arguments for `mcp_client_command`.
    pub mcp_client_args: Option<String>,
    /// Endpoint for MCP client mode (HTTP).
    pub mcp_client_endpoint: Option<String>,
    /// Extra HTTP headers for client-mode transports.
    pub headers: Vec<(String, String)>,
    /// Bypass synthesize output validation.
    pub raw_synthesize: bool,
    /// Grace period duration.
    pub grace_period: Option<Duration>,
    /// Maximum session duration.
    pub max_session: Duration,
    /// Timeout for server readiness gate.
    pub readiness_timeout: Duration,
    /// Test-only transport factory to inject `NullTransport` instead of real stdio.
    #[cfg(test)]
    pub transport_factory: Option<TransportFactory>,
}

/// Builds an `ActorConfig` from CLI `RunArgs`.
///
/// # Errors
///
/// Returns `EngineError::Driver` if any `--header` value is malformed
/// (missing `:`, invalid header name, or invalid header value).
///
/// Implements: TJ-SPEC-015 F-003
pub fn build_actor_config(args: &RunArgs) -> Result<ActorConfig, EngineError> {
    let mut headers: Vec<(String, String)> = Vec::with_capacity(args.header.len());
    for (idx, raw) in args.header.iter().enumerate() {
        headers.push(parse_cli_header(raw, idx + 1)?);
    }

    Ok(ActorConfig {
        mcp_server_bind: args.mcp_server.clone(),
        agui_client_endpoint: args.agui_client_endpoint.clone(),
        a2a_server_bind: args.a2a_server.clone(),
        a2a_client_endpoint: args.a2a_client_endpoint.clone(),
        mcp_client_command: args.mcp_client_command.clone(),
        mcp_client_args: args.mcp_client_args.clone(),
        mcp_client_endpoint: args.mcp_client_endpoint.clone(),
        headers,
        raw_synthesize: args.raw_synthesize,
        grace_period: args.grace_period.map(Into::into),
        max_session: args.max_session.into(),
        readiness_timeout: args.readiness_timeout.into(),
        #[cfg(test)]
        transport_factory: None,
    })
}

/// Parses and validates a CLI `--header` value (`KEY:VALUE`).
///
/// Returns an `EngineError::Driver` with an actionable message when:
/// - the delimiter `:` is missing,
/// - the header name is empty/invalid,
/// - the header value is not valid for HTTP.
fn parse_cli_header(raw: &str, position: usize) -> Result<(String, String), EngineError> {
    let Some((raw_key, raw_value)) = raw.split_once(':') else {
        return Err(EngineError::Driver(format!(
            "invalid --header value at position {position}: '{raw}' (expected KEY:VALUE)"
        )));
    };

    let key = raw_key.trim();
    let value = raw_value.trim();

    if key.is_empty() {
        return Err(EngineError::Driver(format!(
            "invalid --header value at position {position}: empty header name"
        )));
    }

    HeaderName::from_bytes(key.as_bytes()).map_err(|_| {
        EngineError::Driver(format!(
            "invalid --header value at position {position}: '{key}' is not a valid HTTP header name"
        ))
    })?;

    HeaderValue::from_str(value).map_err(|_| {
        EngineError::Driver(format!(
            "invalid --header value at position {position}: value for '{key}' is not a valid HTTP header value"
        ))
    })?;

    Ok((key.to_string(), value.to_string()))
}

// ============================================================================
// Mode-Specific Header Resolution (TJ-SPEC-007 §4)
// ============================================================================

/// Returns the env-var prefix for a given actor mode.
///
/// Used for `THOUGHTJACK_{PREFIX}_AUTHORIZATION` and
/// `THOUGHTJACK_{PREFIX}_HEADER_{NAME}` env vars.
fn mode_env_prefix(mode: &str) -> Option<&'static str> {
    match mode {
        "mcp_client" => Some("MCP_CLIENT"),
        "a2a_client" => Some("A2A_CLIENT"),
        "ag_ui_client" => Some("AGUI"),
        _ => None,
    }
}

/// Merges base headers with override headers (case-insensitive dedup).
///
/// Override headers take precedence over base headers for the same
/// header name (case-insensitive comparison).
fn merge_headers(
    base: &[(String, String)],
    overrides: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = base.to_vec();
    for (name, value) in overrides {
        merged.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        merged.push((name.clone(), value.clone()));
    }
    merged
}

/// Collects mode-specific headers from environment variables.
///
/// Reads:
/// - `THOUGHTJACK_{PREFIX}_AUTHORIZATION` → `Authorization` header
/// - `THOUGHTJACK_{PREFIX}_HEADER_{NAME}` → arbitrary header
///   (underscores in `{NAME}` become hyphens)
fn collect_env_headers(prefix: &str) -> Vec<(String, String)> {
    let mut headers = Vec::new();

    // Check for THOUGHTJACK_{PREFIX}_AUTHORIZATION
    let auth_var = format!("THOUGHTJACK_{prefix}_AUTHORIZATION");
    if let Ok(value) = std::env::var(&auth_var) {
        headers.push(("Authorization".to_string(), value));
    }

    // Scan env for THOUGHTJACK_{PREFIX}_HEADER_* vars
    let header_prefix = format!("THOUGHTJACK_{prefix}_HEADER_");
    for (key, value) in std::env::vars() {
        if let Some(suffix) = key.strip_prefix(&header_prefix) {
            let header_name = suffix.replace('_', "-");
            headers.push((header_name, value));
        }
    }

    headers
}

/// Resolves HTTP headers for a given actor mode.
///
/// Merges base headers (from `--header`) with mode-specific env vars:
/// - `THOUGHTJACK_{PREFIX}_AUTHORIZATION` → sets `Authorization` header
/// - `THOUGHTJACK_{PREFIX}_HEADER_{NAME}` → sets arbitrary header
///   (underscores in `{NAME}` become hyphens)
///
/// Mode-specific env vars take precedence over `--header` for the same
/// header name (case-insensitive).
///
/// Implements: TJ-SPEC-007 F-002
fn resolve_headers_for_mode(base: &[(String, String)], mode: &str) -> Vec<(String, String)> {
    let Some(prefix) = mode_env_prefix(mode) else {
        return base.to_vec();
    };

    let env_headers = collect_env_headers(prefix);
    merge_headers(base, &env_headers)
}

/// Parses `--mcp-client-args` using shell quoting rules.
///
/// Returns an `EngineError::Driver` if the string has invalid shell syntax
/// (for example, unbalanced quotes).
fn parse_mcp_client_args(raw: &str) -> Result<Vec<String>, EngineError> {
    shlex::split(raw)
        .ok_or_else(|| EngineError::Driver("invalid --mcp-client-args: unbalanced quotes".into()))
}

/// Parses `--mcp-client-command` into executable + inline arguments.
///
/// Supports command strings with inline args (for example,
/// `"npx -y @modelcontextprotocol/server-everything"`).
///
/// Returns an `EngineError::Driver` if shell syntax is invalid or the
/// command resolves to an empty token list.
fn parse_mcp_client_command(raw: &str) -> Result<(String, Vec<String>), EngineError> {
    let parts = shlex::split(raw).ok_or_else(|| {
        EngineError::Driver("invalid --mcp-client-command: unbalanced quotes".into())
    })?;

    let mut iter = parts.into_iter();
    let command = iter
        .next()
        .ok_or_else(|| EngineError::Driver("invalid --mcp-client-command: empty command".into()))?;

    Ok((command, iter.collect()))
}

/// Waits for the server-readiness gate to open for a client actor.
///
/// Returns an error if the gate channel closes before readiness is signaled.
async fn wait_for_readiness_gate(
    actor_name: &str,
    gate_rx: Option<broadcast::Receiver<()>>,
) -> Result<(), EngineError> {
    if let Some(mut rx) = gate_rx {
        tracing::debug!(actor = %actor_name, "waiting for server readiness gate");
        rx.recv().await.map_err(|err| {
            EngineError::Phase(format!(
                "readiness gate closed before actor '{actor_name}' started: {err}"
            ))
        })?;
        tracing::debug!(actor = %actor_name, "readiness gate opened");
    }
    Ok(())
}

// ============================================================================
// ActorRunContext
// ============================================================================

/// Bundles the parameters shared by all `run_*_actor()` functions.
///
/// Avoids repeating 10+ arguments across every actor runner function.
///
/// Implements: TJ-SPEC-015 F-003
pub(crate) struct ActorRunContext<'a> {
    /// Index of this actor in the document's actor list.
    pub actor_index: usize,
    /// The OATF document.
    pub document: oatf::Document,
    /// Runtime configuration derived from CLI flags.
    pub config: &'a ActorConfig,
    /// Shared append-only trace buffer.
    pub trace: SharedTrace,
    /// Cross-actor extractor store.
    pub extractor_store: ExtractorStore,
    /// Per-phase `await_extractors` configuration.
    pub await_config: HashMap<usize, Vec<AwaitExtractor>>,
    /// Cancellation token for cooperative shutdown.
    pub cancel: CancellationToken,
    /// Oneshot sender to signal readiness (server-mode actors).
    pub ready_tx: Option<oneshot::Sender<()>>,
    /// Broadcast receiver for the readiness gate (client-mode actors).
    pub gate_rx: Option<broadcast::Receiver<()>>,
    /// Event emitter for observability.
    pub events: &'a EventEmitter,
}

// ============================================================================
// run_actor
// ============================================================================

/// Runs a single actor's full lifecycle: transport → driver → `PhaseLoop`.
///
/// Pattern-matches on the actor's mode to create the appropriate transport
/// and protocol driver. Server-mode actors signal readiness via `ready_tx`.
/// Client-mode actors wait for the gate via `gate_rx`.
///
/// # Panics
///
/// Panics if the document has no actors after normalization, or if
/// `actor_index` is out of bounds.
///
/// # Errors
///
/// Returns `EngineError::Driver` if the actor's mode is not yet supported.
/// Propagates any errors from transport binding or phase loop execution.
///
/// Implements: TJ-SPEC-015 F-003
#[allow(clippy::too_many_arguments, clippy::implicit_hasher)]
pub async fn run_actor(
    actor_index: usize,
    document: oatf::Document,
    config: &ActorConfig,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    ready_tx: Option<oneshot::Sender<()>>,
    gate_rx: Option<broadcast::Receiver<()>>,
    events: &EventEmitter,
) -> Result<ActorResult, EngineError> {
    let actors = document_actors(&document);
    let actor = &actors[actor_index];
    let actor_name = actor.name.clone();
    let mode = actor.mode.clone();

    events.emit(ThoughtJackEvent::ActorInit {
        actor_name: actor_name.clone(),
        mode: mode.clone(),
    });

    let ctx = ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx,
        events,
    };

    match mode.as_str() {
        "mcp_server" => run_mcp_server_actor(&actor_name, ctx).await,
        "ag_ui_client" => run_agui_client_actor(&actor_name, ctx).await,
        "a2a_server" => run_a2a_server_actor(&actor_name, ctx).await,
        "a2a_client" => run_a2a_client_actor(&actor_name, ctx).await,
        "mcp_client" => run_mcp_client_actor(&actor_name, ctx).await,
        other => Err(EngineError::Driver(format!(
            "driver for mode '{other}' not yet implemented"
        ))),
    }
}

/// Runs an MCP server actor — creates transport, driver, and phase loop.
async fn run_mcp_server_actor(
    actor_name: &str,
    ctx: ActorRunContext<'_>,
) -> Result<ActorResult, EngineError> {
    let ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx: _gate_rx,
        events,
    } = ctx;
    // Create transport based on CLI flags (or test-injected factory)
    let transport: Arc<dyn crate::transport::Transport> =
        if let Some(ref bind_addr) = config.mcp_server_bind {
            let http_config = HttpConfig {
                bind_addr: bind_addr.clone(),
                max_message_size: crate::transport::DEFAULT_MAX_MESSAGE_SIZE,
            };
            let (transport, addr) = HttpTransport::bind(http_config, cancel.clone())
                .await
                .map_err(|e| EngineError::Driver(format!("HTTP bind failed: {e}")))?;

            events.emit(ThoughtJackEvent::ActorReady {
                actor_name: actor_name.to_string(),
                bind_address: addr.to_string(),
            });

            // Signal readiness
            if let Some(tx) = ready_tx {
                let _ = tx.send(());
            }

            Arc::new(transport)
        } else {
            // stdio (or test-injected null) transport — ready immediately
            #[cfg(test)]
            let transport: Arc<dyn crate::transport::Transport> = config
                .transport_factory
                .as_ref()
                .map_or_else(|| Arc::new(StdioTransport::new()) as _, |f| (f.0)());

            #[cfg(not(test))]
            let transport: Arc<dyn crate::transport::Transport> = Arc::new(StdioTransport::new());

            events.emit(ThoughtJackEvent::ActorReady {
                actor_name: actor_name.to_string(),
                bind_address: "stdio".to_string(),
            });

            if let Some(tx) = ready_tx {
                let _ = tx.send(());
            }

            transport
        };

    // Note: server-mode actors don't wait on gate_rx — they signal it.
    // Client-mode actors would wait here.

    // Create driver
    let driver = McpServerDriver::new(Arc::clone(&transport), config.raw_synthesize);
    let entry_action_sender = driver.entry_action_sender();

    // Create phase engine
    let engine = PhaseEngine::new(document, actor_index);

    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: actor_name.to_string(),
        phase_count,
    });

    // Create and run phase loop
    let loop_config = PhaseLoopConfig {
        trace,
        extractor_store,
        actor_name: actor_name.to_string(),
        await_extractors_config: await_config,
        cancel,
        entry_action_sender: Some(Box::new(entry_action_sender)),
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let result = phase_loop.run().await?;

    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: actor_name.to_string(),
        reason: result.termination.to_string(),
        phases_completed: result.phases_completed,
    });

    Ok(result)
}

/// Runs an AG-UI client actor — creates transport, driver, and phase loop.
///
/// Client-mode actors wait for the readiness gate before sending requests,
/// ensuring server actors are ready to accept connections.
///
/// Implements: TJ-SPEC-016 F-001
async fn run_agui_client_actor(
    actor_name: &str,
    ctx: ActorRunContext<'_>,
) -> Result<ActorResult, EngineError> {
    let ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx,
        events,
    } = ctx;
    let endpoint = config.agui_client_endpoint.as_deref().ok_or_else(|| {
        EngineError::Driver("ag_ui_client mode requires --agui-client-endpoint".to_string())
    })?;

    // Client actors signal readiness immediately (they don't bind a port)
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    wait_for_readiness_gate(actor_name, gate_rx).await?;

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address: endpoint.to_string(),
    });

    // Create driver (merge mode-specific auth env vars with --header)
    let headers = resolve_headers_for_mode(&config.headers, "ag_ui_client");
    let driver = agui::create_agui_driver(endpoint, headers, config.raw_synthesize);

    // Create phase engine
    let engine = PhaseEngine::new(document, actor_index);

    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: actor_name.to_string(),
        phase_count,
    });

    // Create and run phase loop (no entry_action_sender — client mode)
    let loop_config = PhaseLoopConfig {
        trace,
        extractor_store,
        actor_name: actor_name.to_string(),
        await_extractors_config: await_config,
        cancel,
        entry_action_sender: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let result = phase_loop.run().await?;

    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: actor_name.to_string(),
        reason: result.termination.to_string(),
        phases_completed: result.phases_completed,
    });

    Ok(result)
}

/// Runs an A2A server actor — creates driver and phase loop.
///
/// Server-mode: binds HTTP transport inside `drive_phase()`.
///
/// Implements: TJ-SPEC-017 F-001
async fn run_a2a_server_actor(
    actor_name: &str,
    ctx: ActorRunContext<'_>,
) -> Result<ActorResult, EngineError> {
    let ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx: _gate_rx,
        events,
    } = ctx;
    let bind_addr = config
        .a2a_server_bind
        .as_deref()
        .unwrap_or("127.0.0.1:9090");

    let (bound_addr_tx, mut bound_addr_rx) = oneshot::channel();

    // Create driver. Readiness is signalled only after bind succeeds.
    let mut driver = a2a_server::create_a2a_server_driver(bind_addr, config.raw_synthesize);
    if let Some(tx) = ready_tx {
        driver.set_ready_sender(tx);
    }
    driver.set_bound_addr_sender(bound_addr_tx);

    // Create phase engine
    let engine = PhaseEngine::new(document, actor_index);

    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: actor_name.to_string(),
        phase_count,
    });

    // Create and run phase loop (no entry_action_sender — A2A server mode)
    let loop_config = PhaseLoopConfig {
        trace,
        extractor_store,
        actor_name: actor_name.to_string(),
        await_extractors_config: await_config,
        cancel,
        entry_action_sender: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let run_fut = phase_loop.run();
    tokio::pin!(run_fut);
    let mut ready_emitted = false;

    let result = loop {
        tokio::select! {
            ready = &mut bound_addr_rx, if !ready_emitted => {
                if let Ok(addr) = ready {
                    events.emit(ThoughtJackEvent::ActorReady {
                        actor_name: actor_name.to_string(),
                        bind_address: addr.to_string(),
                    });
                }
                ready_emitted = true;
            }
            run_result = &mut run_fut => {
                break run_result?;
            }
        }
    };

    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: actor_name.to_string(),
        reason: result.termination.to_string(),
        phases_completed: result.phases_completed,
    });

    Ok(result)
}

/// Runs an A2A client actor — creates driver and phase loop.
///
/// Client-mode: waits for readiness gate, then sends task messages.
///
/// Implements: TJ-SPEC-017 F-007
async fn run_a2a_client_actor(
    actor_name: &str,
    ctx: ActorRunContext<'_>,
) -> Result<ActorResult, EngineError> {
    let ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx,
        events,
    } = ctx;
    let endpoint = config.a2a_client_endpoint.as_deref().ok_or_else(|| {
        EngineError::Driver("a2a_client mode requires --a2a-client-endpoint".to_string())
    })?;

    // Client actors signal readiness immediately
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    wait_for_readiness_gate(actor_name, gate_rx).await?;

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address: endpoint.to_string(),
    });

    // Create driver (merge mode-specific auth env vars with --header)
    let headers = resolve_headers_for_mode(&config.headers, "a2a_client");
    let driver = a2a_client::create_a2a_client_driver(endpoint, headers, config.raw_synthesize);

    // Create phase engine
    let engine = PhaseEngine::new(document, actor_index);

    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: actor_name.to_string(),
        phase_count,
    });

    // Create and run phase loop (no entry_action_sender — client mode)
    let loop_config = PhaseLoopConfig {
        trace,
        extractor_store,
        actor_name: actor_name.to_string(),
        await_extractors_config: await_config,
        cancel,
        entry_action_sender: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let result = phase_loop.run().await?;

    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: actor_name.to_string(),
        reason: result.termination.to_string(),
        phases_completed: result.phases_completed,
    });

    Ok(result)
}

/// Runs an MCP client actor — creates driver and phase loop.
///
/// Client-mode: signals readiness immediately, waits for readiness gate,
/// spawns server process, creates driver + engine + phase loop.
///
/// Implements: TJ-SPEC-018 F-004
async fn run_mcp_client_actor(
    actor_name: &str,
    ctx: ActorRunContext<'_>,
) -> Result<ActorResult, EngineError> {
    let ActorRunContext {
        actor_index,
        document,
        config,
        trace,
        extractor_store,
        await_config,
        cancel,
        ready_tx,
        gate_rx,
        events,
    } = ctx;
    // Determine transport from CLI flags
    let (driver, bind_address) = match (
        config.mcp_client_command.as_deref(),
        config.mcp_client_endpoint.as_deref(),
    ) {
        (Some(command_raw), _) => {
            let (command, mut args) = parse_mcp_client_command(command_raw)?;
            let extra_args: Vec<String> = config
                .mcp_client_args
                .as_deref()
                .map(parse_mcp_client_args)
                .transpose()?
                .unwrap_or_default();
            args.extend(extra_args);
            let driver = mcp_client::create_mcp_client_driver(
                Some(command.as_str()),
                &args,
                None,
                &[],
                config.raw_synthesize,
            )?;
            (driver, format!("stdio:{command_raw}"))
        }
        (None, Some(endpoint)) => {
            // Merge mode-specific auth env vars with --header
            let headers = resolve_headers_for_mode(&config.headers, "mcp_client");
            let driver = mcp_client::create_mcp_client_driver(
                None,
                &[],
                Some(endpoint),
                &headers,
                config.raw_synthesize,
            )?;
            (driver, endpoint.to_string())
        }
        (None, None) => {
            return Err(EngineError::Driver(
                "mcp_client mode requires --mcp-client-command (stdio) \
                 or --mcp-client-endpoint (HTTP)"
                    .to_string(),
            ));
        }
    };

    // Client actors signal readiness immediately (they don't bind a port)
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    wait_for_readiness_gate(actor_name, gate_rx).await?;

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address,
    });

    // Create phase engine
    let engine = PhaseEngine::new(document, actor_index);

    let phase_count = engine.actor().phases.len();
    events.emit(ThoughtJackEvent::ActorStarted {
        actor_name: actor_name.to_string(),
        phase_count,
    });

    // Create and run phase loop (no entry_action_sender — client mode)
    let loop_config = PhaseLoopConfig {
        trace,
        extractor_store,
        actor_name: actor_name.to_string(),
        await_extractors_config: await_config,
        cancel,
        entry_action_sender: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, loop_config);
    let result = phase_loop.run().await?;

    events.emit(ThoughtJackEvent::ActorCompleted {
        actor_name: actor_name.to_string(),
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

    #[test]
    fn build_actor_config_maps_flags() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: Some("0.0.0.0:8080".to_string()),
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: Some("http://localhost:3000".to_string()),
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec!["Authorization: Bearer token123".to_string()],
            no_semantic: false,
            raw_synthesize: true,
            metrics_port: None,
            events_file: None,
        };

        let config = build_actor_config(&args).expect("valid headers should parse");
        assert_eq!(config.mcp_server_bind, Some("0.0.0.0:8080".to_string()));
        assert_eq!(
            config.agui_client_endpoint,
            Some("http://localhost:3000".to_string())
        );
        assert!(config.raw_synthesize);
        assert_eq!(config.headers.len(), 1);
        assert_eq!(config.headers[0].0, "Authorization");
        assert_eq!(config.headers[0].1, "Bearer token123");
        assert_eq!(config.max_session, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn unsupported_mode_errors() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: unknown_actor
        mode: future_protocol_client
        phases:
          - name: setup
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
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
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            None,
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("not yet implemented"),
            "Expected 'not yet implemented', got: {err}"
        );
    }

    #[tokio::test]
    async fn agui_client_requires_endpoint() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: agui_actor
        mode: ag_ui_client
        phases:
          - name: setup
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
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
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            None,
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("--agui-client-endpoint"),
            "Expected endpoint error, got: {err}"
        );
    }

    #[tokio::test]
    async fn agui_client_fails_when_readiness_gate_closes() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: agui_actor
        mode: ag_ui_client
        phases:
          - name: setup
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "Hello"
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
            mcp_server_bind: None,
            agui_client_endpoint: Some("http://localhost:3000".to_string()),
            a2a_server_bind: None,
            a2a_client_endpoint: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            headers: vec![],
            raw_synthesize: false,
            grace_period: None,
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            Some(rx),
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("readiness gate closed"),
            "Expected gate closed error, got: {err}"
        );
    }

    #[tokio::test]
    async fn a2a_client_fails_when_readiness_gate_closes() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: a2a_actor
        mode: a2a_client
        phases:
          - name: send
            state:
              task_message:
                role: user
                parts:
                  - kind: text
                    text: "Hello"
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
            mcp_server_bind: None,
            agui_client_endpoint: None,
            a2a_server_bind: None,
            a2a_client_endpoint: Some("http://localhost:9090".to_string()),
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            headers: vec![],
            raw_synthesize: false,
            grace_period: None,
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            Some(rx),
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("readiness gate closed"),
            "Expected gate closed error, got: {err}"
        );
    }

    #[tokio::test]
    async fn mcp_client_fails_when_readiness_gate_closes() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: mcp_actor
        mode: mcp_client
        phases:
          - name: probe
            state:
              actions:
                - list_tools
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
            mcp_server_bind: None,
            agui_client_endpoint: None,
            a2a_server_bind: None,
            a2a_client_endpoint: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: Some("http://localhost:8080/mcp".to_string()),
            headers: vec![],
            raw_synthesize: false,
            grace_period: None,
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            Some(rx),
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("readiness gate closed"),
            "Expected gate closed error, got: {err}"
        );
    }

    #[tokio::test]
    async fn a2a_server_mode_recognized() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: a2a_actor
        mode: a2a_server
        phases:
          - name: serve
            state:
              agent_card:
                name: "Test Agent"
                skills: []
                defaultInputModes: ["text/plain"]
                defaultOutputModes: ["text/plain"]
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
            mcp_server_bind: None,
            agui_client_endpoint: None,
            a2a_server_bind: Some("127.0.0.1:0".to_string()),
            a2a_client_endpoint: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            headers: vec![],
            raw_synthesize: false,
            grace_period: None,
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Cancel after a short delay to avoid hanging
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            cancel,
            None,
            None,
            &EventEmitter::noop(),
        )
        .await;

        let actor_result = result.expect("a2a_server actor should run to a graceful termination");
        assert_eq!(actor_result.actor_name, "a2a_actor");
        assert!(
            matches!(
                actor_result.termination,
                crate::engine::types::TerminationReason::Cancelled
                    | crate::engine::types::TerminationReason::TerminalPhaseReached
            ),
            "unexpected termination: {:?}",
            actor_result.termination
        );
    }

    #[tokio::test]
    async fn a2a_client_requires_endpoint() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: a2a_actor
        mode: a2a_client
        phases:
          - name: send
            state:
              task_message:
                role: user
                parts:
                  - kind: text
                    text: "Hello"
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
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
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            None,
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("--a2a-client-endpoint"),
            "Expected endpoint error, got: {err}"
        );
    }

    #[tokio::test]
    async fn mcp_server_stdio_runs_to_completion() {
        // mcp_server actor with terminal phase, no bind address (stdio mode).
        // Null transport reaches EOF immediately, with cancellation as a timeout safety net.
        let yaml = r#"
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
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
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
            max_session: Duration::from_secs(5),
            readiness_timeout: Duration::from_secs(5),
            transport_factory: Some(null_transport_factory()),
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Cancel after short delay — NullTransport returns EOF immediately.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel_clone.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run_actor(
                0,
                doc,
                &config,
                SharedTrace::new(),
                ExtractorStore::new(),
                HashMap::new(),
                cancel,
                None,
                None,
                &EventEmitter::noop(),
            ),
        )
        .await
        .expect("test timed out — stdio actor did not respond to cancellation within 10s");

        let actor_result = result.expect("mcp_server actor should run to a graceful termination");
        assert_eq!(actor_result.actor_name, "default");
        assert!(
            matches!(
                actor_result.termination,
                crate::engine::types::TerminationReason::Cancelled
                    | crate::engine::types::TerminationReason::TerminalPhaseReached
            ),
            "unexpected termination: {:?}",
            actor_result.termination
        );
    }

    #[test]
    fn build_actor_config_parses_multiple_headers() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec![
                "Authorization: Bearer token123".to_string(),
                "X-Custom: value with : colons".to_string(),
                "Accept : application/json".to_string(),
            ],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        };

        let config = build_actor_config(&args).expect("valid headers should parse");
        assert_eq!(config.headers.len(), 3);
        // Standard header
        assert_eq!(config.headers[0].0, "Authorization");
        assert_eq!(config.headers[0].1, "Bearer token123");
        // Value with extra colons — splitn(2, ':') keeps them in value
        assert_eq!(config.headers[1].0, "X-Custom");
        assert_eq!(config.headers[1].1, "value with : colons");
        // Extra spaces — trimmed
        assert_eq!(config.headers[2].0, "Accept");
        assert_eq!(config.headers[2].1, "application/json");
    }

    #[test]
    fn build_actor_config_header_without_colon_rejected() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec!["NoColonHere".to_string(), "Valid: header".to_string()],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        };

        let err = build_actor_config(&args).expect_err("missing colon should be rejected");
        assert!(
            err.to_string().contains("expected KEY:VALUE"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_actor_config_invalid_header_name_rejected() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec!["Bad Name: value".to_string()],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        };

        let err = build_actor_config(&args).expect_err("invalid header name should be rejected");
        assert!(
            err.to_string().contains("valid HTTP header name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_actor_config_invalid_header_value_rejected() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec!["X-Test: value\r\ninjected".to_string()],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        };

        let err = build_actor_config(&args).expect_err("invalid header value should be rejected");
        assert!(
            err.to_string().contains("valid HTTP header value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_actor_config_defaults() {
        let args = RunArgs {
            config: Some(std::path::PathBuf::from("test.yaml")),
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(30)),
            output: None,
            header: vec![],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        };

        let config = build_actor_config(&args).expect("empty header list should be valid");
        assert!(config.mcp_server_bind.is_none());
        assert!(config.agui_client_endpoint.is_none());
        assert!(config.a2a_server_bind.is_none());
        assert!(config.a2a_client_endpoint.is_none());
        assert!(config.mcp_client_command.is_none());
        assert!(config.mcp_client_args.is_none());
        assert!(config.mcp_client_endpoint.is_none());
        assert!(!config.raw_synthesize);
        assert!(config.headers.is_empty());
        assert!(config.grace_period.is_none());
    }

    #[tokio::test]
    async fn mcp_client_requires_command_or_endpoint() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: mcp_actor
        mode: mcp_client
        phases:
          - name: probe
            state:
              actions:
                - list_tools
"#;
        let doc = oatf::load(yaml).unwrap().document;
        let config = ActorConfig {
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
            max_session: Duration::from_secs(300),
            readiness_timeout: Duration::from_secs(30),
            transport_factory: None,
        };

        let result = run_actor(
            0,
            doc,
            &config,
            SharedTrace::new(),
            ExtractorStore::new(),
            HashMap::new(),
            CancellationToken::new(),
            None,
            None,
            &EventEmitter::noop(),
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("mcp_client mode requires"),
            "Expected transport error, got: {err}"
        );
    }

    // ---- Header resolution tests ----

    #[test]
    fn resolve_headers_passthrough_for_server_mode() {
        let base = vec![("X-Custom".to_string(), "val".to_string())];
        let resolved = resolve_headers_for_mode(&base, "mcp_server");
        assert_eq!(resolved, base, "server modes should pass through unchanged");
    }

    #[test]
    fn mode_env_prefix_maps_client_modes() {
        assert_eq!(mode_env_prefix("mcp_client"), Some("MCP_CLIENT"));
        assert_eq!(mode_env_prefix("a2a_client"), Some("A2A_CLIENT"));
        assert_eq!(mode_env_prefix("ag_ui_client"), Some("AGUI"));
        assert_eq!(mode_env_prefix("mcp_server"), None);
        assert_eq!(mode_env_prefix("a2a_server"), None);
    }

    #[test]
    fn merge_headers_override_replaces_base() {
        let base = vec![("Authorization".to_string(), "Bearer cli-token".to_string())];
        let overrides = vec![("Authorization".to_string(), "Bearer env-token".to_string())];
        let merged = merge_headers(&base, &overrides);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].1, "Bearer env-token");
    }

    #[test]
    fn merge_headers_case_insensitive() {
        let base = vec![("authorization".to_string(), "Bearer cli".to_string())];
        let overrides = vec![("Authorization".to_string(), "Bearer env".to_string())];
        let merged = merge_headers(&base, &overrides);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].0, "Authorization");
        assert_eq!(merged[0].1, "Bearer env");
    }

    #[test]
    fn merge_headers_appends_new() {
        let base = vec![("Accept".to_string(), "application/json".to_string())];
        let overrides = vec![("X-Api-Key".to_string(), "key-123".to_string())];
        let merged = merge_headers(&base, &overrides);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, "Accept");
        assert_eq!(merged[1].0, "X-Api-Key");
    }

    #[test]
    fn merge_headers_empty_override_preserves_base() {
        let base = vec![
            ("Accept".to_string(), "application/json".to_string()),
            ("X-Custom".to_string(), "value".to_string()),
        ];
        let merged = merge_headers(&base, &[]);
        assert_eq!(merged, base);
    }

    #[test]
    fn merge_headers_empty_base_uses_overrides() {
        let overrides = vec![("Authorization".to_string(), "Bearer token".to_string())];
        let merged = merge_headers(&[], &overrides);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].1, "Bearer token");
    }

    #[test]
    fn parse_mcp_client_args_respects_quotes() {
        let parsed = parse_mcp_client_args(r#"--flag "two words" 'three words'"#).unwrap();
        assert_eq!(
            parsed,
            vec![
                "--flag".to_string(),
                "two words".to_string(),
                "three words".to_string(),
            ]
        );
    }

    #[test]
    fn parse_mcp_client_args_rejects_unbalanced_quotes() {
        let err = parse_mcp_client_args("\"oops").unwrap_err();
        assert!(err.to_string().contains("invalid --mcp-client-args"));
    }

    #[test]
    fn parse_mcp_client_command_supports_inline_args() {
        let (command, args) =
            parse_mcp_client_command("npx -y @modelcontextprotocol/server-everything").unwrap();
        assert_eq!(command, "npx");
        assert_eq!(
            args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-everything".to_string()
            ]
        );
    }

    #[test]
    fn parse_mcp_client_command_rejects_empty() {
        let err = parse_mcp_client_command("").unwrap_err();
        assert!(err.to_string().contains("invalid --mcp-client-command"));
    }

    #[test]
    fn parse_mcp_client_command_rejects_unbalanced_quotes() {
        let err = parse_mcp_client_command("\"oops").unwrap_err();
        assert!(err.to_string().contains("invalid --mcp-client-command"));
    }
}
