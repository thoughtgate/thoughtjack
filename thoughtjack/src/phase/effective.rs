//! Effective state computation (TJ-SPEC-003 F-002)
//!
//! Computes the server's effective state by applying the current phase's
//! diff operations to the baseline. Each phase is independent — diffs
//! are NOT cumulative across phases.

use indexmap::IndexMap;

use crate::config::schema::{
    BaselineState, BehaviorConfig, Capabilities, Phase, PromptPattern, PromptPatternRef,
    ResourcePattern, ResourcePatternRef, ToolPattern, ToolPatternRef,
};

/// Computed server state after applying phase diffs to baseline.
///
/// Represents the full set of tools, resources, prompts, capabilities,
/// and behavior that the server should expose in its current phase.
#[derive(Debug, Clone)]
pub struct EffectiveState {
    /// Active tool patterns, keyed by tool name
    pub tools: IndexMap<String, ToolPattern>,
    /// Active resource patterns, keyed by resource URI
    pub resources: IndexMap<String, ResourcePattern>,
    /// Active prompt patterns, keyed by prompt name
    pub prompts: IndexMap<String, PromptPattern>,
    /// Server capabilities to advertise
    pub capabilities: Option<Capabilities>,
    /// Active behavior configuration
    pub behavior: Option<BehaviorConfig>,
}

impl EffectiveState {
    /// Computes effective state from baseline + current phase diff.
    ///
    /// Applies diff operations in order: remove -> replace -> add (F-002).
    /// Capabilities are merged (not replaced) if present.
    /// Behavior is fully replaced if present.
    #[must_use]
    pub fn compute(baseline: &BaselineState, current_phase: Option<&Phase>) -> Self {
        let mut tools: IndexMap<String, ToolPattern> = baseline
            .tools
            .iter()
            .map(|t| (t.tool.name.clone(), t.clone()))
            .collect();

        let mut resources: IndexMap<String, ResourcePattern> = baseline
            .resources
            .iter()
            .map(|r| (r.resource.uri.clone(), r.clone()))
            .collect();

        let mut prompts: IndexMap<String, PromptPattern> = baseline
            .prompts
            .iter()
            .map(|p| (p.prompt.name.clone(), p.clone()))
            .collect();

        let mut capabilities = baseline.capabilities.clone();
        let mut behavior = baseline.behavior.clone();

        if let Some(phase) = current_phase {
            // Apply tool diffs: remove -> replace -> add
            if let Some(remove_tools) = &phase.remove_tools {
                for name in remove_tools {
                    tools.shift_remove(name);
                }
            }
            if let Some(replace_tools) = &phase.replace_tools {
                for (name, tool_ref) in replace_tools {
                    if let Some(pattern) = resolve_tool_ref(tool_ref) {
                        tools.insert(name.clone(), pattern);
                    }
                }
            }
            if let Some(add_tools) = &phase.add_tools {
                for tool_ref in add_tools {
                    if let Some(pattern) = resolve_tool_ref(tool_ref) {
                        tools.insert(pattern.tool.name.clone(), pattern);
                    }
                }
            }

            // Apply resource diffs: remove -> replace -> add
            if let Some(remove_resources) = &phase.remove_resources {
                for uri in remove_resources {
                    resources.shift_remove(uri);
                }
            }
            if let Some(replace_resources) = &phase.replace_resources {
                for (uri, resource_ref) in replace_resources {
                    if let Some(pattern) = resolve_resource_ref(resource_ref) {
                        resources.insert(uri.clone(), pattern);
                    }
                }
            }
            if let Some(add_resources) = &phase.add_resources {
                for resource_ref in add_resources {
                    if let Some(pattern) = resolve_resource_ref(resource_ref) {
                        resources.insert(pattern.resource.uri.clone(), pattern);
                    }
                }
            }

            // Apply prompt diffs: remove -> replace -> add
            if let Some(remove_prompts) = &phase.remove_prompts {
                for name in remove_prompts {
                    prompts.shift_remove(name);
                }
            }
            if let Some(replace_prompts) = &phase.replace_prompts {
                for (name, prompt_ref) in replace_prompts {
                    if let Some(pattern) = resolve_prompt_ref(prompt_ref) {
                        prompts.insert(name.clone(), pattern);
                    }
                }
            }
            if let Some(add_prompts) = &phase.add_prompts {
                for prompt_ref in add_prompts {
                    if let Some(pattern) = resolve_prompt_ref(prompt_ref) {
                        prompts.insert(pattern.prompt.name.clone(), pattern);
                    }
                }
            }

            // Merge capabilities if specified (EC-PHASE-017)
            if let Some(phase_caps) = &phase.replace_capabilities {
                capabilities = Some(merge_capabilities(capabilities.as_ref(), phase_caps));
            }

            // Replace behavior if specified
            if phase.behavior.is_some() {
                behavior.clone_from(&phase.behavior);
            }
        }

        Self {
            tools,
            resources,
            prompts,
            capabilities,
            behavior,
        }
    }
}

/// Resolves a `ToolPatternRef` to a `ToolPattern`.
///
/// For inline patterns, returns a clone. For file paths, returns `None`
/// (file resolution is handled by the config loader).
fn resolve_tool_ref(tool_ref: &ToolPatternRef) -> Option<ToolPattern> {
    match tool_ref {
        ToolPatternRef::Inline(pattern) => Some(pattern.clone()),
        ToolPatternRef::Path(_) => None, // File paths resolved at config load time
    }
}

/// Resolves a `ResourcePatternRef` to a `ResourcePattern`.
fn resolve_resource_ref(resource_ref: &ResourcePatternRef) -> Option<ResourcePattern> {
    match resource_ref {
        ResourcePatternRef::Inline(pattern) => Some(pattern.clone()),
        ResourcePatternRef::Path(_) => None,
    }
}

/// Resolves a `PromptPatternRef` to a `PromptPattern`.
fn resolve_prompt_ref(prompt_ref: &PromptPatternRef) -> Option<PromptPattern> {
    match prompt_ref {
        PromptPatternRef::Inline(pattern) => Some(pattern.clone()),
        PromptPatternRef::Path(_) => None,
    }
}

/// Merges baseline capabilities with phase capability overrides (EC-PHASE-017).
///
/// Phase capabilities are merged on top of baseline — fields present in the
/// phase override take precedence, but fields only in the baseline are preserved.
fn merge_capabilities(baseline: Option<&Capabilities>, phase: &Capabilities) -> Capabilities {
    let baseline = baseline.cloned().unwrap_or_default();

    Capabilities {
        tools: phase.tools.clone().or(baseline.tools),
        resources: phase.resources.clone().or(baseline.resources),
        prompts: phase.prompts.clone().or(baseline.prompts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ContentItem, ContentValue, DeliveryConfig, PromptDefinition, PromptResponse,
        ResourceDefinition, ResourceResponse, ResourcesCapability, ResponseConfig, ToolDefinition,
        ToolsCapability,
    };

    fn make_tool(name: &str, description: &str) -> ToolPattern {
        ToolPattern {
            tool: ToolDefinition {
                name: name.to_string(),
                description: description.to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            response: ResponseConfig {
                content: vec![ContentItem::Text {
                    text: ContentValue::Static("result".to_string()),
                }],
                is_error: None,
            },
            behavior: None,
        }
    }

    fn make_resource(uri: &str, name: &str) -> ResourcePattern {
        ResourcePattern {
            resource: ResourceDefinition {
                uri: uri.to_string(),
                name: name.to_string(),
                description: None,
                mime_type: None,
            },
            response: Some(ResourceResponse {
                content: ContentValue::Static("content".to_string()),
            }),
            behavior: None,
        }
    }

    fn make_prompt(name: &str) -> PromptPattern {
        PromptPattern {
            prompt: PromptDefinition {
                name: name.to_string(),
                description: None,
                arguments: None,
            },
            response: PromptResponse { messages: vec![] },
            behavior: None,
        }
    }

    #[test]
    fn test_baseline_only() {
        let baseline = BaselineState {
            tools: vec![make_tool("calc", "Calculator")],
            resources: vec![make_resource("file:///test", "Test")],
            prompts: vec![make_prompt("review")],
            capabilities: None,
            behavior: None,
        };

        let state = EffectiveState::compute(&baseline, None);
        assert_eq!(state.tools.len(), 1);
        assert!(state.tools.contains_key("calc"));
        assert_eq!(state.resources.len(), 1);
        assert!(state.resources.contains_key("file:///test"));
        assert_eq!(state.prompts.len(), 1);
        assert!(state.prompts.contains_key("review"));
    }

    #[test]
    fn test_remove_tools() {
        let baseline = BaselineState {
            tools: vec![make_tool("calc", "Calculator"), make_tool("echo", "Echo")],
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            remove_tools: Some(vec!["calc".to_string()]),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.tools.len(), 1);
        assert!(!state.tools.contains_key("calc"));
        assert!(state.tools.contains_key("echo"));
    }

    #[test]
    fn test_replace_tools() {
        let baseline = BaselineState {
            tools: vec![make_tool("calc", "Benign calculator")],
            ..Default::default()
        };

        let mut replace_tools = IndexMap::new();
        replace_tools.insert(
            "calc".to_string(),
            ToolPatternRef::Inline(make_tool("calc", "Malicious calculator")),
        );

        let phase = Phase {
            name: "attack".to_string(),
            replace_tools: Some(replace_tools),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.tools.len(), 1);
        assert_eq!(state.tools["calc"].tool.description, "Malicious calculator");
    }

    #[test]
    fn test_add_tools() {
        let baseline = BaselineState {
            tools: vec![make_tool("calc", "Calculator")],
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            add_tools: Some(vec![ToolPatternRef::Inline(make_tool(
                "exploit",
                "Exploit tool",
            ))]),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.tools.len(), 2);
        assert!(state.tools.contains_key("calc"));
        assert!(state.tools.contains_key("exploit"));
    }

    #[test]
    fn test_diff_order_remove_replace_add() {
        let baseline = BaselineState {
            tools: vec![
                make_tool("old", "Old tool"),
                make_tool("swap", "Original swap"),
            ],
            ..Default::default()
        };

        let mut replace_tools = IndexMap::new();
        replace_tools.insert(
            "swap".to_string(),
            ToolPatternRef::Inline(make_tool("swap", "Replaced swap")),
        );

        let phase = Phase {
            name: "attack".to_string(),
            remove_tools: Some(vec!["old".to_string()]),
            replace_tools: Some(replace_tools),
            add_tools: Some(vec![ToolPatternRef::Inline(make_tool("new", "New tool"))]),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.tools.len(), 2);
        assert!(!state.tools.contains_key("old"));
        assert_eq!(state.tools["swap"].tool.description, "Replaced swap");
        assert!(state.tools.contains_key("new"));
    }

    #[test]
    fn test_behavior_override() {
        let baseline = BaselineState {
            behavior: Some(BehaviorConfig {
                delivery: Some(DeliveryConfig::Normal),
                side_effects: None,
            }),
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            behavior: Some(BehaviorConfig {
                delivery: Some(DeliveryConfig::SlowLoris {
                    byte_delay_ms: Some(100),
                    chunk_size: Some(1),
                }),
                side_effects: None,
            }),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        let behavior = state.behavior.unwrap();
        assert!(matches!(
            behavior.delivery,
            Some(DeliveryConfig::SlowLoris { .. })
        ));
    }

    #[test]
    fn test_capabilities_merge() {
        let baseline = BaselineState {
            capabilities: Some(Capabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                resources: None,
                prompts: None,
            }),
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            replace_capabilities: Some(Capabilities {
                tools: None,
                resources: Some(ResourcesCapability {
                    subscribe: Some(true),
                    list_changed: None,
                }),
                prompts: None,
            }),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        let caps = state.capabilities.unwrap();
        // Tools from baseline preserved
        assert_eq!(caps.tools.unwrap().list_changed, Some(true));
        // Resources from phase added
        assert_eq!(caps.resources.unwrap().subscribe, Some(true));
    }

    #[test]
    fn test_capabilities_override() {
        let baseline = BaselineState {
            capabilities: Some(Capabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                resources: None,
                prompts: None,
            }),
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            replace_capabilities: Some(Capabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                resources: None,
                prompts: None,
            }),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        let caps = state.capabilities.unwrap();
        assert_eq!(caps.tools.unwrap().list_changed, Some(false));
    }

    #[test]
    fn test_no_phase_diff_preserves_baseline() {
        let baseline = BaselineState {
            tools: vec![make_tool("calc", "Calculator")],
            behavior: Some(BehaviorConfig {
                delivery: Some(DeliveryConfig::Normal),
                side_effects: None,
            }),
            ..Default::default()
        };

        // Phase with no diffs
        let phase = Phase {
            name: "noop".to_string(),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.tools.len(), 1);
        assert_eq!(state.tools["calc"].tool.description, "Calculator");
        assert!(matches!(
            state.behavior.unwrap().delivery,
            Some(DeliveryConfig::Normal)
        ));
    }

    #[test]
    fn test_remove_resource() {
        let baseline = BaselineState {
            resources: vec![
                make_resource("file:///a", "A"),
                make_resource("file:///b", "B"),
            ],
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            remove_resources: Some(vec!["file:///a".to_string()]),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.resources.len(), 1);
        assert!(state.resources.contains_key("file:///b"));
    }

    #[test]
    fn test_remove_prompt() {
        let baseline = BaselineState {
            prompts: vec![make_prompt("review"), make_prompt("summarize")],
            ..Default::default()
        };

        let phase = Phase {
            name: "attack".to_string(),
            remove_prompts: Some(vec!["review".to_string()]),
            ..default_phase()
        };

        let state = EffectiveState::compute(&baseline, Some(&phase));
        assert_eq!(state.prompts.len(), 1);
        assert!(state.prompts.contains_key("summarize"));
    }

    /// Helper to create a default phase with required fields.
    fn default_phase() -> Phase {
        Phase {
            name: String::new(),
            advance: None,
            on_enter: None,
            replace_tools: None,
            add_tools: None,
            remove_tools: None,
            replace_resources: None,
            add_resources: None,
            remove_resources: None,
            replace_prompts: None,
            add_prompts: None,
            remove_prompts: None,
            replace_capabilities: None,
            behavior: None,
        }
    }
}
