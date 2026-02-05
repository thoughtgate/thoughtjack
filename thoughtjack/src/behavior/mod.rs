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

use crate::config::schema::{BehaviorConfig, DeliveryConfig};
use crate::phase::EffectiveState;
use crate::transport::TransportType;
use crate::transport::jsonrpc::JsonRpcRequest;

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
        "behavior.side_effect.messages",
        "effect" => name.to_string()
    )
    .record(result.messages_sent as f64);
    metrics::histogram!(
        "behavior.side_effect.bytes",
        "effect" => name.to_string()
    )
    .record(result.bytes_sent as f64);
    metrics::histogram!(
        "behavior.side_effect.duration_ms",
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
            response: PromptResponse { messages: vec![] },
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
}
