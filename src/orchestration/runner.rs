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

use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::cli::args::RunArgs;
use crate::engine::mcp_server::McpServerDriver;
use crate::engine::phase::PhaseEngine;
use crate::engine::phase_loop::{PhaseLoop, PhaseLoopConfig};
use crate::engine::trace::SharedTrace;
use crate::engine::types::{ActorResult, AwaitExtractor};
use crate::error::EngineError;
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::orchestration::store::ExtractorStore;
use crate::protocol::{a2a_client, a2a_server, agui};
use crate::transport::http::HttpConfig;
use crate::transport::{HttpTransport, StdioTransport};

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
}

/// Builds an `ActorConfig` from CLI `RunArgs`.
///
/// Implements: TJ-SPEC-015 F-003
#[must_use]
pub fn build_actor_config(args: &RunArgs) -> ActorConfig {
    let headers: Vec<(String, String)> = args
        .header
        .iter()
        .filter_map(|h| {
            let mut parts = h.splitn(2, ':');
            let key = parts.next()?.trim().to_string();
            let value = parts.next()?.trim().to_string();
            Some((key, value))
        })
        .collect();

    ActorConfig {
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
    }
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
    let actors = document
        .attack
        .execution
        .actors
        .as_ref()
        .expect("document should have actors after normalization");
    let actor = &actors[actor_index];
    let actor_name = actor.name.clone();
    let mode = &actor.mode;

    events.emit(ThoughtJackEvent::ActorInit {
        actor_name: actor_name.clone(),
        mode: mode.clone(),
    });

    match mode.as_str() {
        "mcp_server" => {
            run_mcp_server_actor(
                actor_index,
                &actor_name,
                document,
                config,
                trace,
                extractor_store,
                await_config,
                cancel,
                ready_tx,
                gate_rx,
                events,
            )
            .await
        }
        "ag_ui_client" => {
            run_agui_client_actor(
                actor_index,
                &actor_name,
                document,
                config,
                trace,
                extractor_store,
                await_config,
                cancel,
                ready_tx,
                gate_rx,
                events,
            )
            .await
        }
        "a2a_server" => {
            run_a2a_server_actor(
                actor_index,
                &actor_name,
                document,
                config,
                trace,
                extractor_store,
                await_config,
                cancel,
                ready_tx,
                gate_rx,
                events,
            )
            .await
        }
        "a2a_client" => {
            run_a2a_client_actor(
                actor_index,
                &actor_name,
                document,
                config,
                trace,
                extractor_store,
                await_config,
                cancel,
                ready_tx,
                gate_rx,
                events,
            )
            .await
        }
        other => Err(EngineError::Driver(format!(
            "driver for mode '{other}' not yet implemented"
        ))),
    }
}

/// Runs an MCP server actor — creates transport, driver, and phase loop.
#[allow(clippy::too_many_arguments)]
async fn run_mcp_server_actor(
    actor_index: usize,
    actor_name: &str,
    document: oatf::Document,
    config: &ActorConfig,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    ready_tx: Option<oneshot::Sender<()>>,
    _gate_rx: Option<broadcast::Receiver<()>>,
    events: &EventEmitter,
) -> Result<ActorResult, EngineError> {
    // Create transport based on CLI flags
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
            // stdio transport — ready immediately
            let transport = StdioTransport::new();

            events.emit(ThoughtJackEvent::ActorReady {
                actor_name: actor_name.to_string(),
                bind_address: "stdio".to_string(),
            });

            if let Some(tx) = ready_tx {
                let _ = tx.send(());
            }

            Arc::new(transport)
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
#[allow(clippy::too_many_arguments)]
async fn run_agui_client_actor(
    actor_index: usize,
    actor_name: &str,
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
    let endpoint = config.agui_client_endpoint.as_deref().ok_or_else(|| {
        EngineError::Driver("ag_ui_client mode requires --agui-client-endpoint".to_string())
    })?;

    // Client actors signal readiness immediately (they don't bind a port)
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    // Wait for server actors to be ready
    if let Some(mut rx) = gate_rx {
        tracing::debug!(actor = %actor_name, "waiting for server readiness gate");
        let _ = rx.recv().await;
        tracing::debug!(actor = %actor_name, "readiness gate opened");
    }

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address: endpoint.to_string(),
    });

    // Create driver
    let driver = agui::create_agui_driver(endpoint, config.headers.clone(), config.raw_synthesize);

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
/// Server-mode: binds HTTP transport inside `drive_phase()`, signals readiness.
///
/// Implements: TJ-SPEC-017 F-001
#[allow(clippy::too_many_arguments)]
async fn run_a2a_server_actor(
    actor_index: usize,
    actor_name: &str,
    document: oatf::Document,
    config: &ActorConfig,
    trace: SharedTrace,
    extractor_store: ExtractorStore,
    await_config: HashMap<usize, Vec<AwaitExtractor>>,
    cancel: CancellationToken,
    ready_tx: Option<oneshot::Sender<()>>,
    _gate_rx: Option<broadcast::Receiver<()>>,
    events: &EventEmitter,
) -> Result<ActorResult, EngineError> {
    let bind_addr = config
        .a2a_server_bind
        .as_deref()
        .unwrap_or("127.0.0.1:9090");

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address: bind_addr.to_string(),
    });

    // Signal readiness (server binds inside drive_phase, but we signal
    // immediately since the bind happens before accepting requests)
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    // Create driver
    let driver = a2a_server::create_a2a_server_driver(bind_addr, config.raw_synthesize);

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
    let result = phase_loop.run().await?;

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
#[allow(clippy::too_many_arguments)]
async fn run_a2a_client_actor(
    actor_index: usize,
    actor_name: &str,
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
    let endpoint = config.a2a_client_endpoint.as_deref().ok_or_else(|| {
        EngineError::Driver("a2a_client mode requires --a2a-client-endpoint".to_string())
    })?;

    // Client actors signal readiness immediately
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    // Wait for server actors to be ready
    if let Some(mut rx) = gate_rx {
        tracing::debug!(actor = %actor_name, "waiting for server readiness gate");
        let _ = rx.recv().await;
        tracing::debug!(actor = %actor_name, "readiness gate opened");
    }

    events.emit(ThoughtJackEvent::ActorReady {
        actor_name: actor_name.to_string(),
        bind_address: endpoint.to_string(),
    });

    // Create driver
    let driver = a2a_client::create_a2a_client_driver(
        endpoint,
        config.headers.clone(),
        config.raw_synthesize,
    );

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
            config: std::path::PathBuf::from("test.yaml"),
            mcp_server: Some("0.0.0.0:8080".to_string()),
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: Some("http://localhost:3000".to_string()),
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(300)),
            output: None,
            header: vec!["Authorization: Bearer token123".to_string()],
            no_semantic: false,
            raw_synthesize: true,
            metrics_port: None,
            events_file: None,
        };

        let config = build_actor_config(&args);
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

        // Should not get "not yet implemented" — should succeed or cancel gracefully
        assert!(
            result.is_ok(),
            "Expected a2a_server to be recognized, got: {result:?}"
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
}
