use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use oatf::primitives::{interpolate_template, interpolate_value};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;

use super::handler::{normalize_action, server_request_handler};
use super::multiplexer::MessageMultiplexer;
use super::transport::{
    McpClientTransportReader, McpClientTransportWriter, create_http_transport,
    spawn_stdio_transport,
};
use super::{
    CorrelatedResponse, DEFAULT_PHASE_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, HandlerState, INIT_TIMEOUT,
    NotificationMessage, PendingRequest, SERVER_REQUEST_BUFFER_SIZE,
};

// ============================================================================
// McpClientDriver
// ============================================================================

/// MCP client-mode protocol driver.
///
/// Connects to an MCP server, sends JSON-RPC requests, handles
/// server-initiated requests via a background handler task, and
/// emits protocol events for the `PhaseLoop`.
///
/// Implements: TJ-SPEC-018 F-004
pub struct McpClientDriver {
    /// Shared writer (driver + handler both write).
    pub(super) writer: Arc<tokio::sync::Mutex<Box<dyn McpClientTransportWriter>>>,
    /// Pending request map for response correlation.
    pub(super) pending: Arc<std::sync::Mutex<HashMap<String, PendingRequest>>>,
    /// Multiplexer (spawned on first `drive_phase()`).
    pub(super) mux: Option<MessageMultiplexer>,
    /// Notification receiver from multiplexer.
    pub(super) notification_rx: Option<mpsc::UnboundedReceiver<NotificationMessage>>,
    /// Handler event receiver (handler emits events here, driver forwards to `PhaseLoop`).
    pub(super) handler_event_rx: Option<mpsc::UnboundedReceiver<ProtocolEvent>>,
    /// Shared handler state.
    pub(super) handler_state: Arc<tokio::sync::RwLock<HandlerState>>,
    /// Handler task join handle.
    pub(super) handler_handle: Option<JoinHandle<()>>,
    /// Server capabilities (captured during init).
    pub(super) server_capabilities: Option<Value>,
    /// Per-request timeout.
    pub(super) request_timeout: std::time::Duration,
    /// Post-action event loop timeout.
    pub(super) phase_timeout: std::time::Duration,
    /// Whether initialization has completed.
    pub(super) initialized: bool,
    /// Next request ID counter.
    pub(super) next_request_id: u64,
    /// Bypass synthesize output validation.
    pub(super) raw_synthesize: bool,
    /// Transport reader (consumed on first `drive_phase()`).
    pub(super) reader: Option<Box<dyn McpClientTransportReader>>,
    /// Cancellation token for background tasks.
    pub(super) transport_cancel: CancellationToken,
    /// Spawned child process (for stdio transport).
    #[allow(dead_code)]
    pub(super) child: Option<Child>,
    /// Stderr from spawned child (for diagnostics on exit).
    pub(super) child_stderr: Option<ChildStderr>,
}

impl McpClientDriver {
    /// Try to read buffered stderr from the child process.
    ///
    /// Consumes the stderr handle. Returns the output (truncated to 4 KB)
    /// or an empty string if no stderr is available.
    async fn capture_stderr(&mut self) -> String {
        let Some(mut stderr) = self.child_stderr.take() else {
            return String::new();
        };
        let mut buf = vec![0u8; 4096];
        match tokio::time::timeout(std::time::Duration::from_millis(100), stderr.read(&mut buf))
            .await
        {
            Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buf[..n]).to_string(),
            _ => String::new(),
        }
    }

    /// Generate the next monotonically increasing request ID.
    pub(super) const fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Send a JSON-RPC request and await its correlated response.
    ///
    /// Registers the oneshot channel BEFORE sending to prevent races.
    /// Server-initiated requests are handled concurrently by the
    /// multiplexer + handler while this method awaits.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on timeout or multiplexer close.
    async fn send_and_await(
        &mut self,
        method: &str,
        params: Option<Value>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<CorrelatedResponse, EngineError> {
        let id = json!(self.next_id());
        let id_key = id.to_string();

        // Register pending request for correlation (includes params for qualifier resolution)
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id_key,
                PendingRequest {
                    method: method.to_string(),
                    params: params.clone(),
                },
            );

        // Register response channel BEFORE sending (prevents race)
        let mux = self
            .mux
            .as_ref()
            .ok_or_else(|| EngineError::Driver("multiplexer not started".to_string()))?;
        let response_rx = mux.register_response(&id);

        // Send request
        self.writer
            .lock()
            .await
            .send_request_with_id(method, params.clone(), &id)
            .await?;

        // Emit outgoing event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: method.to_string(),
            content: params.unwrap_or(Value::Null),
        });

        // Await response via oneshot — multiplexer handles concurrent server requests
        let response = match tokio::time::timeout(self.request_timeout, response_rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => {
                let reason = mux.close_reason();
                let stderr = self.capture_stderr().await;
                let mut msg = format!("multiplexer closed while awaiting '{method}': {reason}");
                if !stderr.is_empty() {
                    use std::fmt::Write;
                    let _ = write!(msg, "\nserver stderr: {stderr}");
                }
                return Err(EngineError::Driver(msg));
            }
            Err(_) => {
                return Err(EngineError::Driver(format!(
                    "request timeout for '{method}' after {:?}",
                    self.request_timeout
                )));
            }
        };

        // Emit incoming event.
        // Merge request params into response content so qualifier resolution
        // can access the original request context (e.g., tools/call:calculator
        // resolves from params.name). Request params are placed under
        // `_request` to avoid collisions with response fields.
        let mut content = response.result.clone();
        if let Some(ref req_params) = response.request_params
            && let Some(obj) = content.as_object_mut()
        {
            obj.insert("_request".to_string(), req_params.clone());
        }
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: response.method.clone(),
            content,
        });

        Ok(response)
    }

    /// Forward any buffered events from handler and notifications to the `PhaseLoop`.
    ///
    /// Called between actions to minimize event forwarding latency.
    pub(super) fn forward_pending_events(
        &mut self,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) {
        if let Some(ref mut rx) = self.handler_event_rx {
            while let Ok(evt) = rx.try_recv() {
                let _ = event_tx.send(evt);
            }
        }
        if let Some(ref mut rx) = self.notification_rx {
            while let Ok(notif) = rx.try_recv() {
                let _ = event_tx.send(ProtocolEvent {
                    direction: Direction::Incoming,
                    method: notif.method,
                    content: notif.params.unwrap_or(Value::Null),
                });
            }
        }
    }

    /// Perform the MCP initialization handshake.
    ///
    /// Sends `initialize` request, captures server capabilities,
    /// sends `notifications/initialized` notification.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` if initialization fails.
    ///
    /// Implements: TJ-SPEC-018 F-005
    pub(super) async fn initialize(
        &mut self,
        state: &Value,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<(), EngineError> {
        let init_params = json!({
            "protocolVersion": "2025-11-25",
            "capabilities": build_client_capabilities(state),
            "clientInfo": {
                "name": "ThoughtJack",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let id = json!(self.next_id());
        let id_key = id.to_string();

        // Register pending request
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id_key,
                PendingRequest {
                    method: "initialize".to_string(),
                    params: Some(init_params.clone()),
                },
            );

        // Register response channel BEFORE sending
        let mux = self
            .mux
            .as_ref()
            .ok_or_else(|| EngineError::Driver("multiplexer not started".to_string()))?;
        let response_rx = mux.register_response(&id);

        // Send initialize request
        self.writer
            .lock()
            .await
            .send_request_with_id("initialize", Some(init_params.clone()), &id)
            .await?;

        // Emit outgoing event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "initialize".to_string(),
            content: init_params,
        });

        // Await response
        let response = match tokio::time::timeout(INIT_TIMEOUT, response_rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => {
                let reason = mux.close_reason();
                let stderr = self.capture_stderr().await;
                let mut msg = format!("multiplexer closed during initialization: {reason}");
                if !stderr.is_empty() {
                    use std::fmt::Write;
                    let _ = write!(msg, "\nserver stderr: {stderr}");
                }
                return Err(EngineError::Driver(msg));
            }
            Err(_) => {
                return Err(EngineError::Driver("initialization timeout".to_string()));
            }
        };

        // Check for error response (EC-MCPC-005)
        if response.is_error {
            return Err(EngineError::Driver(format!(
                "server rejected initialization: {}",
                response.result
            )));
        }

        // Capture server capabilities
        self.server_capabilities = Some(response.result.clone());

        // Emit incoming event
        let _ = event_tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: "initialize".to_string(),
            content: response.result,
        });

        // Send initialized notification
        self.writer
            .lock()
            .await
            .send_notification("notifications/initialized", None)
            .await?;

        self.initialized = true;
        tracing::info!("MCP client initialization complete");

        Ok(())
    }

    /// Execute a single normalized action.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on request/response failure.
    ///
    /// Implements: TJ-SPEC-018 F-006
    async fn execute_action(
        &mut self,
        action: &Value,
        extractors: &HashMap<String, String>,
        event_tx: &mpsc::UnboundedSender<ProtocolEvent>,
    ) -> Result<(), EngineError> {
        let action_type = action["type"].as_str().unwrap_or("");

        match action_type {
            "list_tools" => {
                self.send_and_await("tools/list", None, event_tx).await?;
            }
            "call_tool" => {
                let raw_name = action["name"].as_str().unwrap_or_default();
                let (name, _) = interpolate_template(raw_name, extractors, None, None);
                let arguments = action.get("arguments").cloned().unwrap_or(json!({}));
                let (interpolated_args, _) = interpolate_value(&arguments, extractors, None, None);
                let params = json!({"name": name, "arguments": interpolated_args});
                self.send_and_await("tools/call", Some(params), event_tx)
                    .await?;
            }
            "list_resources" => {
                self.send_and_await("resources/list", None, event_tx)
                    .await?;
            }
            "read_resource" => {
                let raw_uri = action["uri"].as_str().unwrap_or_default();
                let (uri, _) = interpolate_template(raw_uri, extractors, None, None);
                let params = json!({"uri": uri});
                self.send_and_await("resources/read", Some(params), event_tx)
                    .await?;
            }
            "list_prompts" => {
                self.send_and_await("prompts/list", None, event_tx).await?;
            }
            "get_prompt" => {
                let raw_name = action["name"].as_str().unwrap_or_default();
                let (name, _) = interpolate_template(raw_name, extractors, None, None);
                let arguments = action.get("arguments").cloned().unwrap_or(json!({}));
                let (interpolated_args, _) = interpolate_value(&arguments, extractors, None, None);
                let params = json!({"name": name, "arguments": interpolated_args});
                self.send_and_await("prompts/get", Some(params), event_tx)
                    .await?;
            }
            "subscribe_resource" => {
                let raw_uri = action["uri"].as_str().unwrap_or_default();
                let (uri, _) = interpolate_template(raw_uri, extractors, None, None);
                let params = json!({"uri": uri});
                self.send_and_await("resources/subscribe", Some(params), event_tx)
                    .await?;
            }
            unknown => {
                tracing::warn!(action_type = %unknown, "unknown MCP client action type, skipping");
            }
        }

        Ok(())
    }

    /// Bootstrap the multiplexer and handler on first `drive_phase()` call.
    pub(super) fn bootstrap(&mut self, extractors: watch::Receiver<HashMap<String, String>>) {
        let reader = self
            .reader
            .take()
            .expect("reader should be available on first drive_phase");

        // Create channels
        let (server_request_tx, server_request_rx) = mpsc::channel(SERVER_REQUEST_BUFFER_SIZE);
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let (handler_event_tx, handler_event_rx) = mpsc::unbounded_channel();

        // Create multiplexer shared state
        let response_senders = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let close_reason = Arc::new(std::sync::Mutex::new(None));

        // Spawn multiplexer
        let mux = MessageMultiplexer::spawn(
            reader,
            Arc::clone(&self.writer),
            Arc::clone(&self.pending),
            server_request_tx,
            notification_tx,
            response_senders,
            close_reason,
            self.transport_cancel.clone(),
        );

        // Spawn handler
        let handler_handle = tokio::spawn(server_request_handler(
            server_request_rx,
            Arc::clone(&self.writer),
            Arc::clone(&self.handler_state),
            extractors, // Ownership transfer — handler holds this for the driver's lifetime
            handler_event_tx,
            self.raw_synthesize,
            self.transport_cancel.clone(),
        ));

        self.mux = Some(mux);
        self.notification_rx = Some(notification_rx);
        self.handler_event_rx = Some(handler_event_rx);
        self.handler_handle = Some(handler_handle);
    }
}

// ============================================================================
// PhaseDriver Implementation
// ============================================================================

#[async_trait]
impl PhaseDriver for McpClientDriver {
    /// Execute the MCP client protocol work for a single phase.
    ///
    /// On the first call, bootstraps the multiplexer and handler.
    /// Performs initialization handshake if not yet done.
    /// Executes phase actions in order, forwarding handler events between each.
    /// After actions, enters an event loop forwarding handler/notification events
    /// until cancel or phase timeout.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Driver` on protocol-level failures.
    ///
    /// Implements: TJ-SPEC-018 F-004
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        // Bootstrap on first call: spawn multiplexer and handler
        if self.mux.is_none() {
            self.bootstrap(extractors.clone());
        }

        // Initialize on first call
        if !self.initialized {
            self.initialize(state, &event_tx).await?;
        }

        // Update handler state for this phase
        {
            let mut hs = self.handler_state.write().await;
            hs.state = state.clone();
        }

        // Clone extractors for action interpolation
        let current_extractors = extractors.borrow().clone();

        // Execute actions defined in the phase state
        if let Some(actions) = state.get("actions").and_then(Value::as_array) {
            for action_value in actions {
                // Forward any buffered handler events before each action
                self.forward_pending_events(&event_tx);

                // Normalize and execute action
                let normalized = normalize_action(action_value);
                self.execute_action(&normalized, &current_extractors, &event_tx)
                    .await?;
            }
        }

        // Post-action event loop: forward handler and notification events
        // until cancel fires or phase_timeout expires. PhaseLoop checks triggers
        // on each forwarded event and will cancel if a trigger fires.
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    break;
                }
                // Monitor handler task for panics
                result = async {
                    if let Some(ref mut handle) = self.handler_handle {
                        handle.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Err(join_err) = result {
                        tracing::error!(
                            error = %join_err,
                            "server request handler task panicked"
                        );
                    }
                    // Handler exited (normally or via panic) — stop event loop
                    self.handler_handle = None;
                    break;
                }
                evt = async {
                    if let Some(ref mut rx) = self.handler_event_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(evt) = evt {
                        let _ = event_tx.send(evt);
                    } else {
                        break;
                    }
                }
                notif = async {
                    if let Some(ref mut rx) = self.notification_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(n) = notif {
                        let _ = event_tx.send(ProtocolEvent {
                            direction: Direction::Incoming,
                            method: n.method,
                            content: n.params.unwrap_or(Value::Null),
                        });
                    } else {
                        break;
                    }
                }
                () = tokio::time::sleep(self.phase_timeout) => {
                    break;
                }
            }
        }

        Ok(DriveResult::Complete)
    }

    async fn on_phase_advanced(&mut self, _from: usize, _to: usize) -> Result<(), EngineError> {
        // No-op: handler state updated at start of next drive_phase,
        // extractors come from watch channel (always fresh).
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Build client capabilities from the phase state.
///
/// Advertises sampling, elicitation, and roots support based on
/// whether the state defines the corresponding response fields.
///
/// Implements: TJ-SPEC-018 F-005
pub(super) fn build_client_capabilities(state: &Value) -> Value {
    let mut caps = json!({});

    if state.get("sampling_responses").is_some() {
        caps["sampling"] = json!({});
    }
    if state.get("roots").is_some() {
        caps["roots"] = json!({"listChanged": false});
    }
    if state.get("elicitation_responses").is_some() {
        caps["elicitation"] = json!({});
    }

    caps
}

// ============================================================================
// Factory Function
// ============================================================================

/// Creates an `McpClientDriver` for stdio or HTTP transport.
///
/// Provide `command` for stdio transport (spawns a server process),
/// or `endpoint` for MCP Streamable HTTP transport.
///
/// # Errors
///
/// Returns `EngineError::Driver` if neither transport option is provided,
/// or if process spawn / HTTP client creation fails.
///
/// Implements: TJ-SPEC-018 F-001
pub fn create_mcp_client_driver(
    command: Option<&str>,
    args: &[String],
    endpoint: Option<&str>,
    headers: &[(String, String)],
    raw_synthesize: bool,
) -> Result<McpClientDriver, EngineError> {
    #[allow(clippy::type_complexity)]
    let (reader, writer, child, child_stderr): (
        Box<dyn McpClientTransportReader>,
        Box<dyn McpClientTransportWriter>,
        Option<Child>,
        Option<ChildStderr>,
    ) = match (command, endpoint) {
        (Some(cmd), _) => {
            let (r, w, mut c) = spawn_stdio_transport(cmd, args)?;
            let stderr = c.stderr.take();
            (Box::new(r), Box::new(w), Some(c), stderr)
        }
        (None, Some(ep)) => {
            let (r, w) = create_http_transport(ep, headers)?;
            (Box::new(r), Box::new(w), None, None)
        }
        (None, None) => {
            return Err(EngineError::Driver(
                "mcp_client mode requires --mcp-client-command (stdio) \
                 or --mcp-client-endpoint (HTTP)"
                    .to_string(),
            ));
        }
    };

    let transport_cancel = CancellationToken::new();

    Ok(McpClientDriver {
        writer: Arc::new(tokio::sync::Mutex::new(writer)),
        pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
        mux: None,
        notification_rx: None,
        handler_event_rx: None,
        handler_state: Arc::new(tokio::sync::RwLock::new(HandlerState {
            state: Value::Null,
        })),
        handler_handle: None,
        server_capabilities: None,
        request_timeout: DEFAULT_REQUEST_TIMEOUT,
        phase_timeout: DEFAULT_PHASE_TIMEOUT,
        initialized: false,
        next_request_id: 1,
        raw_synthesize,
        reader: Some(reader),
        transport_cancel,
        child,
        child_stderr,
    })
}
