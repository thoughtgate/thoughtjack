//! Per-scenario MDX page generation.
//!
//! Assembles frontmatter, diagrams, prose, and framework mappings into
//! a complete MDX file.
//!
//! TJ-SPEC-011 F-003

use crate::error::DocsError;
use crate::mdx::frontmatter::generate_frontmatter;
use crate::mdx::prose;
use crate::mermaid;
use thoughtjack_core::config::schema::{MetadataSeverity, ScenarioMetadata, ServerConfig};

/// Generate a complete MDX page for a scenario.
///
/// Returns the MDX content as a string.
///
/// # Errors
///
/// Returns `DocsError` if diagram generation fails.
///
/// Implements: TJ-SPEC-011 F-003
pub fn generate_scenario_page(
    config: &ServerConfig,
    yaml_source: &str,
) -> Result<String, DocsError> {
    let metadata = config.metadata.as_ref().ok_or_else(|| {
        DocsError::Validation("scenario has no metadata block".to_string())
    })?;

    let mut sections = Vec::new();

    // Header comment
    sections.push("<!-- AUTO-GENERATED — DO NOT EDIT -->".to_string());
    sections.push(String::new());

    // Frontmatter
    sections.push(generate_frontmatter(metadata));
    sections.push(String::new());

    // Component imports (placeholders for React components)
    sections.push("import { AttackMetadataCard } from '@site/src/components/AttackMetadataCard';".to_string());
    sections.push("import { MermaidDiagram } from '@site/src/components/MermaidDiagram';".to_string());
    sections.push("import { MitreMapping } from '@site/src/components/MitreMapping';".to_string());
    sections.push("import { OwaspMcpMapping } from '@site/src/components/OwaspMcpMapping';".to_string());
    sections.push(String::new());

    // Metadata card
    sections.push(format!(
        "<AttackMetadataCard id=\"{}\" severity=\"{}\" />",
        metadata.id,
        severity_str(metadata.severity)
    ));
    sections.push(String::new());

    // Overview
    sections.push("## Overview".to_string());
    sections.push(String::new());
    sections.push(metadata.description.clone());
    sections.push(String::new());

    // Attack flow diagram
    let diagram_type = mermaid::auto_select(config);
    let renderer = mermaid::create_renderer(diagram_type);
    if let Ok(diagram) = renderer.render(config) {
        sections.push("## Attack Flow".to_string());
        sections.push(String::new());
        sections.push("```mermaid".to_string());
        sections.push(diagram);
        sections.push("```".to_string());
        sections.push(String::new());
    }

    // Phase breakdown
    if let Some(ref phases) = config.phases {
        sections.push("## Phase Breakdown".to_string());
        sections.push(String::new());
        for phase in phases {
            sections.push(format!("### {}", phase.name));
            sections.push(String::new());
            sections.push(prose::prose_for_phase(phase));
            sections.push(String::new());
        }
    }

    // Detection guidance
    render_detection_guidance(&mut sections, metadata);

    // Framework mappings
    render_framework_mappings(&mut sections, metadata);

    // Raw YAML
    sections.push("## Raw Scenario".to_string());
    sections.push(String::new());
    sections.push("<details>".to_string());
    sections.push("<summary>View YAML source</summary>".to_string());
    sections.push(String::new());
    sections.push("```yaml".to_string());
    sections.push(yaml_source.to_string());
    sections.push("```".to_string());
    sections.push(String::new());
    sections.push("</details>".to_string());

    Ok(sections.join("\n"))
}

/// Render detection guidance section.
fn render_detection_guidance(sections: &mut Vec<String>, metadata: &ScenarioMetadata) {
    if metadata.detection_guidance.is_empty() {
        return;
    }

    sections.push("## Detection Guidance".to_string());
    sections.push(String::new());
    for guidance in &metadata.detection_guidance {
        sections.push(format!("- {guidance}"));
    }
    sections.push(String::new());
}

/// Render framework mappings section.
fn render_framework_mappings(sections: &mut Vec<String>, metadata: &ScenarioMetadata) {
    let has_mitre = metadata.mitre_attack.is_some();
    let has_owasp = metadata.owasp_mcp.is_some();

    if !has_mitre && !has_owasp {
        return;
    }

    sections.push("## Framework Mappings".to_string());
    sections.push(String::new());

    if let Some(ref mitre) = metadata.mitre_attack {
        if !mitre.tactics.is_empty() || !mitre.techniques.is_empty() {
            sections.push("<MitreMapping".to_string());
            if !mitre.tactics.is_empty() {
                let tactic_ids: Vec<&str> = mitre.tactics.iter().map(|t| t.id.as_str()).collect();
                sections.push(format!("  tactics={{[{}]}}", format_string_array(&tactic_ids)));
            }
            if !mitre.techniques.is_empty() {
                let tech_ids: Vec<&str> =
                    mitre.techniques.iter().map(|t| t.id.as_str()).collect();
                sections.push(format!(
                    "  techniques={{[{}]}}",
                    format_string_array(&tech_ids)
                ));
            }
            sections.push("/>".to_string());
            sections.push(String::new());
        }
    }

    if let Some(ref owasp) = metadata.owasp_mcp {
        if !owasp.is_empty() {
            let ids: Vec<&str> = owasp.iter().map(|o| o.id.as_str()).collect();
            sections.push(format!(
                "<OwaspMcpMapping ids={{[{}]}} />",
                format_string_array(&ids)
            ));
            sections.push(String::new());
        }
    }
}

/// Format a string array for JSX props.
fn format_string_array(items: &[&str]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Convert severity enum to string.
const fn severity_str(severity: MetadataSeverity) -> &'static str {
    match severity {
        MetadataSeverity::Low => "low",
        MetadataSeverity::Medium => "medium",
        MetadataSeverity::High => "high",
        MetadataSeverity::Critical => "critical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtjack_core::config::schema::{
        AttackPrimitive, AttackVector, McpAttackSurface, MitreAttackMapping, MitreTactic,
        MitreTechnique, OwaspMcpEntry,
    };

    fn test_config() -> ServerConfig {
        let yaml = r#"
server:
  name: test

metadata:
  id: TJ-ATK-001
  name: Classic Rug Pull
  description: Trust-building phase followed by tool replacement attack.
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
  tags:
    - temporal
    - rug-pull
  detection_guidance:
    - Monitor for tools/list_changed notifications
    - Compare tool definitions before and after transitions

baseline:
  tools: []

phases:
  - name: trust_building
    advance:
      on: tools/call
      count: 3
  - name: exploit
    replace_tools:
      calculator: tools/calculator/injection.yaml
"#;
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_generate_page_structure() {
        let config = test_config();
        let page = generate_scenario_page(&config, "# raw yaml").unwrap();

        assert!(page.starts_with("<!-- AUTO-GENERATED"));
        assert!(page.contains("---"));
        assert!(page.contains("id: TJ-ATK-001"));
        assert!(page.contains("## Overview"));
        assert!(page.contains("## Attack Flow"));
        assert!(page.contains("```mermaid"));
        assert!(page.contains("## Phase Breakdown"));
        assert!(page.contains("### trust_building"));
        assert!(page.contains("### exploit"));
        assert!(page.contains("## Detection Guidance"));
        assert!(page.contains("## Framework Mappings"));
        assert!(page.contains("## Raw Scenario"));
        assert!(page.contains("# raw yaml"));
    }

    #[test]
    fn test_page_contains_detection_guidance() {
        let config = test_config();
        let page = generate_scenario_page(&config, "").unwrap();
        assert!(page.contains("Monitor for tools/list_changed"));
        assert!(page.contains("Compare tool definitions"));
    }

    #[test]
    fn test_page_contains_mitre_mapping() {
        let config = test_config();
        let page = generate_scenario_page(&config, "").unwrap();
        assert!(page.contains("MitreMapping"));
        assert!(page.contains("TA0001"));
        assert!(page.contains("T1195.002"));
    }

    #[test]
    fn test_page_contains_owasp_mapping() {
        let config = test_config();
        let page = generate_scenario_page(&config, "").unwrap();
        assert!(page.contains("OwaspMcpMapping"));
        assert!(page.contains("MCP03"));
    }

    #[test]
    fn test_no_metadata_error() {
        let yaml = r#"
server:
  name: test
tools:
  - tool:
      name: echo
      description: Echo
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: hello
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let result = generate_scenario_page(&config, "");
        assert!(result.is_err());
    }

    #[test]
    fn test_severity_str() {
        assert_eq!(severity_str(MetadataSeverity::Low), "low");
        assert_eq!(severity_str(MetadataSeverity::Medium), "medium");
        assert_eq!(severity_str(MetadataSeverity::High), "high");
        assert_eq!(severity_str(MetadataSeverity::Critical), "critical");
    }
}
