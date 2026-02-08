//! Coverage matrix generation from scenario metadata.
//!
//! Aggregates MITRE ATT&CK, OWASP MCP Top 10, and MCP attack surface
//! coverage across all scenarios to produce MDX coverage pages.
//!
//! TJ-SPEC-011 F-005

pub mod mcp_surface;
pub mod mitre;
pub mod owasp_mcp;

use std::collections::HashMap;
use thoughtjack_core::config::schema::{
    AttackPrimitive, AttackVector, ScenarioMetadata, ServerConfig,
};

/// Coverage status for a framework category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    /// At least one scenario covers this category.
    Covered,
    /// In scope but no scenario exists yet.
    Gap,
    /// Not in scope for this project.
    OutOfScope,
}

/// A single scenario's summary for coverage tracking.
#[derive(Debug, Clone)]
pub struct ScenarioSummary {
    /// Scenario metadata ID (e.g., `TJ-ATK-001`).
    pub id: String,
    /// Scenario name.
    pub name: String,
}

/// Aggregated coverage data across all scenarios.
///
/// Implements: TJ-SPEC-011 F-005
#[derive(Debug, Default)]
pub struct CoverageMatrix {
    /// MITRE tactic ID → list of covering scenarios.
    pub mitre_tactics: HashMap<String, Vec<ScenarioSummary>>,

    /// MITRE technique ID → list of covering scenarios.
    pub mitre_techniques: HashMap<String, Vec<ScenarioSummary>>,

    /// OWASP MCP risk ID → list of covering scenarios.
    pub owasp_mcp: HashMap<String, Vec<ScenarioSummary>>,

    /// Attack vector → list of covering scenarios.
    pub attack_vectors: HashMap<AttackVector, Vec<ScenarioSummary>>,

    /// Attack primitive → list of covering scenarios.
    pub attack_primitives: HashMap<AttackPrimitive, Vec<ScenarioSummary>>,
}

/// Build a coverage matrix from a collection of server configs.
///
/// Only scenarios with a `metadata` block are included.
///
/// Implements: TJ-SPEC-011 F-005
#[must_use]
pub fn build_coverage_matrix(configs: &[ServerConfig]) -> CoverageMatrix {
    let mut matrix = CoverageMatrix::default();

    for config in configs {
        let Some(ref metadata) = config.metadata else {
            continue;
        };

        let summary = ScenarioSummary {
            id: metadata.id.clone(),
            name: metadata.name.clone(),
        };

        aggregate_mitre(&mut matrix, metadata, &summary);
        aggregate_owasp(&mut matrix, metadata, &summary);
        aggregate_surface(&mut matrix, metadata, &summary);
    }

    matrix
}

/// Aggregate MITRE ATT&CK mappings from a single scenario.
fn aggregate_mitre(
    matrix: &mut CoverageMatrix,
    metadata: &ScenarioMetadata,
    summary: &ScenarioSummary,
) {
    if let Some(ref mitre) = metadata.mitre_attack {
        for tactic in &mitre.tactics {
            matrix
                .mitre_tactics
                .entry(tactic.id.clone())
                .or_default()
                .push(summary.clone());
        }
        for technique in &mitre.techniques {
            matrix
                .mitre_techniques
                .entry(technique.id.clone())
                .or_default()
                .push(summary.clone());
        }
    }
}

/// Aggregate OWASP MCP mappings from a single scenario.
fn aggregate_owasp(
    matrix: &mut CoverageMatrix,
    metadata: &ScenarioMetadata,
    summary: &ScenarioSummary,
) {
    if let Some(ref owasp) = metadata.owasp_mcp {
        for entry in owasp {
            matrix
                .owasp_mcp
                .entry(entry.id.clone())
                .or_default()
                .push(summary.clone());
        }
    }
}

/// Aggregate MCP attack surface mappings from a single scenario.
fn aggregate_surface(
    matrix: &mut CoverageMatrix,
    metadata: &ScenarioMetadata,
    summary: &ScenarioSummary,
) {
    for vector in &metadata.mcp_attack_surface.vectors {
        matrix
            .attack_vectors
            .entry(*vector)
            .or_default()
            .push(summary.clone());
    }
    for primitive in &metadata.mcp_attack_surface.primitives {
        matrix
            .attack_primitives
            .entry(*primitive)
            .or_default()
            .push(summary.clone());
    }
}

/// Format a coverage status as a badge string.
const fn status_badge(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Covered => "Covered",
        CoverageStatus::Gap => "**Gap**",
        CoverageStatus::OutOfScope => "Out of scope",
    }
}

/// Format scenario summaries as markdown links.
fn format_scenario_links(scenarios: &[ScenarioSummary]) -> String {
    if scenarios.is_empty() {
        return "\u{2014}".to_string();
    }
    scenarios
        .iter()
        .map(|s| format!("[{}](../scenarios/{})", s.id, s.id.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Shared test fixture: two scenarios with overlapping metadata.
    pub(crate) fn test_configs_from_yaml() -> Vec<ServerConfig> {
        let yaml1 = r#"
server:
  name: test1
metadata:
  id: TJ-ATK-001
  name: Rug Pull
  description: Trust-then-betray
  severity: high
  mitre_attack:
    tactics:
      - id: TA0001
        name: Initial Access
    techniques:
      - id: T1195.002
        name: Supply Chain Compromise
  owasp_mcp:
    - id: MCP03
      name: Tool Poisoning
  mcp_attack_surface:
    vectors:
      - tool_injection
      - capability_mutation
    primitives:
      - rug_pull
  tags: [temporal]
baseline:
  tools: []
"#;

        let yaml2 = r#"
server:
  name: test2
metadata:
  id: TJ-ATK-002
  name: Slow Loris
  description: Byte-at-a-time delivery
  severity: medium
  mitre_attack:
    tactics:
      - id: TA0001
        name: Initial Access
    techniques:
      - id: T1499.001
        name: Endpoint DoS
  owasp_mcp:
    - id: MCP03
      name: Tool Poisoning
    - id: MCP06
      name: Indirect Prompt Injection
  mcp_attack_surface:
    vectors:
      - response_delay
    primitives:
      - slow_loris
  tags: [dos]
baseline:
  tools: []
"#;

        vec![
            serde_yaml::from_str(yaml1).unwrap(),
            serde_yaml::from_str(yaml2).unwrap(),
        ]
    }

    #[test]
    fn test_build_coverage_matrix() {
        let configs = test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);

        // TA0001 referenced by both
        assert_eq!(matrix.mitre_tactics["TA0001"].len(), 2);

        // T1195.002 only in first
        assert_eq!(matrix.mitre_techniques["T1195.002"].len(), 1);

        // MCP03 referenced by both
        assert_eq!(matrix.owasp_mcp["MCP03"].len(), 2);

        // MCP06 only in second
        assert_eq!(matrix.owasp_mcp["MCP06"].len(), 1);

        // Vectors
        assert_eq!(matrix.attack_vectors[&AttackVector::ToolInjection].len(), 1);
        assert_eq!(matrix.attack_vectors[&AttackVector::ResponseDelay].len(), 1);

        // Primitives
        assert_eq!(matrix.attack_primitives[&AttackPrimitive::RugPull].len(), 1);
        assert_eq!(
            matrix.attack_primitives[&AttackPrimitive::SlowLoris].len(),
            1
        );
    }

    #[test]
    fn test_empty_configs() {
        let matrix = build_coverage_matrix(&[]);
        assert!(matrix.mitre_tactics.is_empty());
        assert!(matrix.owasp_mcp.is_empty());
    }

    #[test]
    fn test_config_without_metadata_skipped() {
        let yaml = r#"
server:
  name: no-metadata
tools:
  - tool:
      name: echo
      description: Echo tool
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: hello
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let matrix = build_coverage_matrix(&[config]);
        assert!(matrix.mitre_tactics.is_empty());
    }
}
