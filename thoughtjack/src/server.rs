//! Server runtime orchestrating MCP request handling (TJ-SPEC-002 / TJ-SPEC-003).
//!
//! The [`Server`] wires together the transport, phase engine, behavior
//! coordinator, and handler dispatch into a running MCP server.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::behavior::{BehaviorCoordinator, ResolvedBehavior, record_delivery_metrics};
use crate::config::schema::{
    BaselineState, BehaviorConfig, EntryAction, GeneratorLimits, ServerConfig, SideEffectTrigger,
    UnknownMethodHandling,
};
use crate::error::ThoughtJackError;
use crate::handlers;
use crate::observability::events::{Event, EventEmitter};
use crate::observability::metrics;
use crate::phase::engine::PhaseEngine;
use crate::phase::state::EventType;
use crate::transport::jsonrpc::{
    JSONRPC_VERSION, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    error_codes,
};
use crate::transport::{ConnectionContext, Transport};

/// MCP server runtime.
///
/// Orchestrates request handling by coordinating the transport layer,
/// phase engine, behavior coordinator, and handler dispatch.
///
/// Implements: TJ-SPEC-002 F-001
pub struct Server {
    config: Arc<ServerConfig>,
    transport: Box<dyn Transport>,
    phase_engine: Arc<PhaseEngine>,
    behavior_coordinator: BehaviorCoordinator,
    event_emitter: EventEmitter,
    connection: ConnectionContext,
    generator_limits: GeneratorLimits,
    cancel: CancellationToken,
}

impl Server {
    /// Creates a new server from configuration and transport.
    ///
    /// Converts the `ServerConfig` into a baseline + phases pair and
    /// initialises all subsystems. The `cli_behavior` override (if
    /// provided) takes highest priority in the behaviour scoping chain.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn new(
        config: Arc<ServerConfig>,
        transport: Box<dyn Transport>,
        cli_behavior: Option<BehaviorConfig>,
        event_emitter: EventEmitter,
    ) -> Self {
        let (baseline, phases) = build_baseline_and_phases(&config);
        let phase_engine = Arc::new(PhaseEngine::new(phases, baseline));
        let behavior_coordinator = BehaviorCoordinator::new(cli_behavior);
        let connection = ConnectionContext::stdio();
        let generator_limits = GeneratorLimits::default();
        let cancel = CancellationToken::new();

        Self {
            config,
            transport,
            phase_engine,
            behavior_coordinator,
            event_emitter,
            connection,
            generator_limits,
            cancel,
        }
    }

    /// Runs the server's main request-handling loop.
    ///
    /// Receives messages from the transport, dispatches them to handlers,
    /// delivers responses via the resolved behavior, and manages phase
    /// transitions.
    ///
    /// The loop exits on EOF, cancellation, or unrecoverable error.
    ///
    /// # Errors
    ///
    /// Returns an error if the transport fails fatally.
    ///
    /// Implements: TJ-SPEC-002 F-001
    pub async fn run(&self) -> Result<(), ThoughtJackError> {
        let server_name = &self.config.server.name;
        let server_version = self.config.server.version.as_deref().unwrap_or("0.0.0");
        let transport_type = self.transport.transport_type();

        // Emit startup event
        self.event_emitter.emit(Event::ServerStarted {
            timestamp: Utc::now(),
            server_name: server_name.clone(),
            transport: transport_type.to_string(),
        });

        metrics::set_current_phase(self.phase_engine.current_phase_name());

        // Start background timer task for time-based triggers
        let timer_handle = self.phase_engine.start_timer_task();

        // Spawn continuous side effects (if any in the initial effective state)
        let _continuous_handles = self.spawn_continuous_side_effects();

        let mut initialized = false;

        let result = self
            .main_loop(server_name, server_version, &mut initialized)
            .await;

        // Shutdown
        self.phase_engine.shutdown();
        timer_handle.abort();

        self.event_emitter.emit(Event::ServerStopped {
            timestamp: Utc::now(),
            reason: match &result {
                Ok(()) => "EOF".to_string(),
                Err(e) => format!("error: {e}"),
            },
        });

        result
    }

    /// Core message loop.
    async fn main_loop(
        &self,
        server_name: &str,
        server_version: &str,
        initialized: &mut bool,
    ) -> Result<(), ThoughtJackError> {
        loop {
            // Wait for next message or cancellation
            let message = tokio::select! {
                () = self.cancel.cancelled() => {
                    info!("server cancelled");
                    break;
                }
                msg = self.transport.receive_message() => msg?,
            };

            let Some(message) = message else {
                debug!("transport EOF — shutting down");
                break;
            };

            // Only handle requests; skip responses and notifications
            let request = match message {
                JsonRpcMessage::Request(req) => req,
                JsonRpcMessage::Response(_) => {
                    debug!("ignoring incoming response");
                    continue;
                }
                JsonRpcMessage::Notification(notif) => {
                    debug!(method = %notif.method, "ignoring incoming notification");
                    continue;
                }
            };

            let start = Instant::now();

            // Emit request received event
            self.event_emitter.emit(Event::RequestReceived {
                timestamp: Utc::now(),
                request_id: request.id.clone(),
                method: request.method.clone(),
            });
            metrics::record_request(&request.method);

            // === CRITICAL ORDERING ===
            // 1. Capture effective state BEFORE transition
            let effective_state = self.phase_engine.effective_state();

            // 2. Build event type and record it — may trigger transition via CAS
            let event = EventType::new(&request.method);
            let transition = self
                .phase_engine
                .record_event(&event, request.params.as_ref());

            // 3. Merge with any timer-triggered transition
            let transition = match transition {
                Some(t) => Some(t),
                None => self.phase_engine.recv_transition().await.unwrap_or(None),
            };

            // 4. Route to handler (uses PRE-transition effective state)
            let handler_result = handlers::handle_request(
                &request,
                &effective_state,
                server_name,
                server_version,
                &self.generator_limits,
            )
            .await;

            let response = match handler_result {
                Ok(Some(resp)) => Some(resp),
                Ok(None) => self.handle_unknown_method(&request),
                Err(e) => {
                    error!(method = %request.method, error = %e, "handler error");
                    Some(JsonRpcResponse::error(
                        request.id.clone(),
                        error_codes::INTERNAL_ERROR,
                        format!("internal error: {e}"),
                    ))
                }
            };

            // 5. Deliver response via resolved behavior
            if let Some(ref resp) = response {
                let resolved = self.behavior_coordinator.resolve(
                    &request,
                    &effective_state,
                    self.transport.transport_type(),
                );

                self.deliver_response(resp, &resolved, &request, start)
                    .await;

                // Execute OnRequest side effects after delivery
                self.execute_on_request_effects(&resolved).await;

                // If this was a successful initialize, run OnConnect effects
                if !*initialized && request.method == "initialize" && resp.error.is_none() {
                    *initialized = true;
                    self.execute_on_connect_effects(&resolved).await;
                }
            }

            // 6. THEN execute entry actions (response-before-transition guarantee)
            if let Some(ref trans) = transition {
                let from_name = self.phase_engine.current_phase_name().to_string();

                metrics::record_phase_transition(
                    &trans.from_phase.to_string(),
                    &trans.to_phase.to_string(),
                );
                metrics::set_current_phase(self.phase_engine.current_phase_name());

                self.execute_entry_actions(&trans.entry_actions).await;

                self.event_emitter.emit(Event::PhaseEntered {
                    timestamp: Utc::now(),
                    phase_name: from_name,
                    phase_index: trans.to_phase,
                });
            }

            metrics::record_request_duration(&request.method, start.elapsed());
        }

        Ok(())
    }

    /// Handles an unknown method per the config's `unknown_methods` setting.
    fn handle_unknown_method(&self, request: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        let handling = self.config.unknown_methods.unwrap_or_default();

        match handling {
            UnknownMethodHandling::Error => Some(JsonRpcResponse::error(
                request.id.clone(),
                error_codes::METHOD_NOT_FOUND,
                format!("method not found: {}", request.method),
            )),
            UnknownMethodHandling::Ignore => {
                Some(JsonRpcResponse::success(request.id.clone(), json!(null)))
            }
            UnknownMethodHandling::Drop => {
                debug!(method = %request.method, "dropping unknown method");
                None
            }
        }
    }

    /// Delivers a response via the resolved behavior's delivery mechanism.
    async fn deliver_response(
        &self,
        response: &JsonRpcResponse,
        resolved: &ResolvedBehavior,
        request: &JsonRpcRequest,
        start: Instant,
    ) {
        let message = JsonRpcMessage::Response(response.clone());
        let success = response.error.is_none();

        match resolved
            .delivery
            .deliver(&message, self.transport.as_ref())
            .await
        {
            Ok(result) => {
                record_delivery_metrics(resolved.delivery.name(), &result);
                metrics::record_response(&request.method, success);
                metrics::record_delivery_duration(result.duration);

                self.event_emitter.emit(Event::ResponseSent {
                    timestamp: Utc::now(),
                    request_id: request.id.clone(),
                    success,
                    #[allow(clippy::cast_possible_truncation)]
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => {
                error!(
                    method = %request.method,
                    error = %e,
                    "delivery failed"
                );
            }
        }
    }

    /// Executes entry actions from a phase transition.
    async fn execute_entry_actions(&self, actions: &[EntryAction]) {
        for action in actions {
            match action {
                EntryAction::SendNotification {
                    send_notification: method,
                } => {
                    let notification = JsonRpcMessage::Notification(JsonRpcNotification::new(
                        method.clone(),
                        None,
                    ));
                    if let Err(e) = self.transport.send_message(&notification).await {
                        warn!(method, error = %e, "failed to send entry notification");
                    }
                }
                EntryAction::SendRequest { send_request: cfg } => {
                    let id = cfg.id.clone().unwrap_or_else(|| json!(0));
                    let request = JsonRpcMessage::Request(JsonRpcRequest {
                        jsonrpc: JSONRPC_VERSION.to_string(),
                        method: cfg.method.clone(),
                        params: cfg.params.clone(),
                        id,
                    });
                    if let Err(e) = self.transport.send_message(&request).await {
                        warn!(method = %cfg.method, error = %e, "failed to send entry request");
                    }
                }
                EntryAction::Log { log: message } => {
                    info!(entry_action = "log", "{message}");
                }
            }
        }
    }

    /// Executes side effects that have `OnRequest` trigger.
    async fn execute_on_request_effects(&self, resolved: &ResolvedBehavior) {
        for effect in &resolved.side_effects {
            if effect.trigger() == SideEffectTrigger::OnRequest {
                let cancel = self.cancel.child_token();
                if let Err(e) = effect
                    .execute(self.transport.as_ref(), &self.connection, cancel)
                    .await
                {
                    warn!(effect = effect.name(), error = %e, "OnRequest side effect failed");
                }
            }
        }
    }

    /// Executes side effects that have `OnConnect` trigger.
    async fn execute_on_connect_effects(&self, resolved: &ResolvedBehavior) {
        for effect in &resolved.side_effects {
            if effect.trigger() == SideEffectTrigger::OnConnect {
                let cancel = self.cancel.child_token();
                if let Err(e) = effect
                    .execute(self.transport.as_ref(), &self.connection, cancel)
                    .await
                {
                    warn!(effect = effect.name(), error = %e, "OnConnect side effect failed");
                }
            }
        }
    }

    /// Spawns continuous side effects as background tasks.
    fn spawn_continuous_side_effects(&self) -> Vec<JoinHandle<()>> {
        // Resolve behavior for a synthetic request to get baseline side effects
        let effective_state = self.phase_engine.effective_state();
        let synthetic_request = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialize".to_string(),
            params: None,
            id: json!(0),
        };
        let resolved = self.behavior_coordinator.resolve(
            &synthetic_request,
            &effective_state,
            self.transport.transport_type(),
        );

        // We can't move the side effects out of resolved since they're boxed trait objects,
        // so we log which ones we'd spawn. In practice, continuous effects need the
        // transport reference to outlive the spawn, which requires Arc<dyn Transport>.
        // For now, log and skip — continuous effects will be wired with Arc transport later.
        let handles = Vec::new();
        for effect in &resolved.side_effects {
            if effect.trigger() == SideEffectTrigger::Continuous {
                info!(
                    effect = effect.name(),
                    "continuous side effect detected (background spawn deferred)"
                );
            }
        }
        handles
    }

    /// Returns the cancellation token for external shutdown.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Builds a `(BaselineState, Vec<Phase>)` from a `ServerConfig`.
///
/// If the config uses the phased form (baseline + phases), those are
/// used directly. Otherwise, constructs a baseline from the simple-server
/// top-level tools/resources/prompts.
fn build_baseline_and_phases(
    config: &ServerConfig,
) -> (BaselineState, Vec<crate::config::schema::Phase>) {
    config.baseline.as_ref().map_or_else(
        || {
            // Simple server: construct baseline from top-level definitions
            let baseline = BaselineState {
                tools: config.tools.clone().unwrap_or_default(),
                resources: config.resources.clone().unwrap_or_default(),
                prompts: config.prompts.clone().unwrap_or_default(),
                capabilities: config.server.capabilities.clone(),
                behavior: config.behavior.clone(),
            };
            (baseline, vec![])
        },
        |baseline| {
            let phases = config.phases.clone().unwrap_or_default();
            (baseline.clone(), phases)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ContentItem, ContentValue, DeliveryConfig, ResponseConfig, ServerMetadata, ToolDefinition,
        ToolPattern,
    };
    use crate::transport::TransportType;
    use std::io::Write;
    use std::sync::{Arc as StdArc, Mutex};

    // ========================================================================
    // Mock Transport
    // ========================================================================

    struct MockTransport {
        messages_to_receive: Mutex<Vec<JsonRpcMessage>>,
        sent_messages: StdArc<Mutex<Vec<Vec<u8>>>>,
    }

    impl MockTransport {
        fn new(incoming: Vec<JsonRpcMessage>) -> Self {
            Self {
                messages_to_receive: Mutex::new(incoming),
                sent_messages: StdArc::new(Mutex::new(Vec::new())),
            }
        }

        fn sent_bytes(&self) -> Vec<Vec<u8>> {
            self.sent_messages.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Transport for MockTransport {
        async fn send_message(&self, message: &JsonRpcMessage) -> crate::transport::Result<()> {
            let bytes = serde_json::to_vec(message)?;
            self.sent_messages.lock().unwrap().push(bytes);
            Ok(())
        }

        async fn send_raw(&self, bytes: &[u8]) -> crate::transport::Result<()> {
            self.sent_messages.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }

        async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
            let mut queue = self.messages_to_receive.lock().unwrap();
            if queue.is_empty() {
                Ok(None) // EOF
            } else {
                Ok(Some(queue.remove(0)))
            }
        }

        fn supports_behavior(&self, _behavior: &DeliveryConfig) -> bool {
            true
        }

        fn transport_type(&self) -> TransportType {
            TransportType::Stdio
        }
    }

    // ========================================================================
    // Test Writer for EventEmitter
    // ========================================================================

    #[derive(Clone)]
    struct TestWriter(StdArc<Mutex<Vec<u8>>>);

    impl TestWriter {
        fn new() -> Self {
            Self(StdArc::new(Mutex::new(Vec::new())))
        }

        fn contents(&self) -> String {
            let buf = self.0.lock().unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        }
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    fn simple_config() -> ServerConfig {
        ServerConfig {
            server: ServerMetadata {
                name: "test-server".to_string(),
                version: Some("1.0.0".to_string()),
                state_scope: None,
                capabilities: None,
            },
            baseline: None,
            tools: Some(vec![ToolPattern {
                tool: ToolDefinition {
                    name: "calc".to_string(),
                    description: "Calculator".to_string(),
                    input_schema: json!({"type": "object"}),
                },
                response: ResponseConfig {
                    content: vec![ContentItem::Text {
                        text: ContentValue::Static("42".to_string()),
                    }],
                    is_error: None,
                },
                behavior: None,
            }]),
            resources: None,
            prompts: None,
            phases: None,
            behavior: None,
            logging: None,
            unknown_methods: None,
        }
    }

    fn make_init_request() -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialize".to_string(),
            params: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0" }
            })),
            id: json!(0),
        })
    }

    fn make_tools_list_request() -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "tools/list".to_string(),
            params: None,
            id: json!(1),
        })
    }

    fn make_tool_call_request(name: &str) -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "tools/call".to_string(),
            params: Some(json!({"name": name})),
            id: json!(2),
        })
    }

    // ========================================================================
    // Tests
    // ========================================================================

    #[tokio::test]
    async fn initialize_handshake() {
        let config = Arc::new(simple_config());
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        let transport = Box::new(MockTransport::new(vec![make_init_request()]));

        let server = Server::new(config, transport, None, emitter);
        server.run().await.unwrap();

        let sent = server.transport.as_ref();
        // Can't access sent_messages via trait, so check event output
        let events = tw.contents();
        assert!(events.contains("ServerStarted"));
        assert!(events.contains("RequestReceived"));
        assert!(events.contains("ResponseSent"));
        assert!(events.contains("ServerStopped"));
    }

    #[tokio::test]
    async fn tools_list_returns_tools() {
        let config = Arc::new(simple_config());
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        let mock = MockTransport::new(vec![make_tools_list_request()]);
        let sent_ref = mock.sent_messages.clone();
        let transport = Box::new(mock);

        let server = Server::new(config, transport, None, emitter);
        server.run().await.unwrap();

        // Find the response in sent bytes
        let sent = sent_ref.lock().unwrap();
        let response_bytes = sent.iter().find(|b| {
            String::from_utf8_lossy(b).contains("tools")
                && String::from_utf8_lossy(b).contains("calc")
        });
        assert!(
            response_bytes.is_some(),
            "Expected tools/list response with 'calc'"
        );
    }

    #[tokio::test]
    async fn tool_call_returns_content() {
        let config = Arc::new(simple_config());
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        let mock = MockTransport::new(vec![make_tool_call_request("calc")]);
        let sent_ref = mock.sent_messages.clone();
        let transport = Box::new(mock);

        let server = Server::new(config, transport, None, emitter);
        server.run().await.unwrap();

        let sent = sent_ref.lock().unwrap();
        let response_bytes = sent
            .iter()
            .find(|b| String::from_utf8_lossy(b).contains("42"));
        assert!(
            response_bytes.is_some(),
            "Expected tool/call response with '42'"
        );
    }

    #[tokio::test]
    async fn unknown_method_error() {
        let mut cfg = simple_config();
        cfg.unknown_methods = Some(UnknownMethodHandling::Error);
        let config = Arc::new(cfg);
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        let mock = MockTransport::new(vec![JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "evil/method".to_string(),
            params: None,
            id: json!(99),
        })]);
        let sent_ref = mock.sent_messages.clone();
        let transport = Box::new(mock);

        let server = Server::new(config, transport, None, emitter);
        server.run().await.unwrap();

        let sent = sent_ref.lock().unwrap();
        let response_bytes = sent
            .iter()
            .find(|b| String::from_utf8_lossy(b).contains("-32601"));
        assert!(response_bytes.is_some(), "Expected METHOD_NOT_FOUND error");
    }

    #[tokio::test]
    async fn unknown_method_drop() {
        let mut cfg = simple_config();
        cfg.unknown_methods = Some(UnknownMethodHandling::Drop);
        let config = Arc::new(cfg);
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        let mock = MockTransport::new(vec![JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "evil/method".to_string(),
            params: None,
            id: json!(99),
        })]);
        let sent_ref = mock.sent_messages.clone();
        let transport = Box::new(mock);

        let server = Server::new(config, transport, None, emitter);
        server.run().await.unwrap();

        // No response should be sent for Drop mode
        let sent = sent_ref.lock().unwrap();
        assert!(
            sent.is_empty(),
            "Expected no response for Drop mode, got {} messages",
            sent.len()
        );
    }

    #[test]
    fn build_baseline_from_simple_config() {
        let config = simple_config();
        let (baseline, phases) = build_baseline_and_phases(&config);
        assert_eq!(baseline.tools.len(), 1);
        assert_eq!(baseline.tools[0].tool.name, "calc");
        assert!(phases.is_empty());
    }

    #[test]
    fn build_baseline_from_phased_config() {
        let config = ServerConfig {
            server: ServerMetadata {
                name: "phased".to_string(),
                version: None,
                state_scope: None,
                capabilities: None,
            },
            baseline: Some(BaselineState {
                tools: vec![ToolPattern {
                    tool: ToolDefinition {
                        name: "base-tool".to_string(),
                        description: "Baseline tool".to_string(),
                        input_schema: json!({"type": "object"}),
                    },
                    response: ResponseConfig {
                        content: vec![ContentItem::Text {
                            text: ContentValue::Static("ok".to_string()),
                        }],
                        is_error: None,
                    },
                    behavior: None,
                }],
                ..Default::default()
            }),
            tools: None,
            resources: None,
            prompts: None,
            phases: Some(vec![]),
            behavior: None,
            logging: None,
            unknown_methods: None,
        };

        let (baseline, phases) = build_baseline_and_phases(&config);
        assert_eq!(baseline.tools.len(), 1);
        assert_eq!(baseline.tools[0].tool.name, "base-tool");
        assert!(phases.is_empty());
    }
}
