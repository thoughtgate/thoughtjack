//! MCP attack surface coverage page generation.
//!
//! Generates an MDX page showing coverage of attack vectors and
//! behavioral primitives from the `ThoughtJack`-native classification.
//!
//! TJ-SPEC-011 F-005

use super::{CoverageMatrix, CoverageStatus, ScenarioSummary, format_scenario_links, status_badge};
use crate::config::schema::{AttackPrimitive, AttackVector};

/// All known attack vectors in display order.
const ALL_VECTORS: &[(AttackVector, &str)] = &[
    (AttackVector::ToolInjection, "Tool Injection"),
    (AttackVector::ResourceInjection, "Resource Injection"),
    (AttackVector::PromptPoisoning, "Prompt Poisoning"),
    (AttackVector::CapabilityMutation, "Capability Mutation"),
    (AttackVector::NotificationAbuse, "Notification Abuse"),
    (AttackVector::SchemaManipulation, "Schema Manipulation"),
    (AttackVector::DescriptionHijack, "Description Hijack"),
    (AttackVector::ResponseDelay, "Response Delay"),
    (AttackVector::ConnectionAbuse, "Connection Abuse"),
];

/// All known attack primitives in display order.
const ALL_PRIMITIVES: &[(AttackPrimitive, &str)] = &[
    (AttackPrimitive::RugPull, "Rug Pull"),
    (AttackPrimitive::Sleeper, "Sleeper"),
    (AttackPrimitive::SlowLoris, "Slow Loris"),
    (AttackPrimitive::Flood, "Flood"),
    (AttackPrimitive::NestedJson, "Nested JSON"),
    (AttackPrimitive::UnboundedLine, "Unbounded Line"),
    (AttackPrimitive::Fuzzing, "Fuzzing"),
    (AttackPrimitive::IdCollision, "ID Collision"),
    (AttackPrimitive::TimeBomb, "Time Bomb"),
];

/// Generate MCP attack surface coverage MDX page.
///
/// Implements: TJ-SPEC-011 F-005
#[must_use]
pub fn generate_mcp_surface_coverage(matrix: &CoverageMatrix) -> String {
    let mut lines = vec![
        "---".to_string(),
        "id: mcp-attack-surface".to_string(),
        "title: MCP Attack Surface Coverage".to_string(),
        "sidebar_label: Attack Surface".to_string(),
        "---".to_string(),
        String::new(),
        "<!-- AUTO-GENERATED — DO NOT EDIT -->".to_string(),
        String::new(),
        "# MCP Attack Surface Coverage".to_string(),
        String::new(),
        "ThoughtJack-native classification of attack vectors and behavioral primitives."
            .to_string(),
        String::new(),
        "## Attack Vectors".to_string(),
        String::new(),
        "| Vector | Status | Scenarios |".to_string(),
        "|--------|--------|-----------|".to_string(),
    ];

    for (vector, name) in ALL_VECTORS {
        let (status, scenarios) = vector_status(matrix, *vector);
        lines.push(format!(
            "| {} | {} | {} |",
            name,
            status_badge(status),
            format_scenario_links(&scenarios),
        ));
    }

    lines.push(String::new());

    // Primitives table
    lines.push("## Behavioral Primitives".to_string());
    lines.push(String::new());
    lines.push("| Primitive | Status | Scenarios |".to_string());
    lines.push("|-----------|--------|-----------|".to_string());

    for (primitive, name) in ALL_PRIMITIVES {
        let (status, scenarios) = primitive_status(matrix, *primitive);
        lines.push(format!(
            "| {} | {} | {} |",
            name,
            status_badge(status),
            format_scenario_links(&scenarios),
        ));
    }

    lines.push(String::new());

    // Summary
    let vector_covered = ALL_VECTORS
        .iter()
        .filter(|(v, _)| matrix.attack_vectors.contains_key(v))
        .count();
    let primitive_covered = ALL_PRIMITIVES
        .iter()
        .filter(|(p, _)| matrix.attack_primitives.contains_key(p))
        .count();

    lines.push("## Summary".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- **Vectors:** {vector_covered}/{} covered",
        ALL_VECTORS.len()
    ));
    lines.push(format!(
        "- **Primitives:** {primitive_covered}/{} covered",
        ALL_PRIMITIVES.len()
    ));
    lines.push(String::new());

    lines.join("\n")
}

/// Determine coverage status for an attack vector.
fn vector_status(
    matrix: &CoverageMatrix,
    vector: AttackVector,
) -> (CoverageStatus, Vec<ScenarioSummary>) {
    matrix.attack_vectors.get(&vector).map_or_else(
        || (CoverageStatus::Gap, Vec::new()),
        |scenarios| (CoverageStatus::Covered, scenarios.clone()),
    )
}

/// Determine coverage status for a behavioral primitive.
fn primitive_status(
    matrix: &CoverageMatrix,
    primitive: AttackPrimitive,
) -> (CoverageStatus, Vec<ScenarioSummary>) {
    matrix.attack_primitives.get(&primitive).map_or_else(
        || (CoverageStatus::Gap, Vec::new()),
        |scenarios| (CoverageStatus::Covered, scenarios.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docgen::coverage::build_coverage_matrix;

    #[test]
    fn test_generate_surface_page() {
        let configs = crate::docgen::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let page = generate_mcp_surface_coverage(&matrix);

        assert!(page.contains("MCP Attack Surface Coverage"));
        assert!(page.contains("Tool Injection"));
        assert!(page.contains("Rug Pull"));
        assert!(page.contains("Covered"));
        assert!(page.contains("**Gap**"));
    }

    #[test]
    fn test_empty_matrix_all_gaps() {
        let matrix = CoverageMatrix::default();
        let page = generate_mcp_surface_coverage(&matrix);
        assert!(!page.contains("Covered"));
        assert!(page.contains("0/9 covered"));
    }

    #[test]
    fn test_summary_counts() {
        let configs = crate::docgen::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let page = generate_mcp_surface_coverage(&matrix);

        // 3 vectors covered: tool_injection, capability_mutation, response_delay
        assert!(page.contains("3/9 covered"));
        // 2 primitives covered: rug_pull, slow_loris
        assert!(page.contains("2/9 covered"));
    }
}
