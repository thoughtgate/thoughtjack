//! MITRE ATT&CK coverage page generation.
//!
//! Generates an MDX page showing tactic → technique → scenario coverage.
//!
//! TJ-SPEC-011 F-005

use super::{CoverageMatrix, CoverageStatus, ScenarioSummary, format_scenario_links, status_badge};

/// Known MITRE ATT&CK tactics relevant to MCP attacks.
const KNOWN_TACTICS: &[(&str, &str)] = &[
    ("TA0001", "Initial Access"),
    ("TA0002", "Execution"),
    ("TA0003", "Persistence"),
    ("TA0005", "Defense Evasion"),
    ("TA0006", "Credential Access"),
    ("TA0009", "Collection"),
    ("TA0010", "Exfiltration"),
    ("TA0011", "Command and Control"),
    ("TA0040", "Impact"),
];

/// Generate MITRE ATT&CK coverage MDX page.
///
/// Implements: TJ-SPEC-011 F-005
#[must_use]
pub fn generate_mitre_coverage(matrix: &CoverageMatrix) -> String {
    let mut lines = vec![
        "---".to_string(),
        "id: mitre-matrix".to_string(),
        "title: MITRE ATT&CK Coverage".to_string(),
        "sidebar_label: MITRE ATT&CK".to_string(),
        "---".to_string(),
        String::new(),
        "<!-- AUTO-GENERATED — DO NOT EDIT -->".to_string(),
        String::new(),
        "# MITRE ATT&CK Coverage Matrix".to_string(),
        String::new(),
        "Coverage of MITRE ATT&CK tactics and techniques by ThoughtJack scenarios.".to_string(),
        String::new(),
        "## Tactics".to_string(),
        String::new(),
        "| Tactic | Status | Scenarios |".to_string(),
        "|--------|--------|-----------|".to_string(),
    ];

    for (id, name) in KNOWN_TACTICS {
        let (status, scenarios) = tactic_status(matrix, id);
        lines.push(format!(
            "| {} {} | {} | {} |",
            id,
            name,
            status_badge(status),
            format_scenario_links(&scenarios),
        ));
    }

    lines.push(String::new());

    // Techniques table
    lines.push("## Techniques".to_string());
    lines.push(String::new());

    if matrix.mitre_techniques.is_empty() {
        lines.push("No techniques mapped yet.".to_string());
    } else {
        lines.push("| Technique | Scenarios |".to_string());
        lines.push("|-----------|-----------|".to_string());

        let mut technique_ids: Vec<&String> = matrix.mitre_techniques.keys().collect();
        technique_ids.sort();

        for tech_id in technique_ids {
            let scenarios = &matrix.mitre_techniques[tech_id];
            lines.push(format!(
                "| {} | {} |",
                tech_id,
                format_scenario_links(scenarios),
            ));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

/// Determine the coverage status and scenarios for a tactic.
fn tactic_status(
    matrix: &CoverageMatrix,
    tactic_id: &str,
) -> (CoverageStatus, Vec<ScenarioSummary>) {
    matrix.mitre_tactics.get(tactic_id).map_or_else(
        || (CoverageStatus::Gap, Vec::new()),
        |scenarios| (CoverageStatus::Covered, scenarios.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docgen::coverage::build_coverage_matrix;

    #[test]
    fn test_generate_mitre_page() {
        let configs = crate::docgen::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let page = generate_mitre_coverage(&matrix);

        assert!(page.contains("MITRE ATT&CK Coverage"));
        assert!(page.contains("TA0001"));
        assert!(page.contains("Covered"));
        assert!(page.contains("**Gap**"));
    }

    #[test]
    fn test_empty_matrix() {
        let matrix = CoverageMatrix::default();
        let page = generate_mitre_coverage(&matrix);
        assert!(page.contains("**Gap**"));
        assert!(page.contains("No techniques mapped"));
    }

    #[test]
    fn test_format_scenario_links_empty() {
        assert_eq!(format_scenario_links(&[]), "\u{2014}");
    }

    #[test]
    fn test_format_scenario_links_single() {
        let scenarios = vec![ScenarioSummary {
            id: "TJ-ATK-001".to_string(),
            name: "Test".to_string(),
        }];
        let links = format_scenario_links(&scenarios);
        assert!(links.contains("TJ-ATK-001"));
        assert!(links.contains("tj-atk-001"));
    }
}
