//! OATF document loader with ThoughtJack-specific pre-processing.
//!
//! This module handles loading OATF YAML documents via the `oatf-rs` SDK,
//! with a pre-processing step that extracts `await_extractors` keys
//! (a `ThoughtJack` runtime hint) before passing clean YAML to the SDK.
//!
//! See TJ-SPEC-013 §7 for the loading pipeline.

use crate::engine::types::AwaitExtractor;
use crate::error::LoaderError;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
use std::time::Duration;

/// Result of pre-processing YAML before SDK loading.
#[derive(Debug)]
struct PreprocessResult {
    /// Clean YAML with `await_extractors` keys removed.
    clean_yaml: String,
    /// Extracted `await_extractors` configs keyed by `(actor_name, phase_index)`.
    await_extractors: HashMap<(String, usize), Vec<AwaitExtractor>>,
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

    let mut await_map: HashMap<(String, usize), Vec<AwaitExtractor>> = HashMap::new();

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
            extract_from_phases(phases, "default", is_single_actor, &mut await_map)?;
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
                    extract_from_phases(phases, &actor_name, is_single_actor, &mut await_map)?;
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
    await_map: &mut HashMap<(String, usize), Vec<AwaitExtractor>>,
) -> Result<(), LoaderError> {
    let Some(phases_seq) = phases.as_sequence_mut() else {
        return Ok(());
    };

    for (phase_index, phase) in phases_seq.iter_mut().enumerate() {
        let Some(phase_map) = phase.as_mapping_mut() else {
            continue;
        };

        let await_key = serde_yaml::Value::String("await_extractors".to_string());

        if let Some(await_val) = phase_map.remove(&await_key) {
            if is_single_actor {
                tracing::warn!(
                    phase_index,
                    "await_extractors on single-actor document — ignored"
                );
                continue;
            }

            if let Some(specs) = parse_await_extractors(&await_val)? {
                await_map.insert((actor_name.to_string(), phase_index), specs);
            }
        }
    }

    Ok(())
}

/// Parse an `await_extractors` YAML value into `AwaitExtractor` specs.
///
fn parse_await_extractors(
    value: &serde_yaml::Value,
) -> Result<Option<Vec<AwaitExtractor>>, LoaderError> {
    let seq = value.as_sequence().ok_or_else(|| {
        LoaderError::Preprocess("await_extractors must be a sequence".to_string())
    })?;
    let mut result = Vec::with_capacity(seq.len());

    for (i, item) in seq.iter().enumerate() {
        let actor = item
            .get("actor")
            .and_then(serde_yaml::Value::as_str)
            .ok_or_else(|| {
                LoaderError::Preprocess(format!(
                    "await_extractors[{i}] is missing required string field 'actor'"
                ))
            })?;
        let extractor_seq = item
            .get("extractors")
            .and_then(serde_yaml::Value::as_sequence)
            .ok_or_else(|| {
                LoaderError::Preprocess(format!(
                    "await_extractors[{i}] for actor '{actor}' is missing required sequence field 'extractors'"
                ))
            })?;
        let mut extractors = Vec::with_capacity(extractor_seq.len());
        for (extractor_index, extractor) in extractor_seq.iter().enumerate() {
            let extractor_name = extractor.as_str().ok_or_else(|| {
                LoaderError::Preprocess(format!(
                    "await_extractors[{i}].extractors[{extractor_index}] for actor '{actor}' must be a string"
                ))
            })?;
            extractors.push(extractor_name.to_string());
        }
        if extractors.is_empty() {
            return Err(LoaderError::Preprocess(format!(
                "await_extractors[{i}] for actor '{actor}' must contain at least one extractor"
            )));
        }
        let timeout_str = item
            .get("timeout")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("30s");
        let timeout = humantime::parse_duration(timeout_str).map_err(|e| {
            LoaderError::Preprocess(format!(
                "await_extractors[{i}] for actor '{actor}' has invalid timeout '{timeout_str}': {e}"
            ))
        })?;

        result.push(AwaitExtractor {
            actor: actor.to_string(),
            extractors,
            timeout,
        });
    }

    if result.is_empty() {
        Ok(None)
    } else {
        Ok(Some(result))
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

    // Validate actors exist after SDK normalization (centralizes invariant
    // so downstream code can use document_actors() infallibly).
    if document
        .attack
        .execution
        .actors
        .as_ref()
        .is_none_or(Vec::is_empty)
    {
        return Err(LoaderError::OatfLoad(
            "document has no actors after normalization".to_string(),
        ));
    }

    let await_by_index = preprocess.await_extractors;

    // Check for circular await_extractors dependencies (EC-ORCH-003)
    detect_await_cycles(&await_by_index)?;

    // Validate await_extractors reference existing actors (OATF §5.5)
    let actor_names: HashSet<&str> = document_actors(&document)
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    for ((_, _), specs) in &await_by_index {
        for spec in specs {
            if !actor_names.contains(spec.actor.as_str()) {
                return Err(LoaderError::OatfLoad(format!(
                    "await_extractors references non-existent actor: '{}'",
                    spec.actor
                )));
            }
        }
    }

    Ok(LoadedDocument {
        document,
        await_extractors: await_by_index,
    })
}

/// Returns the actors slice from a normalized OATF document.
///
/// This is the single place that asserts the post-normalization invariant
/// that `document.attack.execution.actors` is `Some`. All code that needs
/// the actors list should call this helper rather than inlining the
/// `expect()` at each call site.
///
/// For documents loaded via [`load_document()`], the invariant is validated
/// at load time and this function will never panic. For documents
/// constructed by other means (e.g., tests), it panics with a clear message.
///
/// # Panics
///
/// Panics if the document has no actors.
///
/// Implements: TJ-SPEC-013 F-001
#[must_use]
pub fn document_actors(document: &oatf::Document) -> &[oatf::Actor] {
    document
        .attack
        .execution
        .actors
        .as_deref()
        .expect("document should have actors after normalization (validated at load time)")
}

/// Detects circular dependencies in `await_extractors` configuration.
///
/// Builds a directed graph of actor → awaited actor edges and checks for
/// cycles using depth-first search. A cycle means two actors would block
/// waiting for each other, both timing out.
///
/// Implements: TJ-SPEC-015 EC-ORCH-003
fn detect_await_cycles(
    await_map: &HashMap<(String, usize), Vec<AwaitExtractor>>,
) -> Result<(), LoaderError> {
    // Build adjacency list: actor → set of actors it depends on
    let mut edges: HashMap<&str, HashSet<&str>> = HashMap::new();
    for ((actor_name, _), specs) in await_map {
        for spec in specs {
            edges
                .entry(actor_name.as_str())
                .or_default()
                .insert(spec.actor.as_str());
        }
    }

    // DFS cycle detection
    let mut visited: HashSet<&str> = HashSet::new();
    let mut in_stack: HashSet<&str> = HashSet::new();

    for &actor in edges.keys() {
        if !visited.contains(actor) {
            let mut path = Vec::new();
            if has_cycle(actor, &edges, &mut visited, &mut in_stack, &mut path) {
                path.push(path[0]); // close the cycle for display
                return Err(LoaderError::CyclicDependency(path.join(" → ")));
            }
        }
    }

    Ok(())
}

/// Recursive DFS for cycle detection.
fn has_cycle<'a>(
    node: &'a str,
    edges: &HashMap<&'a str, HashSet<&'a str>>,
    visited: &mut HashSet<&'a str>,
    in_stack: &mut HashSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> bool {
    visited.insert(node);
    in_stack.insert(node);
    path.push(node);

    if let Some(deps) = edges.get(node) {
        for &dep in deps {
            if !visited.contains(dep) {
                if has_cycle(dep, edges, visited, in_stack, path) {
                    return true;
                }
            } else if in_stack.contains(dep) {
                // Found cycle — trim path to start from the cycle entry
                if let Some(pos) = path.iter().position(|&p| p == dep) {
                    *path = path[pos..].to_vec();
                }
                return true;
            }
        }
    }

    in_stack.remove(node);
    path.pop();
    false
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
        let key = ("actor1".to_string(), 0usize);
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

    #[test]
    fn detect_await_cycles_no_deps() {
        let map = HashMap::new();
        assert!(detect_await_cycles(&map).is_ok());
    }

    #[test]
    fn detect_await_cycles_linear_ok() {
        // A → B (no cycle)
        let mut map = HashMap::new();
        map.insert(
            ("actor_a".to_string(), 1),
            vec![AwaitExtractor {
                actor: "actor_b".to_string(),
                extractors: vec!["token".to_string()],
                timeout: Duration::from_secs(5),
            }],
        );
        assert!(detect_await_cycles(&map).is_ok());
    }

    #[test]
    fn detect_await_cycles_circular_detected() {
        // A → B and B → A (cycle)
        let mut map = HashMap::new();
        map.insert(
            ("actor_a".to_string(), 1),
            vec![AwaitExtractor {
                actor: "actor_b".to_string(),
                extractors: vec!["token".to_string()],
                timeout: Duration::from_secs(5),
            }],
        );
        map.insert(
            ("actor_b".to_string(), 0),
            vec![AwaitExtractor {
                actor: "actor_a".to_string(),
                extractors: vec!["secret".to_string()],
                timeout: Duration::from_secs(5),
            }],
        );
        let err = detect_await_cycles(&map).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("actor_a") && msg.contains("actor_b"),
            "Expected cycle mentioning both actors, got: {msg}"
        );
    }

    #[test]
    fn detect_await_cycles_self_cycle() {
        // A → A (self-cycle)
        let mut map = HashMap::new();
        map.insert(
            ("actor_a".to_string(), 0),
            vec![AwaitExtractor {
                actor: "actor_a".to_string(),
                extractors: vec!["token".to_string()],
                timeout: Duration::from_secs(5),
            }],
        );
        assert!(detect_await_cycles(&map).is_err());
    }

    // ========================================================================
    // Property-based tests
    // ========================================================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Generates a random DAG (forward-only edges via topological ordering).
        fn arb_dag(
            max_actors: usize,
        ) -> impl Strategy<Value = HashMap<(String, usize), Vec<AwaitExtractor>>> {
            (2..=max_actors)
                .prop_flat_map(|n| {
                    let names = (0..n).map(|i| format!("actor_{i}")).collect::<Vec<_>>();
                    // Generate edges: each node can have edges to nodes with higher index only
                    let edge_strategies: Vec<_> = (0..n)
                        .map(|from| {
                            let targets: Vec<String> = names[from + 1..].to_vec();
                            if targets.is_empty() {
                                Just(Vec::<String>::new()).boxed()
                            } else {
                                prop::collection::vec(prop::sample::select(targets), 0..=2)
                                    .prop_map(|mut v| {
                                        v.sort();
                                        v.dedup();
                                        v
                                    })
                                    .boxed()
                            }
                        })
                        .collect();

                    (Just(names), edge_strategies)
                })
                .prop_map(|(names, edges)| {
                    let mut map = HashMap::new();
                    for (from_idx, targets) in edges.into_iter().enumerate() {
                        if !targets.is_empty() {
                            let specs: Vec<AwaitExtractor> = targets
                                .into_iter()
                                .map(|target| AwaitExtractor {
                                    actor: target,
                                    extractors: vec!["token".to_string()],
                                    timeout: Duration::from_secs(5),
                                })
                                .collect();
                            map.insert((names[from_idx].clone(), 0), specs);
                        }
                    }
                    map
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn prop_dag_no_false_positive(dag in arb_dag(6)) {
                // Forward-only edges guarantee no cycles
                prop_assert!(detect_await_cycles(&dag).is_ok(),
                    "DAG with forward-only edges should never have cycles");
            }

            #[test]
            fn prop_cycle_always_detected(n in 2..=6_usize) {
                let names: Vec<String> = (0..n).map(|i| format!("actor_{i}")).collect();
                // Create a forward DAG with one injected back-edge
                let mut map: HashMap<(String, usize), Vec<AwaitExtractor>> = HashMap::new();

                // Forward edges: 0→1, 1→2, ..., (n-2)→(n-1)
                for i in 0..n - 1 {
                    map.insert(
                        (names[i].clone(), 0),
                        vec![AwaitExtractor {
                            actor: names[i + 1].clone(),
                            extractors: vec!["token".to_string()],
                            timeout: Duration::from_secs(5),
                        }],
                    );
                }

                // Back edge: (n-1)→0 — creates a cycle
                map.insert(
                    (names[n - 1].clone(), 0),
                    vec![AwaitExtractor {
                        actor: names[0].clone(),
                        extractors: vec!["token".to_string()],
                        timeout: Duration::from_secs(5),
                    }],
                );

                prop_assert!(detect_await_cycles(&map).is_err(),
                    "cycle 0→1→...→(n-1)→0 should be detected");
            }

            #[test]
            fn prop_self_loop_detected(name in "[a-z_]{1,10}") {
                let mut map = HashMap::new();
                map.insert(
                    (name.clone(), 0),
                    vec![AwaitExtractor {
                        actor: name,
                        extractors: vec!["token".to_string()],
                        timeout: Duration::from_secs(5),
                    }],
                );
                prop_assert!(detect_await_cycles(&map).is_err(),
                    "self-loop should always be detected");
            }

            #[test]
            fn prop_empty_graph_ok(_dummy in 0..1_u8) {
                let map: HashMap<(String, usize), Vec<AwaitExtractor>> = HashMap::new();
                prop_assert!(detect_await_cycles(&map).is_ok());
            }
        }
    }

    /// EC-OATF-006: Document with schema violation → `load_document()` returns
    /// `LoaderError::OatfLoad` with a descriptive message.
    #[test]
    fn ec_oatf_006_sdk_validation_error() {
        // Missing required `oatf:` version field — should fail SDK validation
        let yaml = r#"
attack:
  name: bad_document
  execution:
    mode: mcp_server
    state:
      tools:
        - name: calculator
          description: "test"
"#;

        let result = load_document(yaml);
        assert!(result.is_err(), "invalid document should produce error");

        let err = result.unwrap_err();
        match &err {
            LoaderError::OatfLoad(msg) => {
                assert!(
                    !msg.is_empty(),
                    "OatfLoad error should have a descriptive message, got empty"
                );
            }
            other => panic!("Expected LoaderError::OatfLoad, got: {other:?}"),
        }
    }

    /// OATF §5.5: `await_extractors` referencing a non-existent actor is rejected.
    #[test]
    fn await_extractors_nonexistent_actor_rejected() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: bad-await-ref
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            await_extractors:
              - actor: ghost
                extractors:
                  - token
                timeout: "5s"
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: client
        mode: mcp_client
        phases:
          - name: probe
            state:
              actions:
                - list_tools
"#;

        let err = load_document(yaml).expect_err("should reject non-existent actor reference");
        match &err {
            LoaderError::OatfLoad(msg) => {
                assert!(
                    msg.contains("ghost"),
                    "error should mention the bad actor name, got: {msg}"
                );
            }
            other => panic!("Expected LoaderError::OatfLoad, got: {other:?}"),
        }
    }
}
