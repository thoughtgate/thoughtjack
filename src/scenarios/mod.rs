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
    /// Compound attacks combining multiple techniques.
    #[value(name = "multi_vector")]
    MultiVector,
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
            Self::MultiVector => "Multi-Vector",
        }
    }

    /// Returns all category variants in display order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Temporal,
            Self::Injection,
            Self::Resource,
            Self::DoS,
            Self::Protocol,
            Self::MultiVector,
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
            Self::MultiVector => write!(f, "multi_vector"),
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
        // ── Temporal ────────────────────────────────────────────
        BuiltinScenario {
            name: "rug-pull",
            description: "Trust-building calculator that swaps tool definitions after 5 calls",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-002"],
            tags: &[
                "supply-chain",
                "phased",
                "trust-building",
                "tool-swap",
                "tier-1",
            ],
            features: &[
                "phases",
                "replace-tools",
                "add-tools",
                "list-changed-notification",
            ],
            yaml: include_str!("../../scenarios/rug-pull.yaml"),
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
            total_bytes < 200_000,
            "Total embedded YAML is {total_bytes} bytes, exceeds 200KB budget"
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
        let temporal = list_scenarios(Some(ScenarioCategory::Temporal), None);
        assert!(
            !temporal.is_empty(),
            "Expected at least 1 temporal scenario"
        );
        for s in &temporal {
            assert_eq!(s.category, ScenarioCategory::Temporal);
        }
    }

    #[test]
    fn list_filter_by_tag() {
        let phased = list_scenarios(None, Some("phased"));
        assert!(!phased.is_empty(), "Expected at least 1 phased scenario");
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
    fn list_filter_empty_category() {
        let result = list_scenarios(Some(ScenarioCategory::Protocol), None);
        assert!(
            result.is_empty(),
            "No protocol scenarios after v0.2 archive"
        );
    }

    #[test]
    fn category_display_lowercase() {
        assert_eq!(ScenarioCategory::Injection.to_string(), "injection");
        assert_eq!(ScenarioCategory::DoS.to_string(), "dos");
        assert_eq!(ScenarioCategory::Temporal.to_string(), "temporal");
        assert_eq!(ScenarioCategory::Resource.to_string(), "resource");
        assert_eq!(ScenarioCategory::Protocol.to_string(), "protocol");
        assert_eq!(ScenarioCategory::MultiVector.to_string(), "multi_vector");
    }

    #[test]
    fn category_label_titlecase() {
        assert_eq!(ScenarioCategory::Injection.label(), "Injection");
        assert_eq!(ScenarioCategory::DoS.label(), "DoS");
        assert_eq!(ScenarioCategory::Temporal.label(), "Temporal");
        assert_eq!(ScenarioCategory::Resource.label(), "Resource");
        assert_eq!(ScenarioCategory::Protocol.label(), "Protocol");
        assert_eq!(ScenarioCategory::MultiVector.label(), "Multi-Vector");
    }

    #[test]
    fn list_scenario_names_returns_all() {
        let names = list_scenario_names();
        assert_eq!(names.len(), 1, "Expected exactly 1 built-in scenario");
        assert!(names.contains(&"rug-pull"));
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
}
