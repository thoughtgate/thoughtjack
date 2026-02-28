//! OATF document loader with ThoughtJack-specific pre-processing.
//!
//! This module handles loading OATF YAML documents via the `oatf-rs` SDK,
//! with a pre-processing step that extracts `await_extractors` keys
//! (a `ThoughtJack` runtime hint) before passing clean YAML to the SDK.
//!
//! See TJ-SPEC-013 §7 for the loading pipeline.

use std::collections::HashMap;
use std::time::Duration;

use crate::engine::types::AwaitExtractor;
use crate::error::LoaderError;

/// Result of pre-processing YAML before SDK loading.
#[derive(Debug)]
struct PreprocessResult {
    /// Clean YAML with `await_extractors` keys removed.
    clean_yaml: String,
    /// Extracted `await_extractors` configs keyed by `(actor_name, phase_name)`.
    await_extractors: HashMap<(String, String), Vec<AwaitExtractor>>,
}

/// Result of loading an OATF document.
///
/// Contains the normalized document and any extracted `await_extractors`
/// configuration, keyed by phase index per actor.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug)]
pub struct LoadedDocument {
    /// The normalized OATF document.
    pub document: oatf::Document,
    /// `await_extractors` configs keyed by `(actor_name, phase_index)`.
    pub await_extractors: HashMap<(String, usize), Vec<AwaitExtractor>>,
}

/// Pre-processes YAML to extract `await_extractors` keys.
///
/// Parses the YAML, finds any `await_extractors` entries on phase
/// objects, extracts them into a lookup table, removes them from
/// the YAML tree, and re-serializes clean YAML for the SDK.
///
/// If the document has only a single actor, `await_extractors` is
/// ignored with a warning (cross-actor sync is meaningless).
///
/// # Errors
///
/// Returns `LoaderError::Preprocess` if YAML parsing fails.
///
/// Implements: TJ-SPEC-013 F-001
fn preprocess_yaml(yaml: &str) -> Result<PreprocessResult, LoaderError> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| LoaderError::Preprocess(e.to_string()))?;

    let mut await_map: HashMap<(String, String), Vec<AwaitExtractor>> = HashMap::new();

    // Navigate to the phases in the document.
    // OATF supports three forms: single-phase (state), multi-phase (phases), multi-actor (actors).
    // After normalization the SDK converts all to actors, but we pre-process raw YAML.

    let attack = doc.get_mut("attack");
    let execution = attack.and_then(|a| a.get_mut("execution"));

    if let Some(execution) = execution {
        // Determine actor count for single-actor warning
        let is_single_actor = execution
            .get("actors")
            .is_none_or(|a| a.as_sequence().is_none_or(|seq| seq.len() <= 1));

        // Process phases in multi-phase form
        if let Some(phases) = execution.get_mut("phases") {
            extract_from_phases(phases, "default", is_single_actor, &mut await_map);
        }

        // Process phases in multi-actor form
        if let Some(actors) = execution.get_mut("actors")
            && let Some(actors_seq) = actors.as_sequence_mut()
        {
            for actor in actors_seq {
                let actor_name = actor
                    .get("name")
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or("default")
                    .to_string();

                if let Some(phases) = actor.get_mut("phases") {
                    extract_from_phases(phases, &actor_name, is_single_actor, &mut await_map);
                }
            }
        }
    }

    let clean_yaml =
        serde_yaml::to_string(&doc).map_err(|e| LoaderError::Preprocess(e.to_string()))?;

    Ok(PreprocessResult {
        clean_yaml,
        await_extractors: await_map,
    })
}

/// Extract `await_extractors` from a phases sequence.
fn extract_from_phases(
    phases: &mut serde_yaml::Value,
    actor_name: &str,
    is_single_actor: bool,
    await_map: &mut HashMap<(String, String), Vec<AwaitExtractor>>,
) {
    let Some(phases_seq) = phases.as_sequence_mut() else {
        return;
    };

    for phase in phases_seq {
        let Some(phase_map) = phase.as_mapping_mut() else {
            continue;
        };

        let phase_name = phase_map
            .get(serde_yaml::Value::String("name".to_string()))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("unnamed")
            .to_string();

        let await_key = serde_yaml::Value::String("await_extractors".to_string());

        if let Some(await_val) = phase_map.remove(&await_key) {
            if is_single_actor {
                tracing::warn!(
                    phase = %phase_name,
                    "await_extractors on single-actor document — ignored"
                );
                continue;
            }

            if let Some(specs) = parse_await_extractors(&await_val) {
                await_map.insert((actor_name.to_string(), phase_name), specs);
            }
        }
    }
}

/// Parse an `await_extractors` YAML value into `AwaitExtractor` specs.
///
/// Skips individual malformed entries with a warning rather than
/// failing the entire list.
fn parse_await_extractors(value: &serde_yaml::Value) -> Option<Vec<AwaitExtractor>> {
    let seq = value.as_sequence()?;
    let mut result = Vec::with_capacity(seq.len());

    for (i, item) in seq.iter().enumerate() {
        let Some(actor) = item.get("actor").and_then(serde_yaml::Value::as_str) else {
            tracing::warn!(
                index = i,
                "await_extractors entry missing required 'actor' field — skipped"
            );
            continue;
        };
        let Some(extractor_seq) = item
            .get("extractors")
            .and_then(serde_yaml::Value::as_sequence)
        else {
            tracing::warn!(
                index = i,
                actor,
                "await_extractors entry missing required 'extractors' field — skipped"
            );
            continue;
        };
        let extractors: Vec<String> = extractor_seq
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .map(String::from)
            .collect();
        let timeout_str = item
            .get("timeout")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("30s");
        let timeout = humantime::parse_duration(timeout_str).unwrap_or(Duration::from_secs(30));

        result.push(AwaitExtractor {
            actor: actor.to_string(),
            extractors,
            timeout,
        });
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Loads an OATF document from YAML with `ThoughtJack` pre-processing.
///
/// 1. Pre-processes the YAML to extract `await_extractors` keys.
/// 2. Passes clean YAML to `oatf::load()` for parsing, validation,
///    and normalization.
/// 3. Logs any SDK warnings.
/// 4. Returns the document with extracted `await_extractors` config.
///
/// # Errors
///
/// Returns `LoaderError::OatfLoad` if the SDK rejects the document.
/// Returns `LoaderError::Preprocess` if YAML pre-processing fails.
///
/// Implements: TJ-SPEC-013 F-001
pub fn load_document(yaml: &str) -> Result<LoadedDocument, LoaderError> {
    let preprocess = preprocess_yaml(yaml)?;

    let load_result = oatf::load(&preprocess.clean_yaml).map_err(|errors| {
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        LoaderError::OatfLoad(messages.join("; "))
    })?;

    // Log SDK warnings
    for warning in &load_result.warnings {
        tracing::warn!(
            code = %warning.code,
            path = ?warning.path,
            "{}",
            warning.message
        );
    }

    let document = load_result.document;

    // Convert (actor_name, phase_name) → (actor_name, phase_index) mapping
    let mut await_by_index: HashMap<(String, usize), Vec<AwaitExtractor>> = HashMap::new();

    if let Some(actors) = &document.attack.execution.actors {
        for actor in actors {
            for (phase_index, phase) in actor.phases.iter().enumerate() {
                let phase_name = phase.name.as_deref().unwrap_or("unnamed");
                let key = (actor.name.clone(), phase_name.to_string());
                if let Some(specs) = preprocess.await_extractors.get(&key) {
                    await_by_index.insert((actor.name.clone(), phase_index), specs.clone());
                }
            }
        }
    }

    Ok(LoadedDocument {
        document,
        await_extractors: await_by_index,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_valid_single_phase_document() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "A test tool"
          inputSchema:
            type: object
"#;

        let result = load_document(yaml).unwrap();
        assert!(result.document.attack.name.as_deref() == Some("test"));
        assert!(result.await_extractors.is_empty());
    }

    #[test]
    fn load_valid_multi_phase_document() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: phase_one
        state:
          tools:
            - name: calculator
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 3
      - name: phase_two
"#;

        let result = load_document(yaml).unwrap();
        let actors = result.document.attack.execution.actors.unwrap();
        assert_eq!(actors[0].phases.len(), 2);
    }

    #[test]
    fn load_invalid_document_returns_error() {
        let yaml = "not: valid: oatf";
        assert!(load_document(yaml).is_err());
    }

    #[test]
    fn preprocess_extracts_await_extractors() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    actors:
      - name: actor1
        mode: mcp_server
        phases:
          - name: phase_one
            await_extractors:
              - actor: actor2
                extractors:
                  - token
                  - session_id
                timeout: "10s"
            trigger:
              event: tools/call
          - name: phase_two
      - name: actor2
        mode: a2a_client
        phases:
          - name: setup
"#;

        let result = preprocess_yaml(yaml).unwrap();

        // await_extractors should be extracted
        let key = ("actor1".to_string(), "phase_one".to_string());
        let specs = result.await_extractors.get(&key).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].actor, "actor2");
        assert_eq!(specs[0].extractors, vec!["token", "session_id"]);
        assert_eq!(specs[0].timeout, Duration::from_secs(10));

        // Clean YAML should not contain await_extractors
        assert!(!result.clean_yaml.contains("await_extractors"));
    }

    #[test]
    fn preprocess_single_actor_warns_and_ignores() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    phases:
      - name: phase_one
        await_extractors:
          - actor: other
            extractors:
              - token
            timeout: "5s"
        trigger:
          event: tools/call
      - name: phase_two
"#;

        let result = preprocess_yaml(yaml).unwrap();
        // Single-actor document — await_extractors should be ignored
        assert!(result.await_extractors.is_empty());
    }

    #[test]
    fn preprocess_clean_yaml_passes_through() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#;

        let result = preprocess_yaml(yaml).unwrap();
        assert!(result.await_extractors.is_empty());
        assert!(!result.clean_yaml.is_empty());
    }

    #[test]
    fn preprocess_invalid_yaml_returns_error() {
        let yaml = "{{invalid yaml";
        assert!(preprocess_yaml(yaml).is_err());
    }
}
