//! OWASP MCP Top 10 coverage page generation.
//!
//! Generates an MDX page showing coverage of each OWASP MCP risk category.
//! Certain categories (MCP04, MCP07, MCP08, MCP09) are marked out-of-scope
//! as they address server-side or infrastructure concerns outside
//! `ThoughtJack`'s attack simulation domain.
//!
//! TJ-SPEC-011 F-005

use super::{CoverageMatrix, CoverageStatus, ScenarioSummary, format_scenario_links, status_badge};

/// All OWASP MCP Top 10 risk categories with scope status.
///
/// `true` = in scope for `ThoughtJack`, `false` = out of scope.
const OWASP_MCP_CATEGORIES: &[(&str, &str, bool)] = &[
    ("MCP01", "Prompt Injection via Tool Results", true),
    ("MCP02", "Excessive Tool Permissions", true),
    ("MCP03", "Tool Poisoning", true),
    ("MCP04", "Server-Side Request Forgery via MCP", false),
    ("MCP05", "Tool Argument Injection", true),
    ("MCP06", "Indirect Prompt Injection", true),
    ("MCP07", "Insecure MCP Server Configuration", false),
    ("MCP08", "Lack of Resource Access Control", false),
    ("MCP09", "Insecure Data Storage by MCP Servers", false),
    ("MCP10", "Insufficient Logging and Monitoring", true),
];

/// Generate OWASP MCP Top 10 coverage MDX page.
///
/// `scope_exclusions` can override the default in-scope/out-of-scope status.
///
/// Implements: TJ-SPEC-011 F-005
#[must_use]
pub fn generate_owasp_mcp_coverage(matrix: &CoverageMatrix, scope_exclusions: &[&str]) -> String {
    let mut lines = vec![
        "---".to_string(),
        "id: owasp-mcp".to_string(),
        "title: OWASP MCP Top 10 Coverage".to_string(),
        "sidebar_label: OWASP MCP".to_string(),
        "---".to_string(),
        String::new(),
        "<!-- AUTO-GENERATED — DO NOT EDIT -->".to_string(),
        String::new(),
        "# OWASP MCP Top 10 Coverage".to_string(),
        String::new(),
        "Coverage of OWASP MCP Top 10 risk categories by ThoughtJack scenarios.".to_string(),
        String::new(),
        "| Risk | Name | Status | Scenarios |".to_string(),
        "|------|------|--------|-----------|".to_string(),
    ];

    for (id, name, default_in_scope) in OWASP_MCP_CATEGORIES {
        let excluded = scope_exclusions.contains(id);
        let in_scope = if excluded { false } else { *default_in_scope };

        let (status, scenarios) = if in_scope {
            category_status(matrix, id)
        } else {
            (CoverageStatus::OutOfScope, Vec::new())
        };

        lines.push(format!(
            "| {} | {} | {} | {} |",
            id,
            name,
            status_badge(status),
            format_scenario_links(&scenarios),
        ));
    }

    lines.push(String::new());

    // Summary
    let (covered, gaps, out_of_scope) = summary_counts(matrix, scope_exclusions);
    lines.push("## Summary".to_string());
    lines.push(String::new());
    lines.push(format!("- **Covered:** {covered}"));
    lines.push(format!("- **Gaps:** {gaps}"));
    lines.push(format!("- **Out of scope:** {out_of_scope}"));
    lines.push(String::new());

    lines.join("\n")
}

/// Determine coverage status for an OWASP MCP category.
fn category_status(
    matrix: &CoverageMatrix,
    category_id: &str,
) -> (CoverageStatus, Vec<ScenarioSummary>) {
    matrix.owasp_mcp.get(category_id).map_or_else(
        || (CoverageStatus::Gap, Vec::new()),
        |scenarios| (CoverageStatus::Covered, scenarios.clone()),
    )
}

/// Calculate summary counts.
fn summary_counts(matrix: &CoverageMatrix, scope_exclusions: &[&str]) -> (usize, usize, usize) {
    let mut covered = 0;
    let mut gaps = 0;
    let mut out_of_scope = 0;

    for (id, _, default_in_scope) in OWASP_MCP_CATEGORIES {
        let excluded = scope_exclusions.contains(id);
        let in_scope = if excluded { false } else { *default_in_scope };

        if !in_scope {
            out_of_scope += 1;
        } else if matrix.owasp_mcp.contains_key(*id) {
            covered += 1;
        } else {
            gaps += 1;
        }
    }

    (covered, gaps, out_of_scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::build_coverage_matrix;

    #[test]
    fn test_generate_owasp_page() {
        let configs = crate::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let page = generate_owasp_mcp_coverage(&matrix, &[]);

        assert!(page.contains("OWASP MCP Top 10"));
        assert!(page.contains("MCP03"));
        assert!(page.contains("Covered"));
        assert!(page.contains("Out of scope"));
    }

    #[test]
    fn test_scope_exclusions() {
        let configs = crate::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let page = generate_owasp_mcp_coverage(&matrix, &["MCP03"]);

        assert!(page.contains("Out of scope"));
    }

    #[test]
    fn test_summary_counts() {
        let configs = crate::coverage::tests::test_configs_from_yaml();
        let matrix = build_coverage_matrix(&configs);
        let (covered, gaps, out_of_scope) = summary_counts(&matrix, &[]);

        // MCP03 and MCP06 are covered, MCP04/07/08/09 out of scope,
        // MCP01/02/05/10 are gaps
        assert_eq!(covered, 2);
        assert_eq!(gaps, 4);
        assert_eq!(out_of_scope, 4);
    }

    #[test]
    fn test_empty_matrix() {
        let matrix = CoverageMatrix::default();
        let page = generate_owasp_mcp_coverage(&matrix, &[]);
        assert!(page.contains("**Gap**"));
    }
}
