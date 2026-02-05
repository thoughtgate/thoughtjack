//! Configuration validation (TJ-SPEC-006)
//!
//! This module implements schema and semantic validation for `ThoughtJack`
//! configurations. Validation is performed after all directives have been
//! resolved, on the fully deserialized `ServerConfig`.
//!
//! Validation collects ALL errors (doesn't stop at first) to provide
//! comprehensive feedback to users.

use crate::config::loader::ConfigLimits;
use crate::config::schema::{
    BaselineState, Phase, PromptPattern, ResourcePattern, ServerConfig, ToolPattern,
};
use crate::error::{Severity, ValidationIssue};

use std::collections::HashSet;

// ============================================================================
// Public API
// ============================================================================

/// Result of configuration validation.
///
/// Implements: TJ-SPEC-006 F-006
#[derive(Debug, Default)]
pub struct ValidationResult {
    /// Validation errors (prevent loading).
    pub errors: Vec<ValidationIssue>,

    /// Validation warnings (informational).
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationResult {
    /// Returns `true` if there are any errors.
    ///
    /// Implements: TJ-SPEC-006 F-006
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Returns `true` if validation passed (no errors).
    ///
    /// Implements: TJ-SPEC-006 F-006
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Configuration validator.
///
/// Performs schema validation and semantic validation on a `ServerConfig`.
///
/// Implements: TJ-SPEC-006 F-006, F-007
#[derive(Debug, Default)]
pub struct Validator {
    errors: Vec<ValidationIssue>,
    warnings: Vec<ValidationIssue>,
}

impl Validator {
    /// Creates a new validator.
    ///
    /// Implements: TJ-SPEC-006 F-006
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates a configuration and returns the result.
    ///
    /// This method collects all errors and warnings rather than stopping
    /// at the first issue.
    ///
    /// Implements: TJ-SPEC-006 F-006, F-007
    pub fn validate(&mut self, config: &ServerConfig, limits: &ConfigLimits) -> ValidationResult {
        self.errors.clear();
        self.warnings.clear();

        // Schema validation
        self.validate_server_metadata(config);
        self.validate_server_form(config);

        // Semantic validation
        if config.baseline.is_some() || config.phases.is_some() {
            self.validate_phased_server(config);
        } else {
            self.validate_simple_server(config);
        }

        // Limits validation
        self.validate_limits(config, limits);

        ValidationResult {
            errors: std::mem::take(&mut self.errors),
            warnings: std::mem::take(&mut self.warnings),
        }
    }

    // ========================================================================
    // Schema Validation
    // ========================================================================

    /// Validates server metadata.
    fn validate_server_metadata(&mut self, config: &ServerConfig) {
        // server.name is required
        if config.server.name.is_empty() {
            self.add_error("server.name", "Server name is required and cannot be empty");
        }

        // Warn on very long server names
        if config.server.name.len() > 100 {
            self.add_warning(
                "server.name",
                "Server name is unusually long (> 100 characters)",
            );
        }
    }

    /// Validates that the server form is consistent (simple vs phased).
    fn validate_server_form(&mut self, config: &ServerConfig) {
        let has_baseline = config.baseline.is_some();
        let has_phases = config.phases.is_some();
        let has_top_level_tools = config.tools.is_some();
        let has_top_level_resources = config.resources.is_some();
        let has_top_level_prompts = config.prompts.is_some();
        let has_top_level_items =
            has_top_level_tools || has_top_level_resources || has_top_level_prompts;

        // Cannot mix phased and simple server forms
        if has_baseline && has_top_level_items {
            self.add_error(
                "",
                "Cannot have both 'baseline' and top-level 'tools/resources/prompts'. \
                 Use either simple server form (tools/resources/prompts at root) \
                 or phased server form (baseline + phases).",
            );
        }

        // If using phases, should have baseline
        if has_phases && !has_baseline {
            self.add_warning(
                "phases",
                "Phases defined without baseline. The phase diffs will apply to an empty baseline.",
            );
        }
    }

    // ========================================================================
    // Simple Server Validation
    // ========================================================================

    /// Validates a simple (non-phased) server configuration.
    fn validate_simple_server(&mut self, config: &ServerConfig) {
        if let Some(tools) = &config.tools {
            self.validate_tools(tools, "tools");
        }

        if let Some(resources) = &config.resources {
            self.validate_resources(resources, "resources");
        }

        if let Some(prompts) = &config.prompts {
            self.validate_prompts(prompts, "prompts");
        }
    }

    // ========================================================================
    // Phased Server Validation
    // ========================================================================

    /// Validates a phased server configuration.
    fn validate_phased_server(&mut self, config: &ServerConfig) {
        // Collect baseline tool/resource/prompt names for cross-reference validation
        let baseline = config.baseline.as_ref();
        let baseline_tool_names = collect_baseline_tool_names(baseline);
        let baseline_resource_uris = collect_baseline_resource_uris(baseline);
        let baseline_prompt_names = collect_baseline_prompt_names(baseline);

        // Validate baseline
        if let Some(baseline) = baseline {
            self.validate_baseline(baseline);
        }

        // Validate phases
        if let Some(phases) = &config.phases {
            self.validate_phases(
                phases,
                &baseline_tool_names,
                &baseline_resource_uris,
                &baseline_prompt_names,
            );
        }
    }

    /// Validates the baseline state.
    fn validate_baseline(&mut self, baseline: &BaselineState) {
        self.validate_tools(&baseline.tools, "baseline.tools");
        self.validate_resources(&baseline.resources, "baseline.resources");
        self.validate_prompts(&baseline.prompts, "baseline.prompts");
    }

    /// Validates phases and their cross-references.
    fn validate_phases(
        &mut self,
        phases: &[Phase],
        baseline_tools: &HashSet<String>,
        baseline_resources: &HashSet<String>,
        baseline_prompts: &HashSet<String>,
    ) {
        // Check for duplicate phase names
        let mut phase_names = HashSet::new();
        for (idx, phase) in phases.iter().enumerate() {
            let path = format!("phases[{idx}]");

            // Phase name uniqueness
            if !phase_names.insert(&phase.name) {
                self.add_error(
                    &format!("{path}.name"),
                    &format!("Duplicate phase name: '{}'", phase.name),
                );
            }

            // Validate phase name is not empty
            if phase.name.is_empty() {
                self.add_error(&format!("{path}.name"), "Phase name cannot be empty");
            }

            // Validate trigger
            if let Some(trigger) = &phase.advance {
                self.validate_trigger(trigger, &format!("{path}.advance"));
            }

            // Validate replace_tools targets exist in baseline
            if let Some(replace_tools) = &phase.replace_tools {
                for tool_name in replace_tools.keys() {
                    if !baseline_tools.contains(tool_name) {
                        self.add_error(
                            &format!("{path}.replace_tools.{tool_name}"),
                            &format!(
                                "Cannot replace unknown tool '{tool_name}'. Tool not found in baseline."
                            ),
                        );
                    }
                }
            }

            // Validate remove_tools targets exist in baseline
            if let Some(remove_tools) = &phase.remove_tools {
                for tool_name in remove_tools {
                    if !baseline_tools.contains(tool_name) {
                        self.add_error(
                            &format!("{path}.remove_tools"),
                            &format!(
                                "Cannot remove unknown tool '{tool_name}'. Tool not found in baseline."
                            ),
                        );
                    }
                }
            }

            // Validate replace_resources targets exist in baseline
            if let Some(replace_resources) = &phase.replace_resources {
                for uri in replace_resources.keys() {
                    if !baseline_resources.contains(uri) {
                        self.add_error(
                            &format!("{path}.replace_resources.{uri}"),
                            &format!(
                                "Cannot replace unknown resource '{uri}'. Resource not found in baseline."
                            ),
                        );
                    }
                }
            }

            // Validate remove_resources targets exist in baseline
            if let Some(remove_resources) = &phase.remove_resources {
                for uri in remove_resources {
                    if !baseline_resources.contains(uri) {
                        self.add_error(
                            &format!("{path}.remove_resources"),
                            &format!(
                                "Cannot remove unknown resource '{uri}'. Resource not found in baseline."
                            ),
                        );
                    }
                }
            }

            // Validate replace_prompts targets exist in baseline
            if let Some(replace_prompts) = &phase.replace_prompts {
                for name in replace_prompts.keys() {
                    if !baseline_prompts.contains(name) {
                        self.add_error(
                            &format!("{path}.replace_prompts.{name}"),
                            &format!(
                                "Cannot replace unknown prompt '{name}'. Prompt not found in baseline."
                            ),
                        );
                    }
                }
            }

            // Validate remove_prompts targets exist in baseline
            if let Some(remove_prompts) = &phase.remove_prompts {
                for name in remove_prompts {
                    if !baseline_prompts.contains(name) {
                        self.add_error(
                            &format!("{path}.remove_prompts"),
                            &format!(
                                "Cannot remove unknown prompt '{name}'. Prompt not found in baseline."
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Validates a trigger configuration.
    fn validate_trigger(&mut self, trigger: &crate::config::schema::Trigger, path: &str) {
        // Check that at least one trigger type is specified
        let has_event = trigger.on.is_some();
        let has_time = trigger.after.is_some();

        if !has_event && !has_time {
            self.add_error(
                path,
                "Trigger must have either 'on' (event-based) or 'after' (time-based)",
            );
        }

        // Validate event name format
        if let Some(event) = &trigger.on {
            self.validate_event_name(event, &format!("{path}.on"));
        }

        // Validate duration format
        if let Some(duration) = &trigger.after {
            self.validate_duration(duration, &format!("{path}.after"));
        }

        // Validate timeout duration if present
        if let Some(timeout) = &trigger.timeout {
            self.validate_duration(timeout, &format!("{path}.timeout"));
        }
    }

    /// Validates an MCP event name.
    fn validate_event_name(&mut self, event: &str, path: &str) {
        // Valid event names:
        // - tools/call
        // - tools/list
        // - tools/call:tool_name
        // - resources/read
        // - resources/list
        // - resources/read:uri
        // - prompts/get
        // - prompts/list
        // - prompts/get:name
        // - initialize

        let valid_prefixes = [
            "tools/call",
            "tools/list",
            "resources/read",
            "resources/list",
            "prompts/get",
            "prompts/list",
            "initialize",
        ];

        // Extract the base event (before any colon)
        let base_event = event.split(':').next().unwrap_or(event);

        if !valid_prefixes.contains(&base_event) {
            self.add_error(
                path,
                &format!(
                    "Invalid event name '{event}'. Valid events: {}",
                    valid_prefixes.join(", ")
                ),
            );
        }
    }

    /// Validates a duration string (e.g., "30s", "5m").
    fn validate_duration(&mut self, duration: &str, path: &str) {
        // Valid formats: Ns, Nm, Nh, Nms
        // Where N is a positive integer

        let trimmed = duration.trim();
        if trimmed.is_empty() {
            self.add_error(path, "Duration cannot be empty");
            return;
        }

        // Check for valid suffixes
        let valid_suffixes = ["ms", "s", "m", "h"];
        let has_valid_suffix = valid_suffixes.iter().any(|s| trimmed.ends_with(s));

        if !has_valid_suffix {
            self.add_error(
                path,
                &format!(
                    "Invalid duration '{duration}'. Expected format: <number><unit> where unit is ms, s, m, or h"
                ),
            );
            return;
        }

        // Extract numeric part
        let numeric_part = trimmed.trim_end_matches(|c: char| c.is_alphabetic()).trim();

        if numeric_part.parse::<u64>().is_err() {
            self.add_error(
                path,
                &format!(
                    "Invalid duration '{duration}'. The numeric part must be a positive integer."
                ),
            );
        }
    }

    // ========================================================================
    // Tool/Resource/Prompt Validation
    // ========================================================================

    /// Validates a list of tool patterns.
    fn validate_tools(&mut self, tools: &[ToolPattern], base_path: &str) {
        let mut tool_names = HashSet::new();

        for (idx, tool) in tools.iter().enumerate() {
            let path = format!("{base_path}[{idx}]");

            // Tool name is required
            if tool.tool.name.is_empty() {
                self.add_error(&format!("{path}.tool.name"), "Tool name is required");
            }

            // Tool name uniqueness
            if !tool_names.insert(&tool.tool.name) {
                self.add_error(
                    &format!("{path}.tool.name"),
                    &format!("Duplicate tool name: '{}'", tool.tool.name),
                );
            }

            // Tool description is required
            if tool.tool.description.is_empty() {
                self.add_error(
                    &format!("{path}.tool.description"),
                    "Tool description is required",
                );
            }

            // inputSchema must be an object
            if !tool.tool.input_schema.is_object() {
                self.add_error(
                    &format!("{path}.tool.inputSchema"),
                    "inputSchema must be a JSON object",
                );
            }

            // Response content must not be empty
            if tool.response.content.is_empty() {
                self.add_error(
                    &format!("{path}.response.content"),
                    "Response content cannot be empty",
                );
            }
        }
    }

    /// Validates a list of resource patterns.
    fn validate_resources(&mut self, resources: &[ResourcePattern], base_path: &str) {
        let mut resource_uris = HashSet::new();

        for (idx, resource) in resources.iter().enumerate() {
            let path = format!("{base_path}[{idx}]");

            // Resource URI is required
            if resource.resource.uri.is_empty() {
                self.add_error(&format!("{path}.resource.uri"), "Resource URI is required");
            }

            // Resource URI uniqueness
            if !resource_uris.insert(&resource.resource.uri) {
                self.add_error(
                    &format!("{path}.resource.uri"),
                    &format!("Duplicate resource URI: '{}'", resource.resource.uri),
                );
            }

            // Resource name is required
            if resource.resource.name.is_empty() {
                self.add_error(
                    &format!("{path}.resource.name"),
                    "Resource name is required",
                );
            }
        }
    }

    /// Validates a list of prompt patterns.
    fn validate_prompts(&mut self, prompts: &[PromptPattern], base_path: &str) {
        let mut prompt_names = HashSet::new();

        for (idx, prompt) in prompts.iter().enumerate() {
            let path = format!("{base_path}[{idx}]");

            // Prompt name is required
            if prompt.prompt.name.is_empty() {
                self.add_error(&format!("{path}.prompt.name"), "Prompt name is required");
            }

            // Prompt name uniqueness
            if !prompt_names.insert(&prompt.prompt.name) {
                self.add_error(
                    &format!("{path}.prompt.name"),
                    &format!("Duplicate prompt name: '{}'", prompt.prompt.name),
                );
            }

            // Response messages must not be empty
            if prompt.response.messages.is_empty() {
                self.add_error(
                    &format!("{path}.response.messages"),
                    "Prompt response messages cannot be empty",
                );
            }
        }
    }

    // ========================================================================
    // Limits Validation
    // ========================================================================

    /// Validates configuration against size limits.
    fn validate_limits(&mut self, config: &ServerConfig, limits: &ConfigLimits) {
        // Count phases
        if let Some(phases) = &config.phases {
            if phases.len() > limits.max_phases {
                self.add_error(
                    "phases",
                    &format!(
                        "Too many phases: {} (maximum: {}). \
                         Set THOUGHTJACK_MAX_PHASES to increase the limit.",
                        phases.len(),
                        limits.max_phases
                    ),
                );
            }
        }

        // Count tools
        let tool_count = count_total_tools(config);
        if tool_count > limits.max_tools {
            self.add_error(
                "tools",
                &format!(
                    "Too many tools: {tool_count} (maximum: {}). \
                     Set THOUGHTJACK_MAX_TOOLS to increase the limit.",
                    limits.max_tools
                ),
            );
        }

        // Count resources
        let resource_count = count_total_resources(config);
        if resource_count > limits.max_resources {
            self.add_error(
                "resources",
                &format!(
                    "Too many resources: {resource_count} (maximum: {}). \
                     Set THOUGHTJACK_MAX_RESOURCES to increase the limit.",
                    limits.max_resources
                ),
            );
        }

        // Count prompts
        let prompt_count = count_total_prompts(config);
        if prompt_count > limits.max_prompts {
            self.add_error(
                "prompts",
                &format!(
                    "Too many prompts: {prompt_count} (maximum: {}). \
                     Set THOUGHTJACK_MAX_PROMPTS to increase the limit.",
                    limits.max_prompts
                ),
            );
        }
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    /// Adds an error to the collection.
    fn add_error(&mut self, path: &str, message: &str) {
        self.errors.push(ValidationIssue {
            path: path.to_string(),
            message: message.to_string(),
            severity: Severity::Error,
        });
    }

    /// Adds a warning to the collection.
    fn add_warning(&mut self, path: &str, message: &str) {
        self.warnings.push(ValidationIssue {
            path: path.to_string(),
            message: message.to_string(),
            severity: Severity::Warning,
        });
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Collects tool names from baseline.
fn collect_baseline_tool_names(baseline: Option<&BaselineState>) -> HashSet<String> {
    baseline.map_or_else(HashSet::new, |b| {
        b.tools.iter().map(|t| t.tool.name.clone()).collect()
    })
}

/// Collects resource URIs from baseline.
fn collect_baseline_resource_uris(baseline: Option<&BaselineState>) -> HashSet<String> {
    baseline.map_or_else(HashSet::new, |b| {
        b.resources.iter().map(|r| r.resource.uri.clone()).collect()
    })
}

/// Collects prompt names from baseline.
fn collect_baseline_prompt_names(baseline: Option<&BaselineState>) -> HashSet<String> {
    baseline.map_or_else(HashSet::new, |b| {
        b.prompts.iter().map(|p| p.prompt.name.clone()).collect()
    })
}

/// Counts total tools in the configuration.
fn count_total_tools(config: &ServerConfig) -> usize {
    let baseline_count = config.baseline.as_ref().map_or(0, |b| b.tools.len());
    let simple_count = config.tools.as_ref().map_or(0, Vec::len);
    let phase_added: usize = config.phases.as_ref().map_or(0, |phases| {
        phases
            .iter()
            .map(|p| p.add_tools.as_ref().map_or(0, Vec::len))
            .sum()
    });

    baseline_count + simple_count + phase_added
}

/// Counts total resources in the configuration.
fn count_total_resources(config: &ServerConfig) -> usize {
    let baseline_count = config.baseline.as_ref().map_or(0, |b| b.resources.len());
    let simple_count = config.resources.as_ref().map_or(0, Vec::len);
    let phase_added: usize = config.phases.as_ref().map_or(0, |phases| {
        phases
            .iter()
            .map(|p| p.add_resources.as_ref().map_or(0, Vec::len))
            .sum()
    });

    baseline_count + simple_count + phase_added
}

/// Counts total prompts in the configuration.
fn count_total_prompts(config: &ServerConfig) -> usize {
    let baseline_count = config.baseline.as_ref().map_or(0, |b| b.prompts.len());
    let simple_count = config.prompts.as_ref().map_or(0, Vec::len);
    let phase_added: usize = config.phases.as_ref().map_or(0, |phases| {
        phases
            .iter()
            .map(|p| p.add_prompts.as_ref().map_or(0, Vec::len))
            .sum()
    });

    baseline_count + simple_count + phase_added
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::*;

    fn default_limits() -> ConfigLimits {
        ConfigLimits::default()
    }

    fn minimal_config() -> ServerConfig {
        ServerConfig {
            server: ServerMetadata {
                name: "test-server".to_string(),
                version: None,
                state_scope: None,
                capabilities: None,
            },
            baseline: None,
            tools: None,
            resources: None,
            prompts: None,
            phases: None,
            behavior: None,
            logging: None,
            unknown_methods: None,
        }
    }

    fn make_tool(name: &str) -> ToolPattern {
        ToolPattern {
            tool: ToolDefinition {
                name: name.to_string(),
                description: "Test tool".to_string(),
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

    fn make_resource(uri: &str) -> ResourcePattern {
        ResourcePattern {
            resource: ResourceDefinition {
                uri: uri.to_string(),
                name: "Test resource".to_string(),
                description: None,
                mime_type: None,
            },
            response: None,
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
            response: PromptResponse {
                messages: vec![PromptMessage {
                    role: Role::User,
                    content: ContentValue::Static("test".to_string()),
                }],
            },
            behavior: None,
        }
    }

    #[test]
    fn test_validate_minimal_config() {
        let config = minimal_config();
        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());
        assert!(result.is_valid());
    }

    #[test]
    fn test_validate_empty_server_name() {
        let mut config = minimal_config();
        config.server.name = String::new();

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(result.errors.iter().any(|e| e.path == "server.name"));
    }

    #[test]
    fn test_validate_mixed_server_forms() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState::default());
        config.tools = Some(vec![make_tool("calc")]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Cannot have both"))
        );
    }

    #[test]
    fn test_validate_duplicate_tool_names() {
        let mut config = minimal_config();
        config.tools = Some(vec![make_tool("calc"), make_tool("calc")]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Duplicate tool name"))
        );
    }

    #[test]
    fn test_validate_empty_tool_name() {
        let mut config = minimal_config();
        let mut tool = make_tool("");
        tool.tool.name = String::new();
        config.tools = Some(vec![tool]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Tool name is required"))
        );
    }

    #[test]
    fn test_validate_missing_tool_description() {
        let mut config = minimal_config();
        let mut tool = make_tool("calc");
        tool.tool.description = String::new();
        config.tools = Some(vec![tool]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("description is required"))
        );
    }

    #[test]
    fn test_validate_invalid_input_schema() {
        let mut config = minimal_config();
        let mut tool = make_tool("calc");
        tool.tool.input_schema = serde_json::json!("not an object");
        config.tools = Some(vec![tool]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("inputSchema must be a JSON object"))
        );
    }

    #[test]
    fn test_validate_empty_response_content() {
        let mut config = minimal_config();
        let mut tool = make_tool("calc");
        tool.response.content = vec![];
        config.tools = Some(vec![tool]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Response content cannot be empty"))
        );
    }

    #[test]
    fn test_validate_duplicate_phase_names() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState::default());
        config.phases = Some(vec![
            Phase {
                name: "phase1".to_string(),
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
            },
            Phase {
                name: "phase1".to_string(), // Duplicate
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
            },
        ]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Duplicate phase name"))
        );
    }

    #[test]
    fn test_validate_dangling_replace_tools() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState {
            tools: vec![make_tool("calc")],
            resources: vec![],
            prompts: vec![],
            capabilities: None,
            behavior: None,
        });
        config.phases = Some(vec![Phase {
            name: "exploit".to_string(),
            advance: None,
            on_enter: None,
            replace_tools: Some(
                [(
                    "nonexistent".to_string(),
                    ToolPatternRef::Inline(make_tool("evil")),
                )]
                .into_iter()
                .collect(),
            ),
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
        }]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Cannot replace unknown tool"))
        );
    }

    #[test]
    fn test_validate_dangling_remove_tools() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState {
            tools: vec![make_tool("calc")],
            resources: vec![],
            prompts: vec![],
            capabilities: None,
            behavior: None,
        });
        config.phases = Some(vec![Phase {
            name: "exploit".to_string(),
            advance: None,
            on_enter: None,
            replace_tools: None,
            add_tools: None,
            remove_tools: Some(vec!["nonexistent".to_string()]),
            replace_resources: None,
            add_resources: None,
            remove_resources: None,
            replace_prompts: None,
            add_prompts: None,
            remove_prompts: None,
            replace_capabilities: None,
            behavior: None,
        }]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Cannot remove unknown tool"))
        );
    }

    #[test]
    fn test_validate_invalid_event_name() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState::default());
        config.phases = Some(vec![Phase {
            name: "phase1".to_string(),
            advance: Some(Trigger {
                on: Some("invalid/event".to_string()),
                count: None,
                match_condition: None,
                after: None,
                timeout: None,
                on_timeout: None,
            }),
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
        }]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Invalid event name"))
        );
    }

    #[test]
    fn test_validate_valid_event_names() {
        let mut validator = Validator::new();

        for event in [
            "tools/call",
            "tools/list",
            "tools/call:calculator",
            "resources/read",
            "resources/list",
            "resources/read:file:///etc/passwd",
            "prompts/get",
            "prompts/list",
            "initialize",
        ] {
            validator.errors.clear();
            validator.validate_event_name(event, "test");
            assert!(
                validator.errors.is_empty(),
                "Event '{event}' should be valid"
            );
        }
    }

    #[test]
    fn test_validate_invalid_duration() {
        let mut validator = Validator::new();

        for duration in ["", "30", "abc", "30x", "-5s"] {
            validator.errors.clear();
            validator.validate_duration(duration, "test");
            assert!(
                !validator.errors.is_empty(),
                "Duration '{duration}' should be invalid"
            );
        }
    }

    #[test]
    fn test_validate_valid_durations() {
        let mut validator = Validator::new();

        for duration in ["30s", "5m", "1h", "100ms", "0s"] {
            validator.errors.clear();
            validator.validate_duration(duration, "test");
            assert!(
                validator.errors.is_empty(),
                "Duration '{duration}' should be valid"
            );
        }
    }

    #[test]
    fn test_validate_trigger_missing_type() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState::default());
        config.phases = Some(vec![Phase {
            name: "phase1".to_string(),
            advance: Some(Trigger {
                on: None,
                count: Some(3), // count without event
                match_condition: None,
                after: None,
                timeout: None,
                on_timeout: None,
            }),
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
        }]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(result.errors.iter().any(|e| {
            e.message
                .contains("'on' (event-based) or 'after' (time-based)")
        }));
    }

    #[test]
    fn test_validate_too_many_phases() {
        let mut config = minimal_config();
        config.baseline = Some(BaselineState::default());
        config.phases = Some(
            (0..200)
                .map(|i| Phase {
                    name: format!("phase{i}"),
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
                })
                .collect(),
        );

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Too many phases"))
        );
    }

    #[test]
    fn test_validate_collects_all_errors() {
        let mut config = minimal_config();
        config.server.name = String::new(); // Error 1

        let mut tool1 = make_tool("calc");
        tool1.tool.description = String::new(); // Error 2
        tool1.response.content = vec![]; // Error 3

        let mut tool2 = make_tool("calc"); // Error 4: duplicate
        tool2.tool.input_schema = serde_json::json!("not object"); // Error 5

        config.tools = Some(vec![tool1, tool2]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        // Should have collected all errors, not stopped at first
        assert!(result.errors.len() >= 4);
    }

    #[test]
    fn test_validate_phases_without_baseline_warning() {
        let mut config = minimal_config();
        config.phases = Some(vec![Phase {
            name: "phase1".to_string(),
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
        }]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(!result.warnings.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.message.contains("Phases defined without baseline"))
        );
    }

    #[test]
    fn test_validate_duplicate_resource_uris() {
        let mut config = minimal_config();
        config.resources = Some(vec![
            make_resource("file:///test"),
            make_resource("file:///test"),
        ]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Duplicate resource URI"))
        );
    }

    #[test]
    fn test_validate_duplicate_prompt_names() {
        let mut config = minimal_config();
        config.prompts = Some(vec![make_prompt("greet"), make_prompt("greet")]);

        let mut validator = Validator::new();
        let result = validator.validate(&config, &default_limits());

        assert!(result.has_errors());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("Duplicate prompt name"))
        );
    }
}
