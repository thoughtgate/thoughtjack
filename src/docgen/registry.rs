//! Scenario registry parsing and orphan detection.
//!
//! The registry (`registry.yaml`) controls scenario ordering, categorization,
//! and site navigation. It validates that entries point to existing files and
//! detects orphan scenarios (those with metadata but not in the registry).
//!
//! TJ-SPEC-011 F-004

use crate::docgen::error::DocsError;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Parsed scenario registry.
///
/// Implements: TJ-SPEC-011 F-004
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    /// Site-level configuration.
    pub site: SiteConfig,

    /// Ordered list of scenario categories.
    pub categories: Vec<Category>,
}

/// Site-level configuration from the registry.
///
/// Implements: TJ-SPEC-011 F-004
#[derive(Debug, Clone, Deserialize)]
pub struct SiteConfig {
    /// Site title.
    pub title: String,

    /// Site description.
    pub description: String,

    /// Base URL for the documentation site.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// A scenario category in the registry.
///
/// Implements: TJ-SPEC-011 F-004
#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    /// Category identifier (used in URLs).
    pub id: String,

    /// Human-readable category name.
    pub name: String,

    /// Category description.
    #[serde(default)]
    pub description: Option<String>,

    /// Display order.
    #[serde(default)]
    pub order: Option<u32>,

    /// Ordered list of scenario file paths within this category.
    #[serde(default)]
    pub scenarios: Vec<PathBuf>,
}

/// Result of registry validation.
///
/// Implements: TJ-SPEC-011 F-004
#[derive(Debug, Default)]
pub struct RegistryValidation {
    /// Files referenced in registry but not found on disk.
    pub missing_files: Vec<PathBuf>,

    /// Scenario files with metadata that are not in the registry.
    pub orphan_scenarios: Vec<PathBuf>,

    /// Categories with no scenarios.
    pub empty_categories: Vec<String>,
}

impl RegistryValidation {
    /// Returns `true` if there are no issues.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing_files.is_empty()
            && self.orphan_scenarios.is_empty()
            && self.empty_categories.is_empty()
    }

    /// Returns `true` if there are errors (missing files).
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.missing_files.is_empty()
    }
}

/// Parse a registry YAML file.
///
/// # Errors
///
/// Returns `DocsError::Yaml` if the file cannot be parsed.
///
/// Implements: TJ-SPEC-011 F-004
pub fn parse_registry(content: &str) -> Result<Registry, DocsError> {
    let registry: Registry = serde_yaml::from_str(content)?;
    Ok(registry)
}

/// Validate registry entries against the filesystem and detect orphans.
///
/// - `base_dir` is the directory relative to which scenario paths are resolved.
/// - `scenario_files_with_metadata` is the set of scenario files that have metadata blocks.
/// - `include_targets` is the set of files referenced via `$include` (considered accounted for).
///
/// # Errors
///
/// Returns `DocsError::Registry` if validation fails in strict mode.
///
/// Implements: TJ-SPEC-011 F-004, EC-DOCS-002, EC-DOCS-003
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn validate_registry(
    registry: &Registry,
    base_dir: &Path,
    scenario_files_with_metadata: &HashSet<PathBuf>,
    include_targets: &HashSet<PathBuf>,
) -> RegistryValidation {
    let mut validation = RegistryValidation::default();

    // Collect all paths referenced in registry
    let mut registry_paths: HashSet<PathBuf> = HashSet::new();

    for category in &registry.categories {
        if category.scenarios.is_empty() {
            validation.empty_categories.push(category.id.clone());
        }

        for scenario_path in &category.scenarios {
            registry_paths.insert(scenario_path.clone());

            // Check file exists (EC-DOCS-002)
            let full_path = base_dir.join(scenario_path);
            if !full_path.exists() {
                validation.missing_files.push(scenario_path.clone());
            }
        }
    }

    // Detect orphans (EC-DOCS-003): files with metadata not in registry
    // Exclude $include targets (EC-DOCS-003b)
    for metadata_file in scenario_files_with_metadata {
        if !registry_paths.contains(metadata_file) && !include_targets.contains(metadata_file) {
            validation.orphan_scenarios.push(metadata_file.clone());
        }
    }

    validation
}

/// Collect all scenario paths from a registry.
///
/// Implements: TJ-SPEC-011 F-004
#[must_use]
pub fn all_scenario_paths(registry: &Registry) -> Vec<&Path> {
    registry
        .categories
        .iter()
        .flat_map(|cat| cat.scenarios.iter().map(PathBuf::as_path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_registry() {
        let yaml = r"
site:
  title: ThoughtJack Attack Catalog
  description: Reference catalog of MCP attack scenarios
  base_url: /thoughtjack

categories:
  - id: injection
    name: Injection Attacks
    description: Prompt injection via tool responses
    order: 1
    scenarios:
      - scenarios/injection/tool-response.yaml
      - scenarios/injection/resource.yaml

  - id: phased
    name: Phased Attacks
    order: 2
    scenarios:
      - scenarios/phased/rug-pull.yaml
";

        let registry = parse_registry(yaml).unwrap();
        assert_eq!(registry.site.title, "ThoughtJack Attack Catalog");
        assert_eq!(registry.categories.len(), 2);
        assert_eq!(registry.categories[0].id, "injection");
        assert_eq!(registry.categories[0].scenarios.len(), 2);
        assert_eq!(registry.categories[1].scenarios.len(), 1);
    }

    #[test]
    fn test_empty_category_detection() {
        let yaml = r"
site:
  title: Test
  description: Test

categories:
  - id: empty
    name: Empty Category
    scenarios: []
  - id: full
    name: Full Category
    scenarios:
      - scenarios/test.yaml
";

        let registry = parse_registry(yaml).unwrap();
        let validation = validate_registry(
            &registry,
            Path::new("/nonexistent"),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(validation.empty_categories, vec!["empty"]);
    }

    #[test]
    fn test_orphan_detection() {
        let yaml = r"
site:
  title: Test
  description: Test

categories:
  - id: test
    name: Test
    scenarios:
      - scenarios/registered.yaml
";

        let registry = parse_registry(yaml).unwrap();
        let mut metadata_files = HashSet::new();
        metadata_files.insert(PathBuf::from("scenarios/registered.yaml"));
        metadata_files.insert(PathBuf::from("scenarios/orphan.yaml"));

        let validation = validate_registry(
            &registry,
            Path::new("/nonexistent"),
            &metadata_files,
            &HashSet::new(),
        );
        assert_eq!(
            validation.orphan_scenarios,
            vec![PathBuf::from("scenarios/orphan.yaml")]
        );
    }

    #[test]
    fn test_include_targets_not_orphans() {
        let yaml = r"
site:
  title: Test
  description: Test

categories:
  - id: test
    name: Test
    scenarios: []
";

        let registry = parse_registry(yaml).unwrap();
        let mut include_targets = HashSet::new();
        include_targets.insert(PathBuf::from("tools/fragment.yaml"));

        let mut metadata_files = HashSet::new();
        metadata_files.insert(PathBuf::from("tools/fragment.yaml"));

        let validation = validate_registry(
            &registry,
            Path::new("/nonexistent"),
            &metadata_files,
            &include_targets,
        );
        assert!(validation.orphan_scenarios.is_empty());
    }

    #[test]
    fn test_all_scenario_paths() {
        let yaml = r"
site:
  title: Test
  description: Test

categories:
  - id: a
    name: A
    scenarios:
      - s1.yaml
      - s2.yaml
  - id: b
    name: B
    scenarios:
      - s3.yaml
";

        let registry = parse_registry(yaml).unwrap();
        let paths = all_scenario_paths(&registry);
        assert_eq!(paths.len(), 3);
    }
}
