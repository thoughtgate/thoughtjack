//! Metadata validation for scenario files.
//!
//! Validates that scenario metadata fields conform to expected patterns
//! and detects duplicate IDs across the registry.
//!
//! TJ-SPEC-011 F-010

use std::collections::HashMap;
use std::path::Path;
use thoughtjack_core::config::schema::ScenarioMetadata;

/// A validation error with path context.
///
/// Implements: TJ-SPEC-011 F-010
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// Path to the scenario file.
    pub path: String,
    /// Field that failed validation.
    pub field: String,
    /// Human-readable error message.
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ERROR [TJ-SPEC-011] {}\n  \u{2192} {}: {}",
            self.path, self.field, self.message
        )
    }
}

/// Validate a single scenario's metadata.
///
/// Returns a list of validation errors found. An empty list means valid.
///
/// Implements: TJ-SPEC-011 F-010
#[must_use]
pub fn validate_metadata(metadata: &ScenarioMetadata, path: &Path) -> Vec<ValidationError> {
    let path_str = path.display().to_string();
    let mut errors = Vec::new();

    // Validate metadata.id matches TJ-ATK-\d{3}
    if !is_valid_atk_id(&metadata.id) {
        errors.push(ValidationError {
            path: path_str.clone(),
            field: "metadata.id".to_string(),
            message: format!("expected format TJ-ATK-NNN, got \"{}\"", metadata.id),
        });
    }

    // Validate MITRE tactic IDs
    if let Some(ref mitre) = metadata.mitre_attack {
        for tactic in &mitre.tactics {
            if !is_valid_tactic_id(&tactic.id) {
                errors.push(ValidationError {
                    path: path_str.clone(),
                    field: "metadata.mitre_attack.tactics[].id".to_string(),
                    message: format!(
                        "expected format TA followed by 4 digits, got \"{}\"",
                        tactic.id
                    ),
                });
            }
        }

        for technique in &mitre.techniques {
            if !is_valid_technique_id(&technique.id) {
                errors.push(ValidationError {
                    path: path_str.clone(),
                    field: "metadata.mitre_attack.techniques[].id".to_string(),
                    message: format!(
                        "expected format T followed by 4 digits (optional .NNN), got \"{}\"",
                        technique.id
                    ),
                });
            }
        }
    }

    // Validate OWASP MCP IDs
    if let Some(ref owasp) = metadata.owasp_mcp {
        for entry in owasp {
            if !is_valid_mcp_id(&entry.id) {
                errors.push(ValidationError {
                    path: path_str.clone(),
                    field: "metadata.owasp_mcp[].id".to_string(),
                    message: format!(
                        "expected format MCP followed by 2 digits, got \"{}\"",
                        entry.id
                    ),
                });
            }
        }
    }

    // Validate OWASP Agentic IDs
    if let Some(ref agentic) = metadata.owasp_agentic {
        for entry in agentic {
            if !is_valid_asi_id(&entry.id) {
                errors.push(ValidationError {
                    path: path_str.clone(),
                    field: "metadata.owasp_agentic[].id".to_string(),
                    message: format!(
                        "expected format ASI followed by 2 digits, got \"{}\"",
                        entry.id
                    ),
                });
            }
        }
    }

    errors
}

/// Detect duplicate metadata IDs across scenarios.
///
/// Returns a list of validation errors for any duplicate IDs found.
///
/// Implements: TJ-SPEC-011 F-010
#[must_use]
pub fn detect_duplicate_ids(scenarios: &[(String, ScenarioMetadata)]) -> Vec<ValidationError> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    let mut errors = Vec::new();

    for (path, metadata) in scenarios {
        if let Some(first_path) = seen.get(metadata.id.as_str()) {
            errors.push(ValidationError {
                path: path.clone(),
                field: "metadata.id".to_string(),
                message: format!(
                    "duplicate ID \"{}\", first seen in {}",
                    metadata.id, first_path
                ),
            });
        } else {
            seen.insert(&metadata.id, path);
        }
    }

    errors
}

/// Check if an ID matches `TJ-ATK-\d{3}`.
fn is_valid_atk_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 10
        && bytes.starts_with(b"TJ-ATK-")
        && bytes[7].is_ascii_digit()
        && bytes[8].is_ascii_digit()
        && bytes[9].is_ascii_digit()
}

/// Check if an ID matches `TA\d{4}`.
fn is_valid_tactic_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 6 && bytes.starts_with(b"TA") && bytes[2..].iter().all(u8::is_ascii_digit)
}

/// Check if an ID matches `T\d{4}(\.\d{3})?`.
fn is_valid_technique_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() == 5 {
        // T followed by 4 digits
        bytes[0] == b'T' && bytes[1..].iter().all(u8::is_ascii_digit)
    } else if bytes.len() == 9 {
        // T followed by 4 digits, dot, 3 digits
        bytes[0] == b'T'
            && bytes[1..5].iter().all(u8::is_ascii_digit)
            && bytes[5] == b'.'
            && bytes[6..].iter().all(u8::is_ascii_digit)
    } else {
        false
    }
}

/// Check if an ID matches `MCP\d{2}`.
fn is_valid_mcp_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 5
        && bytes.starts_with(b"MCP")
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
}

/// Check if an ID matches `ASI\d{2}`.
fn is_valid_asi_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 5
        && bytes.starts_with(b"ASI")
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtjack_core::config::schema::{
        McpAttackSurface, MetadataSeverity, MitreAttackMapping, MitreTactic, MitreTechnique,
        OwaspAgenticEntry, OwaspMcpEntry,
    };

    fn valid_metadata() -> ScenarioMetadata {
        ScenarioMetadata {
            id: "TJ-ATK-001".to_string(),
            name: "Test Scenario".to_string(),
            description: "A test.".to_string(),
            author: None,
            created: None,
            updated: None,
            severity: MetadataSeverity::High,
            mitre_attack: Some(MitreAttackMapping {
                tactics: vec![MitreTactic {
                    id: "TA0001".to_string(),
                    name: "Initial Access".to_string(),
                }],
                techniques: vec![MitreTechnique {
                    id: "T1195.002".to_string(),
                    name: "Supply Chain Compromise".to_string(),
                    sub_technique: None,
                }],
            }),
            owasp_mcp: Some(vec![OwaspMcpEntry {
                id: "MCP03".to_string(),
                name: "Tool Poisoning".to_string(),
            }]),
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

    #[test]
    fn test_valid_metadata_no_errors() {
        let metadata = valid_metadata();
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn test_invalid_atk_id() {
        let mut metadata = valid_metadata();
        metadata.id = "INVALID-001".to_string();
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("metadata.id"));
    }

    #[test]
    fn test_invalid_atk_id_too_short() {
        let mut metadata = valid_metadata();
        metadata.id = "TJ-ATK-01".to_string();
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_invalid_tactic_id() {
        let mut metadata = valid_metadata();
        metadata.mitre_attack = Some(MitreAttackMapping {
            tactics: vec![MitreTactic {
                id: "INVALID".to_string(),
                name: "Bad".to_string(),
            }],
            techniques: vec![],
        });
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("tactics"));
    }

    #[test]
    fn test_invalid_technique_id() {
        let mut metadata = valid_metadata();
        metadata.mitre_attack = Some(MitreAttackMapping {
            tactics: vec![],
            techniques: vec![MitreTechnique {
                id: "T12345".to_string(),
                name: "Bad".to_string(),
                sub_technique: None,
            }],
        });
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("techniques"));
    }

    #[test]
    fn test_valid_technique_without_sub() {
        let mut metadata = valid_metadata();
        metadata.mitre_attack = Some(MitreAttackMapping {
            tactics: vec![],
            techniques: vec![MitreTechnique {
                id: "T1234".to_string(),
                name: "Good".to_string(),
                sub_technique: None,
            }],
        });
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert!(errors.is_empty());
    }

    #[test]
    fn test_invalid_owasp_mcp_id() {
        let mut metadata = valid_metadata();
        metadata.owasp_mcp = Some(vec![OwaspMcpEntry {
            id: "BAD01".to_string(),
            name: "Bad".to_string(),
        }]);
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("owasp_mcp"));
    }

    #[test]
    fn test_invalid_owasp_agentic_id() {
        let mut metadata = valid_metadata();
        metadata.owasp_agentic = Some(vec![OwaspAgenticEntry {
            id: "BAD01".to_string(),
            name: "Bad".to_string(),
        }]);
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("owasp_agentic"));
    }

    #[test]
    fn test_duplicate_id_detection() {
        let scenarios = vec![
            ("a.yaml".to_string(), valid_metadata()),
            ("b.yaml".to_string(), valid_metadata()),
        ];
        let errors = detect_duplicate_ids(&scenarios);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("duplicate"));
        assert!(errors[0].message.contains("a.yaml"));
    }

    #[test]
    fn test_no_duplicates() {
        let mut meta2 = valid_metadata();
        meta2.id = "TJ-ATK-002".to_string();
        let scenarios = vec![
            ("a.yaml".to_string(), valid_metadata()),
            ("b.yaml".to_string(), meta2),
        ];
        let errors = detect_duplicate_ids(&scenarios);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_error_display_format() {
        let error = ValidationError {
            path: "scenarios/test.yaml".to_string(),
            field: "metadata.id".to_string(),
            message: "expected format TJ-ATK-NNN".to_string(),
        };
        let display = error.to_string();
        assert!(display.contains("ERROR [TJ-SPEC-011]"));
        assert!(display.contains("scenarios/test.yaml"));
        assert!(display.contains("\u{2192}"));
    }

    #[test]
    fn test_multiple_errors_accumulated() {
        let mut metadata = valid_metadata();
        metadata.id = "BAD".to_string();
        metadata.mitre_attack = Some(MitreAttackMapping {
            tactics: vec![MitreTactic {
                id: "INVALID".to_string(),
                name: "Bad".to_string(),
            }],
            techniques: vec![MitreTechnique {
                id: "INVALID".to_string(),
                name: "Bad".to_string(),
                sub_technique: None,
            }],
        });
        metadata.owasp_mcp = Some(vec![OwaspMcpEntry {
            id: "BAD".to_string(),
            name: "Bad".to_string(),
        }]);
        let errors = validate_metadata(&metadata, Path::new("test.yaml"));
        assert_eq!(errors.len(), 4); // id + tactic + technique + owasp
    }
}
