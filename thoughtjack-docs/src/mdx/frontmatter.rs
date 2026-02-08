//! YAML frontmatter generation for MDX pages.
//!
//! TJ-SPEC-011 F-003

use thoughtjack_core::config::schema::ScenarioMetadata;

/// Generate Docusaurus YAML frontmatter from scenario metadata.
///
/// Produces frontmatter with `id`, `title`, `sidebar_label`, and `tags`.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn generate_frontmatter(metadata: &ScenarioMetadata) -> String {
    let mut lines = Vec::new();
    lines.push("---".to_string());
    lines.push(format!("id: {}", metadata.id));
    lines.push(format!("title: {}", quote_yaml_string(&metadata.name)));
    lines.push(format!(
        "sidebar_label: {}",
        quote_yaml_string(&metadata.name)
    ));

    if !metadata.tags.is_empty() {
        lines.push("tags:".to_string());
        for tag in &metadata.tags {
            lines.push(format!("  - {tag}"));
        }
    }

    lines.push("---".to_string());
    lines.join("\n")
}

/// Quote a YAML string value if it contains special characters.
fn quote_yaml_string(s: &str) -> String {
    if s.contains(':') || s.contains('#') || s.contains('"') || s.starts_with(' ') {
        let escaped = s.replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtjack_core::config::schema::{McpAttackSurface, MetadataSeverity};

    fn minimal_metadata() -> ScenarioMetadata {
        ScenarioMetadata {
            id: "TJ-ATK-001".to_string(),
            name: "Classic Rug Pull".to_string(),
            description: "A rug pull attack".to_string(),
            author: None,
            created: None,
            updated: None,
            severity: MetadataSeverity::High,
            mitre_attack: None,
            owasp_mcp: None,
            owasp_agentic: None,
            mcp_attack_surface: McpAttackSurface {
                vectors: vec![],
                primitives: vec![],
            },
            tags: vec!["temporal".to_string(), "rug-pull".to_string()],
            detection_guidance: vec![],
            references: vec![],
        }
    }

    #[test]
    fn test_basic_frontmatter() {
        let meta = minimal_metadata();
        let fm = generate_frontmatter(&meta);
        assert!(fm.starts_with("---"));
        assert!(fm.ends_with("---"));
        assert!(fm.contains("id: TJ-ATK-001"));
        assert!(fm.contains("title: Classic Rug Pull"));
        assert!(fm.contains("sidebar_label: Classic Rug Pull"));
    }

    #[test]
    fn test_frontmatter_with_tags() {
        let meta = minimal_metadata();
        let fm = generate_frontmatter(&meta);
        assert!(fm.contains("tags:"));
        assert!(fm.contains("  - temporal"));
        assert!(fm.contains("  - rug-pull"));
    }

    #[test]
    fn test_frontmatter_no_tags() {
        let mut meta = minimal_metadata();
        meta.tags.clear();
        let fm = generate_frontmatter(&meta);
        assert!(!fm.contains("tags:"));
    }

    #[test]
    fn test_quote_special_chars() {
        assert_eq!(
            quote_yaml_string("Name: with colon"),
            "\"Name: with colon\""
        );
    }

    #[test]
    fn test_quote_normal_string() {
        assert_eq!(quote_yaml_string("Normal Name"), "Normal Name");
    }
}
