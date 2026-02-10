//! Scenario index page generation.
//!
//! Generates `scenarios/index.mdx` — the top-level catalog page that
//! lists every scenario grouped by registry category.
//!
//! TJ-SPEC-011 F-003

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use crate::config::schema::{MetadataSeverity, ServerConfig};
use crate::docgen::registry::Registry;

/// Generate the scenario index MDX page.
///
/// Produces frontmatter, a description paragraph, one table per
/// registry category, a Quick Start section, and coverage links.
///
/// `id_map` maps scenario file paths (relative to the scenarios base
/// directory) to their lowercase metadata IDs — the same mapping the
/// sidebar generator uses.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn generate_index_page<S: BuildHasher>(
    registry: &Registry,
    configs: &[(String, ServerConfig)],
    base: &Path,
    id_map: &HashMap<PathBuf, String, S>,
) -> String {
    let mut sections = Vec::new();

    // Frontmatter must be first in MDX files
    sections.push("---".to_string());
    sections.push("sidebar_position: 0".to_string());
    sections.push(format!("title: {}", registry.site.title));
    sections.push(format!("description: {}", registry.site.description));
    sections.push("---".to_string());
    sections.push(String::new());

    // Header comment
    sections.push("<!-- AUTO-GENERATED — DO NOT EDIT -->".to_string());
    sections.push(String::new());

    // Title and description
    sections.push(format!("# {}", registry.site.title));
    sections.push(String::new());
    sections.push(format!(
        "ThoughtJack ships with **{}** attack scenarios that simulate real-world MCP security threats. \
         Each scenario is a self-contained YAML configuration that can be run directly with \
         `thoughtjack server run --scenario <name>`.",
        count_scenarios(registry)
    ));
    sections.push(String::new());

    // Category tables
    sections.push("## Categories".to_string());
    sections.push(String::new());

    for category in &registry.categories {
        sections.push(format!("### {}", category.name));
        sections.push(String::new());
        if let Some(ref desc) = category.description {
            sections.push(desc.clone());
            sections.push(String::new());
        }

        sections.push("| ID | Scenario | Severity |".to_string());
        sections.push("|----|----------|----------|".to_string());

        for scenario_path in &category.scenarios {
            let row = build_table_row(scenario_path, configs, base, id_map);
            sections.push(row);
        }

        sections.push(String::new());
    }

    // Quick Start
    sections.push("## Quick Start".to_string());
    sections.push(String::new());
    sections.push("```bash".to_string());
    sections.push("# List all scenarios".to_string());
    sections.push("thoughtjack scenarios list".to_string());
    sections.push(String::new());
    sections.push("# View a scenario's YAML".to_string());
    sections.push("thoughtjack scenarios show rug-pull".to_string());
    sections.push(String::new());
    sections.push("# Run a scenario".to_string());
    sections.push("thoughtjack server run --scenario rug-pull".to_string());
    sections.push("```".to_string());
    sections.push(String::new());

    // Coverage links
    sections.push("## Coverage".to_string());
    sections.push(String::new());
    sections.push("See how scenarios map to security frameworks:".to_string());
    sections.push(String::new());
    sections.push("- [MITRE ATT&CK Matrix](/docs/coverage/mitre-matrix)".to_string());
    sections.push("- [OWASP MCP Top 10](/docs/coverage/owasp-mcp)".to_string());
    sections.push("- [MCP Attack Surface](/docs/coverage/mcp-attack-surface)".to_string());

    sections.join("\n")
}

/// Count total scenarios across all categories.
fn count_scenarios(registry: &Registry) -> usize {
    registry.categories.iter().map(|c| c.scenarios.len()).sum()
}

/// Build a markdown table row for a single scenario.
fn build_table_row<S: BuildHasher>(
    scenario_path: &Path,
    configs: &[(String, ServerConfig)],
    base: &Path,
    id_map: &HashMap<PathBuf, String, S>,
) -> String {
    let full_path = base.join(scenario_path);
    let full_str = full_path.display().to_string();

    // Find the matching config
    let config = configs.iter().find(|(p, _)| *p == full_str).map(|(_, c)| c);

    let doc_id = id_map
        .get(scenario_path)
        .cloned()
        .unwrap_or_else(|| fallback_doc_id(scenario_path));

    config.and_then(|c| c.metadata.as_ref()).map_or_else(
        || {
            let name = scenario_path.file_stem().map_or_else(
                || scenario_path.to_string_lossy().to_string(),
                |s| s.to_string_lossy().to_string(),
            );
            format!("| [{doc_id}](/docs/scenarios/{doc_id}) | {name} | — |")
        },
        |metadata| {
            let id_upper = metadata.id.to_uppercase();
            let name = &metadata.name;
            let severity = severity_label(metadata.severity);
            format!("| [{id_upper}](/docs/scenarios/{doc_id}) | {name} | {severity} |")
        },
    )
}

/// Fallback doc ID from a scenario path.
fn fallback_doc_id(path: &Path) -> String {
    path.file_stem().map_or_else(
        || path.to_string_lossy().to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

/// Convert severity to a display label.
const fn severity_label(severity: MetadataSeverity) -> &'static str {
    match severity {
        MetadataSeverity::Low => "Low",
        MetadataSeverity::Medium => "Medium",
        MetadataSeverity::High => "High",
        MetadataSeverity::Critical => "Critical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::McpAttackSurface;
    use crate::docgen::registry::{Category, SiteConfig};

    fn test_registry() -> Registry {
        Registry {
            site: SiteConfig {
                title: "ThoughtJack Attack Catalog".to_string(),
                description: "Reference catalog of MCP attack scenarios".to_string(),
                base_url: None,
            },
            categories: vec![
                Category {
                    id: "temporal".to_string(),
                    name: "Temporal Attacks".to_string(),
                    description: Some("Trust-building and time-based attacks".to_string()),
                    order: Some(1),
                    scenarios: vec![PathBuf::from("scenarios/rug-pull.yaml")],
                },
                Category {
                    id: "injection".to_string(),
                    name: "Injection Attacks".to_string(),
                    description: None,
                    order: Some(2),
                    scenarios: vec![PathBuf::from("scenarios/prompt-injection.yaml")],
                },
            ],
        }
    }

    fn test_metadata(
        id: &str,
        name: &str,
        severity: MetadataSeverity,
    ) -> crate::config::schema::ScenarioMetadata {
        crate::config::schema::ScenarioMetadata {
            id: id.to_string(),
            name: name.to_string(),
            description: "Test scenario".to_string(),
            author: None,
            created: None,
            updated: None,
            severity,
            mitre_attack: None,
            owasp_mcp: None,
            owasp_agentic: None,
            mcp_attack_surface: McpAttackSurface {
                vectors: vec![],
                primitives: vec![],
            },
            tags: vec![],
            detection_guidance: vec![],
            references: vec![],
        }
    }

    fn test_configs(base: &Path) -> Vec<(String, ServerConfig)> {
        vec![
            (
                base.join("scenarios/rug-pull.yaml").display().to_string(),
                ServerConfig {
                    metadata: Some(test_metadata(
                        "TJ-ATK-001",
                        "Classic Rug Pull",
                        MetadataSeverity::High,
                    )),
                    ..minimal_server_config()
                },
            ),
            (
                base.join("scenarios/prompt-injection.yaml")
                    .display()
                    .to_string(),
                ServerConfig {
                    metadata: Some(test_metadata(
                        "TJ-ATK-004",
                        "Conditional Prompt Injection",
                        MetadataSeverity::High,
                    )),
                    ..minimal_server_config()
                },
            ),
        ]
    }

    fn minimal_server_config() -> ServerConfig {
        let yaml = r"
server:
  name: test
tools: []
";
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_index_page_structure() {
        let reg = test_registry();
        let base = Path::new("/tmp/test");
        let configs = test_configs(base);
        let mut id_map = HashMap::new();
        id_map.insert(
            PathBuf::from("scenarios/rug-pull.yaml"),
            "tj-atk-001".to_string(),
        );
        id_map.insert(
            PathBuf::from("scenarios/prompt-injection.yaml"),
            "tj-atk-004".to_string(),
        );

        let page = generate_index_page(&reg, &configs, base, &id_map);

        assert!(
            page.starts_with("---"),
            "page should start with frontmatter"
        );
        assert!(page.contains("<!-- AUTO-GENERATED"));
        assert!(page.contains("sidebar_position: 0"));
        assert!(page.contains("title: ThoughtJack Attack Catalog"));
        assert!(page.contains("# ThoughtJack Attack Catalog"));
        assert!(page.contains("**2** attack scenarios"));
        assert!(page.contains("## Categories"));
        assert!(page.contains("### Temporal Attacks"));
        assert!(page.contains("### Injection Attacks"));
        assert!(page.contains("## Quick Start"));
        assert!(page.contains("## Coverage"));
    }

    #[test]
    fn test_index_page_table_rows() {
        let reg = test_registry();
        let base = Path::new("/tmp/test");
        let configs = test_configs(base);
        let mut id_map = HashMap::new();
        id_map.insert(
            PathBuf::from("scenarios/rug-pull.yaml"),
            "tj-atk-001".to_string(),
        );

        let page = generate_index_page(&reg, &configs, base, &id_map);

        assert!(page.contains("[TJ-ATK-001](/docs/scenarios/tj-atk-001)"));
        assert!(page.contains("Classic Rug Pull"));
        assert!(page.contains("| High |"));
    }

    #[test]
    fn test_index_page_category_description() {
        let reg = test_registry();
        let base = Path::new("/tmp/test");
        let configs = test_configs(base);

        let page = generate_index_page(&reg, &configs, base, &HashMap::new());

        assert!(page.contains("Trust-building and time-based attacks"));
    }

    #[test]
    fn test_count_scenarios() {
        let reg = test_registry();
        assert_eq!(count_scenarios(&reg), 2);
    }

    #[test]
    fn test_severity_label() {
        assert_eq!(severity_label(MetadataSeverity::Low), "Low");
        assert_eq!(severity_label(MetadataSeverity::Medium), "Medium");
        assert_eq!(severity_label(MetadataSeverity::High), "High");
        assert_eq!(severity_label(MetadataSeverity::Critical), "Critical");
    }

    #[test]
    fn test_fallback_doc_id() {
        assert_eq!(
            fallback_doc_id(Path::new("scenarios/rug-pull.yaml")),
            "rug-pull"
        );
        assert_eq!(fallback_doc_id(Path::new("some/path/test.yml")), "test");
    }
}
