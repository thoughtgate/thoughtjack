//! Behavior module (TJ-SPEC-004).
//!
//! Defines adversarial behaviors and payload generation strategies
//! for security testing scenarios. The [`BehaviorCoordinator`] resolves
//! the effective behavior for each request using a scoping chain:
//! CLI override > item-level > phase/baseline > default.

pub mod delivery;
pub mod side_effects;

pub use delivery::{DeliveryBehavior, DeliveryResult, create_delivery_behavior};
pub use side_effects::{SideEffect, SideEffectOutcome, SideEffectResult, create_side_effect};

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::schema::{BehaviorConfig, DeliveryConfig, SideEffectTrigger};
use crate::phase::EffectiveState;
use crate::transport::jsonrpc::JsonRpcRequest;
use crate::transport::{Transport, TransportType};

// ============================================================================
// ResolvedBehavior
// ============================================================================

/// A fully resolved behavior for a specific request.
///
/// Implements: TJ-SPEC-004 F-013
pub struct ResolvedBehavior {
    /// Delivery behavior controlling how bytes are transmitted.
    pub delivery: Box<dyn DeliveryBehavior>,
    /// Side effects to execute alongside the response.
    pub side_effects: Vec<Box<dyn SideEffect>>,
}

// ============================================================================
// BehaviorCoordinator
// ============================================================================

/// Coordinates behavior resolution using a scoping chain.
///
/// The scoping chain (highest to lowest priority):
/// 1. CLI override
/// 2. Item-level behavior (tool, resource, or prompt)
/// 3. Phase/baseline behavior
/// 4. Default (Normal delivery, no side effects)
///
/// Implements: TJ-SPEC-004 F-013
pub struct BehaviorCoordinator {
    cli_override: Option<BehaviorConfig>,
}

impl BehaviorCoordinator {
    /// Creates a new coordinator with an optional CLI override.
    ///
    /// Implements: TJ-SPEC-004 F-013
    #[must_use]
    pub const fn new(cli_override: Option<BehaviorConfig>) -> Self {
        Self { cli_override }
    }

    /// Resolves the effective behavior config for a request.
    ///
    /// Applies the scoping chain: CLI > item-level > phase/baseline > default.
    ///
    /// Implements: TJ-SPEC-004 F-013
    #[must_use]
    pub fn resolve_config(
        &self,
        request: &JsonRpcRequest,
        effective_state: &EffectiveState,
    ) -> BehaviorConfig {
        // 1. CLI override wins
        if let Some(ref cli) = self.cli_override {
            return cli.clone();
        }

        // 2. Item-level: look up by method + params.name/uri
        if let Some(item_behavior) = Self::lookup_item_behavior(request, effective_state) {
            return item_behavior;
        }

        // 3. Phase/baseline behavior
        if let Some(ref behavior) = effective_state.behavior {
            return behavior.clone();
        }

        // 4. Default
        BehaviorConfig {
            delivery: Some(DeliveryConfig::Normal),
            side_effects: None,
        }
    }

    /// Resolves a request into a fully constructed [`ResolvedBehavior`].
    ///
    /// Implements: TJ-SPEC-004 F-013
    #[must_use]
    pub fn resolve(
        &self,
        request: &JsonRpcRequest,
        effective_state: &EffectiveState,
        _transport_type: TransportType,
    ) -> ResolvedBehavior {
        let config = self.resolve_config(request, effective_state);

        let delivery = config.delivery.as_ref().map_or_else(
            || create_delivery_behavior(&DeliveryConfig::Normal),
            create_delivery_behavior,
        );

        let side_effects = config
            .side_effects
            .as_ref()
            .map(|effects| effects.iter().map(create_side_effect).collect())
            .unwrap_or_default();

        ResolvedBehavior {
            delivery,
            side_effects,
        }
    }

    /// Looks up item-level behavior from the effective state.
    fn lookup_item_behavior(
        request: &JsonRpcRequest,
        effective_state: &EffectiveState,
    ) -> Option<BehaviorConfig> {
        let params = request.params.as_ref()?;

        match request.method.as_str() {
            "tools/call" => {
                let name = params.get("name")?.as_str()?;
                effective_state
                    .tools
                    .get(name)
                    .and_then(|t| t.behavior.clone())
            }
            "resources/read" | "resources/subscribe" => {
                let uri = params.get("uri")?.as_str()?;
                effective_state
                    .resources
                    .get(uri)
                    .and_then(|r| r.behavior.clone())
            }
            "prompts/get" => {
                let name = params.get("name")?.as_str()?;
                effective_state
                    .prompts
                    .get(name)
                    .and_then(|p| p.behavior.clone())
            }
            _ => None,
        }
    }
}

// ============================================================================
// SideEffectManager
// ============================================================================

/// Manages the lifecycle of side effects for a server session.
///
/// Spawns, tracks, and shuts down side effects according to their
/// [`SideEffectTrigger`] type:
///
/// - `OnConnect` / `Continuous` effects start via [`on_connect`](Self::on_connect)
/// - `OnRequest` / `OnSubscribe` / `OnUnsubscribe` effects fire via
///   [`trigger`](Self::trigger)
/// - All running effects are cancelled and joined via [`shutdown`](Self::shutdown)
///
/// Implements: TJ-SPEC-004 F-014
pub struct SideEffectManager {
    transport: Arc<dyn Transport>,
    cancel: CancellationToken,
    running: Vec<JoinHandle<()>>,
}

impl SideEffectManager {
    /// Creates a new manager.
    ///
    /// Implements: TJ-SPEC-004 F-014
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>, cancel: CancellationToken) -> Self {
        Self {
            transport,
            cancel,
            running: Vec::new(),
        }
    }

    /// Fires all effects matching the given trigger.
    ///
    /// Returns a list of results for successfully completed effects.
    /// Transport-incompatible effects are skipped with a warning.
    ///
    /// Implements: TJ-SPEC-004 F-014
    pub async fn trigger(
        &self,
        effects: &[Box<dyn SideEffect>],
        trigger: SideEffectTrigger,
    ) -> Vec<(String, SideEffectResult)> {
        let mut results = Vec::new();
        let transport_type = self.transport.transport_type();

        for effect in effects {
            if effect.trigger() != trigger {
                continue;
            }
            if !effect.supports_transport(transport_type) {
                tracing::warn!(
                    effect = effect.name(),
                    transport = ?transport_type,
                    "side effect not supported on this transport, skipping"
                );
                continue;
            }

            let child_cancel = self.cancel.child_token();
            match effect
                .execute(
                    self.transport.as_ref(),
                    &self.transport.connection_context(),
                    child_cancel,
                )
                .await
            {
                Ok(result) => {
                    record_side_effect_metrics(effect.name(), &result);
                    results.push((effect.name().to_string(), result));
                }
                Err(e) => {
                    tracing::warn!(
                        effect = effect.name(),
                        error = %e,
                        ?trigger,
                        "side effect failed"
                    );
                }
            }
        }

        results
    }

    /// Spawns a single owned side effect as a background task.
    ///
    /// Use this for continuous effects where ownership can be transferred.
    ///
    /// Implements: TJ-SPEC-004 F-014
    pub fn spawn(&mut self, effect: Box<dyn SideEffect>) {
        let transport = Arc::clone(&self.transport);
        let ctx = transport.connection_context();
        let cancel = self.cancel.child_token();
        let name = effect.name().to_string();

        tracing::info!(effect = %name, "spawning background side effect");

        self.running.push(tokio::spawn(async move {
            match effect.execute(transport.as_ref(), &ctx, cancel).await {
                Ok(result) => {
                    record_side_effect_metrics(&name, &result);
                }
                Err(e) => {
                    tracing::warn!(
                        effect = %name,
                        error = %e,
                        "background side effect failed"
                    );
                }
            }
        }));
    }

    /// Cancels all running background effects and waits for them to finish.
    ///
    /// Each task is given a 2-second grace period before being considered
    /// timed out.
    ///
    /// Implements: TJ-SPEC-004 F-014
    pub async fn shutdown(&mut self) {
        self.cancel.cancel();
        for handle in self.running.drain(..) {
            match tokio::time::timeout(Duration::from_secs(2), handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) if e.is_cancelled() => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "background side effect task panicked"),
                Err(_) => {
                    tracing::warn!("background side effect task did not finish within 2s");
                }
            }
        }
    }

    /// Returns the number of currently running background tasks.
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.running.len()
    }
}

// ============================================================================
// Metric helpers
// ============================================================================

/// Records metrics for a delivery operation.
///
/// Implements: TJ-SPEC-004 F-015
#[allow(clippy::cast_precision_loss)]
pub fn record_delivery_metrics(name: &str, result: &DeliveryResult) {
    metrics::counter!(
        "thoughtjack_delivery_bytes_total",
        "behavior" => name.to_string()
    )
    .increment(result.bytes_sent as u64);
    metrics::histogram!(
        "thoughtjack_delivery_duration_ms",
        "behavior" => name.to_string()
    )
    .record(result.duration.as_secs_f64() * 1000.0);
}

/// Records metrics for a side effect operation.
///
/// Implements: TJ-SPEC-004 F-015
#[allow(clippy::cast_precision_loss)]
pub fn record_side_effect_metrics(name: &str, result: &SideEffectResult) {
    metrics::counter!(
        "thoughtjack_side_effects_total",
        "effect" => name.to_string()
    )
    .increment(1);
    metrics::histogram!(
        "thoughtjack_side_effect_messages",
        "effect" => name.to_string()
    )
    .record(result.messages_sent as f64);
    metrics::histogram!(
        "thoughtjack_side_effect_bytes",
        "effect" => name.to_string()
    )
    .record(result.bytes_sent as f64);
    metrics::histogram!(
        "thoughtjack_side_effect_duration_ms",
        "effect" => name.to_string()
    )
    .record(result.duration.as_secs_f64() * 1000.0);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        BehaviorConfig, ContentItem, ContentValue, DeliveryConfig, PromptDefinition, PromptPattern,
        PromptResponse, ResourceDefinition, ResourcePattern, ResourceResponse, ResponseConfig,
        ToolDefinition, ToolPattern,
    };
    use crate::phase::EffectiveState;
    use crate::transport::jsonrpc::JSONRPC_VERSION;
    use indexmap::IndexMap;
    use serde_json::json;

    fn make_request(method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
            id: json!(1),
        }
    }

    fn make_tool_with_behavior(name: &str, behavior: BehaviorConfig) -> ToolPattern {
        ToolPattern {
            tool: ToolDefinition {
                name: name.to_string(),
                description: "test".to_string(),
                input_schema: json!({"type": "object"}),
            },
            response: ResponseConfig {
                content: vec![ContentItem::Text {
                    text: ContentValue::Static("ok".to_string()),
                }],
                is_error: None,
                ..Default::default()
            },
            behavior: Some(behavior),
        }
    }

    fn make_resource_with_behavior(uri: &str, behavior: BehaviorConfig) -> ResourcePattern {
        ResourcePattern {
            resource: ResourceDefinition {
                uri: uri.to_string(),
                name: "test".to_string(),
                description: None,
                mime_type: None,
            },
            response: Some(ResourceResponse {
                content: ContentValue::Static("content".to_string()),
                ..Default::default()
            }),
            behavior: Some(behavior),
        }
    }

    fn make_prompt_with_behavior(name: &str, behavior: BehaviorConfig) -> PromptPattern {
        PromptPattern {
            prompt: PromptDefinition {
                name: name.to_string(),
                description: None,
                arguments: None,
            },
            response: PromptResponse::default(),
            behavior: Some(behavior),
        }
    }

    fn slow_loris_config() -> BehaviorConfig {
        BehaviorConfig {
            delivery: Some(DeliveryConfig::SlowLoris {
                byte_delay_ms: Some(50),
                chunk_size: Some(1),
            }),
            side_effects: None,
        }
    }

    fn response_delay_config() -> BehaviorConfig {
        BehaviorConfig {
            delivery: Some(DeliveryConfig::ResponseDelay { delay_ms: 200 }),
            side_effects: None,
        }
    }

    fn make_state(
        tools: IndexMap<String, ToolPattern>,
        resources: IndexMap<String, ResourcePattern>,
        prompts: IndexMap<String, PromptPattern>,
        behavior: Option<BehaviorConfig>,
    ) -> EffectiveState {
        EffectiveState {
            tools,
            resources,
            prompts,
            capabilities: None,
            behavior,
        }
    }

    // ========================================================================
    // Scoping: tool-level overrides phase-level
    // ========================================================================

    #[test]
    fn test_tool_level_overrides_phase_level() {
        let mut tools = IndexMap::new();
        tools.insert(
            "calc".to_string(),
            make_tool_with_behavior("calc", response_delay_config()),
        );
        let state = make_state(
            tools,
            IndexMap::new(),
            IndexMap::new(),
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("tools/call", Some(json!({"name": "calc"})));
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::ResponseDelay { delay_ms: 200 })
        ));
    }

    // ========================================================================
    // Scoping: non-tool request uses phase behavior
    // ========================================================================

    #[test]
    fn test_non_tool_request_uses_phase_behavior() {
        // EC-BEH-015: Non-tool request uses phase behavior
        let mut tools = IndexMap::new();
        tools.insert(
            "calc".to_string(),
            make_tool_with_behavior("calc", response_delay_config()),
        );
        let state = make_state(
            tools,
            IndexMap::new(),
            IndexMap::new(),
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("tools/list", None);
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::SlowLoris { .. })
        ));
    }

    // ========================================================================
    // Scoping: unknown tool falls through to phase
    // ========================================================================

    #[test]
    fn test_unknown_tool_falls_through() {
        // EC-BEH-022: Unknown tool uses phase behavior
        let mut tools = IndexMap::new();
        tools.insert(
            "calc".to_string(),
            make_tool_with_behavior("calc", response_delay_config()),
        );
        let state = make_state(
            tools,
            IndexMap::new(),
            IndexMap::new(),
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("tools/call", Some(json!({"name": "unknown_tool"})));
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::SlowLoris { .. })
        ));
    }

    // ========================================================================
    // Default behavior
    // ========================================================================

    #[test]
    fn test_default_behavior() {
        let state = make_state(IndexMap::new(), IndexMap::new(), IndexMap::new(), None);
        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("tools/list", None);
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(config.delivery, Some(DeliveryConfig::Normal)));
        assert!(config.side_effects.is_none());
    }

    // ========================================================================
    // CLI override wins
    // ========================================================================

    #[test]
    fn test_cli_override_wins() {
        let cli_config = BehaviorConfig {
            delivery: Some(DeliveryConfig::NestedJson {
                depth: 100,
                key: None,
            }),
            side_effects: None,
        };

        let mut tools = IndexMap::new();
        tools.insert(
            "calc".to_string(),
            make_tool_with_behavior("calc", response_delay_config()),
        );
        let state = make_state(
            tools,
            IndexMap::new(),
            IndexMap::new(),
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(Some(cli_config));
        let request = make_request("tools/call", Some(json!({"name": "calc"})));
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::NestedJson { depth: 100, .. })
        ));
    }

    // ========================================================================
    // Resource lookup
    // ========================================================================

    #[test]
    fn test_resource_level_behavior() {
        let mut resources = IndexMap::new();
        resources.insert(
            "file:///secret".to_string(),
            make_resource_with_behavior("file:///secret", response_delay_config()),
        );
        let state = make_state(
            IndexMap::new(),
            resources,
            IndexMap::new(),
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("resources/read", Some(json!({"uri": "file:///secret"})));
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::ResponseDelay { delay_ms: 200 })
        ));
    }

    // ========================================================================
    // Prompt lookup
    // ========================================================================

    #[test]
    fn test_prompt_level_behavior() {
        let mut prompts = IndexMap::new();
        prompts.insert(
            "inject".to_string(),
            make_prompt_with_behavior("inject", response_delay_config()),
        );
        let state = make_state(
            IndexMap::new(),
            IndexMap::new(),
            prompts,
            Some(slow_loris_config()),
        );

        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("prompts/get", Some(json!({"name": "inject"})));
        let config = coordinator.resolve_config(&request, &state);

        assert!(matches!(
            config.delivery,
            Some(DeliveryConfig::ResponseDelay { delay_ms: 200 })
        ));
    }

    // ========================================================================
    // resolve() produces ResolvedBehavior
    // ========================================================================

    #[test]
    fn test_resolve_produces_delivery() {
        let state = make_state(IndexMap::new(), IndexMap::new(), IndexMap::new(), None);
        let coordinator = BehaviorCoordinator::new(None);
        let request = make_request("tools/list", None);
        let resolved = coordinator.resolve(&request, &state, TransportType::Stdio);

        assert_eq!(resolved.delivery.name(), "normal");
        assert!(resolved.side_effects.is_empty());
    }

    // ========================================================================
    // SideEffectManager tests
    // ========================================================================

    mod manager_tests {
        use super::*;
        use crate::config::schema::{SideEffectConfig, SideEffectType};
        use crate::transport::jsonrpc::JsonRpcMessage;
        use std::collections::HashMap;
        use std::sync::Mutex;

        struct MockTransport {
            raw_sends: Arc<Mutex<Vec<Vec<u8>>>>,
        }

        impl MockTransport {
            fn new() -> Self {
                Self {
                    raw_sends: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        #[async_trait::async_trait]
        impl crate::transport::Transport for MockTransport {
            async fn send_message(
                &self,
                _message: &JsonRpcMessage,
            ) -> crate::transport::Result<()> {
                Ok(())
            }

            async fn send_raw(&self, bytes: &[u8]) -> crate::transport::Result<()> {
                self.raw_sends.lock().unwrap().push(bytes.to_vec());
                Ok(())
            }

            async fn receive_message(&self) -> crate::transport::Result<Option<JsonRpcMessage>> {
                Ok(None)
            }

            fn supports_behavior(&self, _behavior: &DeliveryConfig) -> bool {
                true
            }

            fn transport_type(&self) -> TransportType {
                TransportType::Stdio
            }

            async fn finalize_response(&self) -> crate::transport::Result<()> {
                Ok(())
            }

            fn connection_context(&self) -> crate::transport::ConnectionContext {
                crate::transport::ConnectionContext::stdio()
            }
        }

        #[tokio::test]
        async fn manager_trigger_returns_results() {
            let transport = Arc::new(MockTransport::new());
            let cancel = CancellationToken::new();
            let mgr = SideEffectManager::new(transport, cancel);

            let effects: Vec<Box<dyn SideEffect>> = vec![create_side_effect(&SideEffectConfig {
                type_: SideEffectType::CloseConnection,
                trigger: SideEffectTrigger::OnRequest,
                params: HashMap::new(),
            })];

            let results = mgr.trigger(&effects, SideEffectTrigger::OnRequest).await;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, "close_connection");
        }

        #[tokio::test]
        async fn manager_trigger_filters_by_trigger() {
            let transport = Arc::new(MockTransport::new());
            let cancel = CancellationToken::new();
            let mgr = SideEffectManager::new(transport, cancel);

            let effects: Vec<Box<dyn SideEffect>> = vec![create_side_effect(&SideEffectConfig {
                type_: SideEffectType::CloseConnection,
                trigger: SideEffectTrigger::OnRequest,
                params: HashMap::new(),
            })];

            // Trigger with OnConnect should return no results
            let results = mgr.trigger(&effects, SideEffectTrigger::OnConnect).await;
            assert!(results.is_empty());
        }

        #[tokio::test]
        async fn manager_spawn_and_shutdown() {
            let transport = Arc::new(MockTransport::new());
            let cancel = CancellationToken::new();
            let mut mgr = SideEffectManager::new(transport, cancel);

            let effect = create_side_effect(&SideEffectConfig {
                type_: SideEffectType::NotificationFlood,
                trigger: SideEffectTrigger::Continuous,
                params: {
                    let mut m = HashMap::new();
                    m.insert("rate_per_sec".to_string(), json!(100));
                    m.insert("duration_sec".to_string(), json!(60));
                    m
                },
            });

            mgr.spawn(effect);
            assert_eq!(mgr.running_count(), 1);

            // Shutdown should cancel and join
            mgr.shutdown().await;
            assert_eq!(mgr.running_count(), 0);
        }
    }

    // ========================================================================
    // Metric helper tests
    // ========================================================================

    #[test]
    fn test_record_delivery_metrics_zero_bytes() {
        // Recording metrics with 0 bytes_sent should not panic
        let result = DeliveryResult {
            bytes_sent: 0,
            duration: Duration::ZERO,
            completed: true,
        };
        // Should succeed without panic
        record_delivery_metrics("test_delivery", &result);
    }

    #[test]
    fn test_record_side_effect_metrics_basic() {
        // Recording metrics with realistic values should not panic
        let result = SideEffectResult {
            messages_sent: 42,
            bytes_sent: 1024,
            duration: Duration::from_millis(500),
            completed: true,
            outcome: SideEffectOutcome::Completed,
        };
        // Should succeed without panic
        record_side_effect_metrics("test_effect", &result);
    }
}
