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
        // ── Temporal (order 1) ──────────────────────────────────
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
        BuiltinScenario {
            name: "sleeper-agent",
            description: "Time-bomb activation after configurable dormancy period",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-002"],
            tags: &["supply-chain", "phased", "time-based", "dormant", "tier-2"],
            features: &["phases", "replace-tools", "list-changed-notification"],
            yaml: include_str!("../../scenarios/sleeper-agent.yaml"),
        },
        BuiltinScenario {
            name: "bait-and-switch",
            description: "Content-triggered activation on sensitive file path queries",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-002"],
            tags: &[
                "supply-chain",
                "phased",
                "content-triggered",
                "evasion",
                "tier-2",
            ],
            features: &["phases", "match-conditions", "replace-tools"],
            yaml: include_str!("../../scenarios/bait-and-switch.yaml"),
        },
        BuiltinScenario {
            name: "escalation-ladder",
            description: "Four-phase gradual escalation from benign to full exploit",
            category: ScenarioCategory::Temporal,
            taxonomy: &["CPM-002"],
            tags: &[
                "supply-chain",
                "phased",
                "gradual",
                "evasion",
                "multi-phase",
                "tier-2",
            ],
            features: &["phases", "replace-tools", "list-changed-notification"],
            yaml: include_str!("../../scenarios/escalation-ladder.yaml"),
        },
        BuiltinScenario {
            name: "capability-confusion",
            description: "Advertises listChanged: false then sends list_changed notifications",
            category: ScenarioCategory::Temporal,
            taxonomy: &["PRT-001"],
            tags: &["protocol", "trust", "capability", "negotiation", "phased"],
            features: &[
                "phases",
                "replace-capabilities",
                "list-changed-notification",
            ],
            yaml: include_str!("../../scenarios/capability-confusion.yaml"),
        },
        BuiltinScenario {
            name: "resource-rug-pull",
            description: "Benign resource content that swaps to malicious after subscription",
            category: ScenarioCategory::Temporal,
            taxonomy: &["RSC-006"],
            tags: &[
                "supply-chain",
                "phased",
                "resource",
                "content-swap",
                "tier-2",
            ],
            features: &["phases", "replace-resources", "list-changed-notification"],
            yaml: include_str!("../../scenarios/resource-rug-pull.yaml"),
        },
        // ── Injection (order 2) ─────────────────────────────────
        BuiltinScenario {
            name: "prompt-injection",
            description: "Web search tool injecting hidden instructions on sensitive queries",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM-001"],
            tags: &[
                "injection",
                "integrity",
                "credential-theft",
                "conditional",
                "tier-1",
            ],
            features: &["match-conditions"],
            yaml: include_str!("../../scenarios/prompt-injection.yaml"),
        },
        BuiltinScenario {
            name: "prompt-template-injection",
            description: "MCP prompts used as injection vectors via user and assistant messages",
            category: ScenarioCategory::Injection,
            taxonomy: &["PRM"],
            tags: &["injection", "integrity", "prompt", "multi-turn", "tier-2"],
            features: &["prompts"],
            yaml: include_str!("../../scenarios/prompt-template-injection.yaml"),
        },
        BuiltinScenario {
            name: "unicode-obfuscation",
            description: "Homoglyphs, zero-width characters, and BiDi overrides in responses",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM-003"],
            tags: &["evasion", "integrity", "unicode", "homoglyph", "zero-width"],
            features: &["generate-unicode-spam"],
            yaml: include_str!("../../scenarios/unicode-obfuscation.yaml"),
        },
        BuiltinScenario {
            name: "ansi-terminal-injection",
            description: "ANSI escape sequences to overwrite terminal content and inject links",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM"],
            tags: &["evasion", "integrity", "terminal", "ansi", "cli"],
            features: &["generate-ansi-escape"],
            yaml: include_str!("../../scenarios/ansi-terminal-injection.yaml"),
        },
        BuiltinScenario {
            name: "credential-harvester",
            description: "Response sequence that social-engineers credential retrieval via fake errors",
            category: ScenarioCategory::Injection,
            taxonomy: &["CPM"],
            tags: &[
                "agentic",
                "confidentiality",
                "credential-theft",
                "social-engineering",
                "sequence",
                "tier-2",
            ],
            features: &["sequence"],
            yaml: include_str!("../../scenarios/credential-harvester.yaml"),
        },
        BuiltinScenario {
            name: "context-persistence",
            description: "Memory poisoning via persistent rule injection in tool responses",
            category: ScenarioCategory::Injection,
            taxonomy: &["AGT"],
            tags: &[
                "agentic",
                "integrity",
                "persistence",
                "memory-poisoning",
                "multi-turn",
            ],
            features: &["sequence"],
            yaml: include_str!("../../scenarios/context-persistence.yaml"),
        },
        BuiltinScenario {
            name: "markdown-beacon",
            description: "Tracking pixels via Markdown images, HTML img tags, and CSS references",
            category: ScenarioCategory::Injection,
            taxonomy: &["PRV"],
            tags: &[
                "privacy",
                "tracking",
                "beacon",
                "markdown",
                "rendering",
                "tier-2",
            ],
            features: &["template-interpolation"],
            yaml: include_str!("../../scenarios/markdown-beacon.yaml"),
        },
        // ── Resource (order 3) ──────────────────────────────────
        BuiltinScenario {
            name: "resource-exfiltration",
            description: "Fake credentials and injection payloads for sensitive file paths",
            category: ScenarioCategory::Resource,
            taxonomy: &["RSC-001"],
            tags: &[
                "integrity",
                "confidentiality",
                "resource",
                "credential-theft",
            ],
            features: &["resources"],
            yaml: include_str!("../../scenarios/resource-exfiltration.yaml"),
        },
        // ── DoS (order 4) ───────────────────────────────────────
        BuiltinScenario {
            name: "slow-loris",
            description: "Byte-by-byte response delivery with configurable delay",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-004"],
            tags: &["dos", "availability", "timeout", "streaming", "tier-1"],
            features: &["slow-loris-delivery"],
            yaml: include_str!("../../scenarios/slow-loris.yaml"),
        },
        BuiltinScenario {
            name: "nested-json-dos",
            description: "50,000-level deep JSON payloads for parser stack exhaustion",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-001"],
            tags: &["dos", "availability", "parser", "stack-overflow", "tier-1"],
            features: &["generate-nested-json"],
            yaml: include_str!("../../scenarios/nested-json-dos.yaml"),
        },
        BuiltinScenario {
            name: "notification-flood",
            description: "Server-initiated notification flood at 10,000/sec on tool calls",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-006"],
            tags: &[
                "dos",
                "availability",
                "notification",
                "rate-limiting",
                "side-effect",
            ],
            features: &["notification-flood", "side-effects"],
            yaml: include_str!("../../scenarios/notification-flood.yaml"),
        },
        BuiltinScenario {
            name: "pipe-deadlock",
            description: "Stdio pipe deadlock by filling OS buffers without reading",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM-005"],
            tags: &["dos", "availability", "stdio", "transport", "deadlock"],
            features: &["unbounded-line", "pipe-deadlock", "side-effects"],
            yaml: include_str!("../../scenarios/pipe-deadlock.yaml"),
        },
        BuiltinScenario {
            name: "token-flush",
            description: "500KB+ garbage payload to flush LLM context window",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM"],
            tags: &["dos", "cognitive", "context-window", "llm", "tier-2"],
            features: &["generate-garbage"],
            yaml: include_str!("../../scenarios/token-flush.yaml"),
        },
        BuiltinScenario {
            name: "zombie-process",
            description: "Ignores cancellation and continues slow-dripping responses",
            category: ScenarioCategory::DoS,
            taxonomy: &["TAM"],
            tags: &[
                "dos",
                "availability",
                "cancellation",
                "resource-leak",
                "tier-3",
            ],
            features: &["slow-loris-delivery", "generate-garbage"],
            yaml: include_str!("../../scenarios/zombie-process.yaml"),
        },
        // ── Protocol (order 5) ──────────────────────────────────
        BuiltinScenario {
            name: "id-collision",
            description: "Request ID collision via forced sampling/createMessage IDs",
            category: ScenarioCategory::Protocol,
            taxonomy: &["PRT-002"],
            tags: &["protocol", "state-corruption", "sampling", "phased"],
            features: &[
                "phases",
                "send-request",
                "duplicate-request-ids",
                "side-effects",
            ],
            yaml: include_str!("../../scenarios/id-collision.yaml"),
        },
        BuiltinScenario {
            name: "batch-amplification",
            description: "Single request triggers 10,000 JSON-RPC notification batch",
            category: ScenarioCategory::Protocol,
            taxonomy: &["TAM-002"],
            tags: &["dos", "availability", "batch", "amplification", "protocol"],
            features: &[
                "generate-batch-notifications",
                "batch-amplify",
                "side-effects",
            ],
            yaml: include_str!("../../scenarios/batch-amplification.yaml"),
        },
        // ── Multi-Vector (order 6) ──────────────────────────────
        BuiltinScenario {
            name: "multi-vector-attack",
            description: "Four-phase compound attack across tools, resources, and prompts",
            category: ScenarioCategory::MultiVector,
            taxonomy: &["CPM", "RSC", "PRM"],
            tags: &[
                "compound",
                "phased",
                "multi-vector",
                "defense-in-depth",
                "tier-3",
            ],
            features: &[
                "phases",
                "replace-tools",
                "replace-resources",
                "replace-prompts",
                "slow-loris-delivery",
            ],
            yaml: include_str!("../../scenarios/multi-vector-attack.yaml"),
        },
        BuiltinScenario {
            name: "cross-server-pivot",
            description: "Confused deputy attack pivoting through a benign weather tool",
            category: ScenarioCategory::MultiVector,
            taxonomy: &["AGT"],
            tags: &[
                "agentic",
                "lateral-movement",
                "confused-deputy",
                "cross-server",
                "tier-2",
            ],
            features: &["template-interpolation"],
            yaml: include_str!("../../scenarios/cross-server-pivot.yaml"),
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
    fn list_filter_protocol() {
        let result = list_scenarios(Some(ScenarioCategory::Protocol), None);
        assert!(result.len() >= 2, "Expected at least 2 protocol scenarios");
        for s in &result {
            assert_eq!(s.category, ScenarioCategory::Protocol);
        }
    }

    #[test]
    fn list_filter_multi_vector() {
        let result = list_scenarios(Some(ScenarioCategory::MultiVector), None);
        assert!(
            result.len() >= 2,
            "Expected at least 2 multi-vector scenarios"
        );
        for s in &result {
            assert_eq!(s.category, ScenarioCategory::MultiVector);
        }
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
        assert_eq!(names.len(), 24, "Expected exactly 24 built-in scenarios");
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
