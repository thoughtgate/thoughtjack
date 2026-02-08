//! Built-in attack scenarios (TJ-SPEC-010)
//!
//! Curated attack configurations embedded in the binary at compile time.
//! Enables zero-configuration usage: `thoughtjack server run --scenario rug-pull`

use std::fmt;
use std::sync::LazyLock;

// ============================================================================
// Types
// ============================================================================

/// A built-in scenario embedded in the binary.
///
/// Each scenario is a self-contained YAML configuration that demonstrates
/// a specific attack technique from the `ThoughtJack` attack taxonomy.
///
/// Implements: TJ-SPEC-010 F-001
pub struct BuiltinScenario {
    /// Unique identifier (kebab-case, e.g., "rug-pull").
    pub name: &'static str,

    /// Short human-readable description.
    pub description: &'static str,

    /// Category for organization.
    pub category: ScenarioCategory,

    /// Attack taxonomy IDs this scenario demonstrates.
    pub taxonomy: &'static [&'static str],

    /// Tags for filtering.
    pub tags: &'static [&'static str],

    /// `ThoughtJack` features exercised by this scenario.
    pub features: &'static [&'static str],

    /// Raw YAML content (embedded at compile time).
    pub yaml: &'static str,
}

/// Category for organizing built-in scenarios.
///
/// Implements: TJ-SPEC-010 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScenarioCategory {
    /// Prompt injection and content manipulation.
    Injection,
    /// Denial of service and resource exhaustion.
    #[value(name = "dos")]
    DoS,
    /// Temporal attacks (rug pulls, sleepers).
    Temporal,
    /// Resource and subscription attacks.
    Resource,
    /// Protocol-level attacks.
    Protocol,
}

impl ScenarioCategory {
    /// Returns the human-readable title-case label.
    ///
    /// Implements: TJ-SPEC-010 F-004
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Injection => "Injection",
            Self::DoS => "DoS",
            Self::Temporal => "Temporal",
            Self::Resource => "Resource",
            Self::Protocol => "Protocol",
        }
    }

    /// Returns all category variants in display order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Injection,
            Self::DoS,
            Self::Temporal,
            Self::Resource,
            Self::Protocol,
        ]
    }
}

impl fmt::Display for ScenarioCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Injection => write!(f, "injection"),
            Self::DoS => write!(f, "dos"),
            Self::Temporal => write!(f, "temporal"),
            Self::Resource => write!(f, "resource"),
            Self::Protocol => write!(f, "protocol"),
        }
    }
}

// ============================================================================
// Registry
// ============================================================================

/// Global registry of all built-in scenarios.
///
/// Implements: TJ-SPEC-010 F-001, F-002
static BUILTIN_SCENARIOS: LazyLock<Vec<BuiltinScenario>> = LazyLock::new(|| {
    vec![
        BuiltinScenario {
            name: "prompt-injection",
            description: "Context-aware prompt injection via web search tool",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM-001", "CPM-005"],
            tags: &["injection", "dynamic", "search"],
            features: &["template-interpolation", "match-conditions"],
            yaml: include_str!("../../scenarios/prompt-injection.yaml"),
        },
        BuiltinScenario {
            name: "credential-phishing",
            description: "Search tool returning fake credentials with injection",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM-001"],
            tags: &["injection", "credentials", "phishing"],
            features: &["template-interpolation"],
            yaml: include_str!("../../scenarios/credential-phishing.yaml"),
        },
        BuiltinScenario {
            name: "unicode-obfuscation",
            description: "Unicode homoglyphs and invisibles in tool responses",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM-003"],
            tags: &["injection", "unicode", "obfuscation"],
            features: &["generate-unicode-spam"],
            yaml: include_str!("../../scenarios/unicode-obfuscation.yaml"),
        },
        BuiltinScenario {
            name: "slow-loris",
            description: "Slow loris delivery to test client timeout handling",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-004"],
            tags: &["dos", "slow-loris", "timeout"],
            features: &["slow-loris-delivery"],
            yaml: include_str!("../../scenarios/slow-loris.yaml"),
        },
        BuiltinScenario {
            name: "nested-json-dos",
            description: "Deeply nested JSON payloads for parser DoS",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-001"],
            tags: &["dos", "nested-json", "parser"],
            features: &["generate-nested-json"],
            yaml: include_str!("../../scenarios/nested-json-dos.yaml"),
        },
        BuiltinScenario {
            name: "notification-flood",
            description: "Server-initiated notification flood on tool calls",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-006"],
            tags: &["dos", "notification", "flood"],
            features: &["notification-flood", "side-effects"],
            yaml: include_str!("../../scenarios/notification-flood.yaml"),
        },
        BuiltinScenario {
            name: "rug-pull",
            description: "Trust-building phase followed by tool definition swap",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-002"],
            tags: &["phased", "rug-pull", "tool-shadowing"],
            features: &["phases", "list-changed-notification"],
            yaml: include_str!("../../scenarios/rug-pull.yaml"),
        },
        BuiltinScenario {
            name: "response-sequence",
            description: "Benign responses initially, injection on third call",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-005"],
            tags: &["phased", "sequence", "trust-building"],
            features: &["response-sequence"],
            yaml: include_str!("../../scenarios/response-sequence.yaml"),
        },
        BuiltinScenario {
            name: "resource-exfiltration",
            description: "Fake credentials for sensitive file paths",
            category: ScenarioCategory::Resource,
            taxonomy: &["RSC-001", "RSC-002"],
            tags: &["resource", "exfiltration", "credentials"],
            features: &["resource-patterns"],
            yaml: include_str!("../../scenarios/resource-exfiltration.yaml"),
        },
        BuiltinScenario {
            name: "resource-rug-pull",
            description: "Benign content then malicious after subscription",
            category: ScenarioCategory::Resource,
            taxonomy: &["RSC-006"],
            tags: &["phased", "resource", "rug-pull"],
            features: &["phases", "replace-resources", "list-changed-notification"],
            yaml: include_str!("../../scenarios/resource-rug-pull.yaml"),
        },
    ]
});

// ============================================================================
// Public API
// ============================================================================

/// Look up a scenario by exact name.
///
/// Implements: TJ-SPEC-010 F-001
#[must_use]
pub fn find_scenario(name: &str) -> Option<&'static BuiltinScenario> {
    BUILTIN_SCENARIOS.iter().find(|s| s.name == name)
}

/// List all scenarios, optionally filtered by category and/or tag.
///
/// Implements: TJ-SPEC-010 F-004
#[must_use]
pub fn list_scenarios(
    category: Option<ScenarioCategory>,
    tag: Option<&str>,
) -> Vec<&'static BuiltinScenario> {
    BUILTIN_SCENARIOS
        .iter()
        .filter(|s| category.is_none_or(|c| s.category == c))
        .filter(|s| tag.is_none_or(|t| s.tags.contains(&t)))
        .collect()
}

/// Suggest a similar scenario name for typo correction.
///
/// Returns the closest match if its Damerau-Levenshtein distance is ≤ 3.
///
/// Implements: TJ-SPEC-010 F-006
#[must_use]
pub fn suggest_scenario(input: &str) -> Option<String> {
    BUILTIN_SCENARIOS
        .iter()
        .map(|s| (s.name, strsim::damerau_levenshtein(input, s.name)))
        .filter(|(_, dist)| *dist <= 3)
        .min_by_key(|(_, dist)| *dist)
        .map(|(name, _)| name.to_string())
}

/// Returns all scenario names in registry order.
///
/// Implements: TJ-SPEC-010 F-003
#[must_use]
pub fn list_scenario_names() -> Vec<&'static str> {
    BUILTIN_SCENARIOS.iter().map(|s| s.name).collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::config::loader::{ConfigLoader, LoaderOptions};

    #[test]
    fn all_builtin_scenarios_parse_successfully() {
        for scenario in list_scenarios(None, None) {
            let options = LoaderOptions {
                embedded: true,
                ..LoaderOptions::default()
            };
            let mut loader = ConfigLoader::new(options);
            let result = loader.load_from_str(scenario.yaml);
            assert!(
                result.is_ok(),
                "Built-in scenario '{}' failed to parse: {:?}",
                scenario.name,
                result.err()
            );
        }
    }

    #[test]
    fn no_duplicate_scenario_names() {
        let names: Vec<&str> = list_scenarios(None, None).iter().map(|s| s.name).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "Duplicate scenario names found");
    }

    #[test]
    fn all_builtin_scenarios_are_self_contained() {
        for scenario in list_scenarios(None, None) {
            assert!(
                !scenario.yaml.contains("$include:"),
                "Built-in scenario '{}' contains $include directive",
                scenario.name,
            );
            assert!(
                !scenario.yaml.contains("$file:"),
                "Built-in scenario '{}' contains $file directive",
                scenario.name,
            );
            assert!(
                !scenario.yaml.contains("${env."),
                "Built-in scenario '{}' references environment variables",
                scenario.name,
            );
        }
    }

    #[test]
    fn builtin_scenarios_within_binary_size_budget() {
        let total_bytes: usize = list_scenarios(None, None)
            .iter()
            .map(|s| s.yaml.len())
            .sum();
        assert!(
            total_bytes < 50_000,
            "Total embedded YAML is {total_bytes} bytes, exceeds 50KB budget"
        );
    }

    #[test]
    fn find_scenario_existing() {
        let scenario = find_scenario("rug-pull");
        assert!(scenario.is_some());
        assert_eq!(scenario.unwrap().name, "rug-pull");
        assert_eq!(scenario.unwrap().category, ScenarioCategory::Temporal);
    }

    #[test]
    fn find_scenario_missing() {
        assert!(find_scenario("nonexistent").is_none());
    }

    #[test]
    fn suggest_scenario_close() {
        // "rull-pull" is close to "rug-pull" (distance 2)
        let suggestion = suggest_scenario("rull-pull");
        assert_eq!(suggestion, Some("rug-pull".to_string()));
    }

    #[test]
    fn suggest_scenario_far() {
        // "xyzabc123" is too far from any scenario name
        let suggestion = suggest_scenario("xyzabc123");
        assert!(suggestion.is_none());
    }

    #[test]
    fn list_filter_by_category() {
        let injection = list_scenarios(Some(ScenarioCategory::Injection), None);
        assert!(
            injection.len() >= 2,
            "Expected at least 2 injection scenarios"
        );
        for s in &injection {
            assert_eq!(s.category, ScenarioCategory::Injection);
        }
    }

    #[test]
    fn list_filter_by_tag() {
        let phased = list_scenarios(None, Some("phased"));
        assert!(phased.len() >= 2, "Expected at least 2 phased scenarios");
        for s in &phased {
            assert!(s.tags.contains(&"phased"));
        }
    }

    #[test]
    fn list_filter_by_category_and_tag() {
        let result = list_scenarios(Some(ScenarioCategory::Temporal), Some("phased"));
        assert!(!result.is_empty());
        for s in &result {
            assert_eq!(s.category, ScenarioCategory::Temporal);
            assert!(s.tags.contains(&"phased"));
        }
    }

    #[test]
    fn list_filter_empty_result() {
        let result = list_scenarios(Some(ScenarioCategory::Protocol), None);
        assert!(result.is_empty(), "No protocol scenarios should exist yet");
    }

    #[test]
    fn category_display_lowercase() {
        assert_eq!(ScenarioCategory::Injection.to_string(), "injection");
        assert_eq!(ScenarioCategory::DoS.to_string(), "dos");
        assert_eq!(ScenarioCategory::Temporal.to_string(), "temporal");
        assert_eq!(ScenarioCategory::Resource.to_string(), "resource");
        assert_eq!(ScenarioCategory::Protocol.to_string(), "protocol");
    }

    #[test]
    fn category_label_titlecase() {
        assert_eq!(ScenarioCategory::Injection.label(), "Injection");
        assert_eq!(ScenarioCategory::DoS.label(), "DoS");
        assert_eq!(ScenarioCategory::Temporal.label(), "Temporal");
        assert_eq!(ScenarioCategory::Resource.label(), "Resource");
        assert_eq!(ScenarioCategory::Protocol.label(), "Protocol");
    }

    #[test]
    fn list_scenario_names_returns_all() {
        let names = list_scenario_names();
        assert_eq!(names.len(), 10, "Expected exactly 10 built-in scenarios");
        assert!(names.contains(&"rug-pull"));
        assert!(names.contains(&"prompt-injection"));
        assert!(names.contains(&"slow-loris"));
    }

    #[test]
    fn scenario_metadata_populated() {
        for scenario in list_scenarios(None, None) {
            assert!(!scenario.name.is_empty(), "Scenario name is empty");
            assert!(
                !scenario.description.is_empty(),
                "Scenario '{}' has empty description",
                scenario.name
            );
            assert!(
                !scenario.taxonomy.is_empty(),
                "Scenario '{}' has no taxonomy IDs",
                scenario.name
            );
            assert!(
                !scenario.tags.is_empty(),
                "Scenario '{}' has no tags",
                scenario.name
            );
            assert!(
                !scenario.features.is_empty(),
                "Scenario '{}' has no features",
                scenario.name
            );
            assert!(
                !scenario.yaml.is_empty(),
                "Scenario '{}' has empty YAML",
                scenario.name
            );
        }
    }

    // TJ-SPEC-010 F-005: All built-in scenarios validate semantically
    #[test]
    fn all_builtin_scenarios_validate_semantically() {
        for scenario in list_scenarios(None, None) {
            let options = LoaderOptions {
                embedded: true,
                ..LoaderOptions::default()
            };
            let mut loader = ConfigLoader::new(options);
            let load_result = loader
                .load_from_str(scenario.yaml)
                .unwrap_or_else(|e| panic!("Scenario '{}' failed to parse: {e}", scenario.name));

            let config = &load_result.config;

            // Verify server metadata
            assert!(
                !config.server.name.is_empty(),
                "Scenario '{}' has empty server name",
                scenario.name
            );

            // If tools are defined, verify they have non-empty names
            if let Some(ref tools) = config.tools {
                for tool in tools {
                    assert!(
                        !tool.tool.name.is_empty(),
                        "Scenario '{}' has tool with empty name",
                        scenario.name
                    );
                }
            }

            // If phases are defined, verify they have names and valid structure
            if let Some(ref phases) = config.phases {
                for phase in phases {
                    assert!(
                        !phase.name.is_empty(),
                        "Scenario '{}' has phase with empty name",
                        scenario.name
                    );
                }
            }

            // No warnings should be emitted for built-in scenarios
            assert!(
                load_result.warnings.is_empty(),
                "Scenario '{}' produced warnings: {:?}",
                scenario.name,
                load_result
                    .warnings
                    .iter()
                    .map(|w| &w.message)
                    .collect::<Vec<_>>()
            );
        }
    }
}
