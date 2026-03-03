//! Run command handler (TJ-SPEC-007 v2)
//!
//! Executes an OATF scenario against a target agent. Loads the document,
//! runs orchestration (or single-actor shortcut), evaluates the verdict,
//! and outputs results.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::cli::args::RunArgs;
use crate::engine::trace::SharedTrace;
use crate::engine::types::AwaitExtractor;
use crate::error::ThoughtJackError;
use crate::loader::{self, LoadedDocument, document_actors};
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::orchestration::orchestrator::{ActorOutcome, orchestrate};
use crate::orchestration::runner::{ActorConfig, build_actor_config, run_actor};
use crate::orchestration::store::ExtractorStore;
use crate::verdict::evaluation::{ActorInfo, EvaluationConfig, evaluate_verdict};
use crate::verdict::grace::resolve_grace_period;
use crate::verdict::output::{
    ActorStatus, build_verdict_output, print_human_summary, termination_to_status,
    verdict_exit_code, write_json_verdict,
};

/// Execute an OATF scenario.
///
/// Loads the OATF document, runs the orchestration pipeline, evaluates
/// the verdict against the protocol trace, and outputs results.
///
/// # Errors
///
/// Returns an error if scenario loading, execution, or verdict output fails.
///
/// Implements: TJ-SPEC-007 F-002
pub async fn run(
    args: &RunArgs,
    quiet: bool,
    cancel: CancellationToken,
) -> Result<(), ThoughtJackError> {
    let config = args.config.as_ref().ok_or_else(|| {
        ThoughtJackError::Usage("--config <path> is required for `run`".into())
    })?;
    let yaml = std::fs::read_to_string(config)?;
    run_from_yaml(&yaml, args, quiet, cancel).await
}

/// Execute an OATF scenario from raw YAML content.
///
/// Shared implementation for both `run` (reads from file) and
/// `scenarios run` (uses built-in YAML).
///
/// # Errors
///
/// Returns an error if scenario loading, execution, or verdict output fails.
///
/// Implements: TJ-SPEC-007 F-002
pub async fn run_from_yaml(
    yaml: &str,
    args: &RunArgs,
    quiet: bool,
    cancel: CancellationToken,
) -> Result<(), ThoughtJackError> {
    // EC-CLI-010: warn when synthesize validation is bypassed
    if args.raw_synthesize {
        tracing::warn!("Synthesize output validation disabled (--raw-synthesize)");
    }

    // 1. Load document
    let loaded = loader::load_document(yaml)?;

    // 2. Build ActorConfig from RunArgs
    let config = build_actor_config(args);

    // 3. Set up EventEmitter
    let events: Arc<EventEmitter> = match &args.events_file {
        Some(path) => Arc::new(EventEmitter::from_file(path)?),
        None => Arc::new(EventEmitter::noop()),
    };

    // 4. Get actors
    let actors = loaded
        .document
        .attack
        .execution
        .actors
        .as_ref()
        .ok_or_else(|| ThoughtJackError::Usage("no actors in document".into()))?;

    // 5. Execute: single-actor bypass or multi-actor orchestrate
    let start = Instant::now();
    let (outcomes, trace) = if actors.len() <= 1 {
        run_single_actor(&loaded, &config, &events, cancel).await?
    } else {
        let result = orchestrate(&loaded, &config, &events, cancel)
            .await
            .map_err(|e| ThoughtJackError::Orchestration(e.to_string()))?;
        (result.outcomes, result.trace)
    };
    #[allow(clippy::cast_possible_truncation)]
    let duration_ms = start.elapsed().as_millis() as u64;

    // 6. Build ActorInfo list for verdict
    let actor_infos: Vec<ActorInfo> = actors
        .iter()
        .map(|a| ActorInfo {
            name: a.name.clone(),
            mode: a.mode.clone(),
        })
        .collect();

    // 7. Evaluate verdict
    let trace_snapshot = trace.snapshot();
    let eval_config = EvaluationConfig {
        cel_evaluator: None,
        semantic_evaluator: None,
        no_semantic: args.no_semantic,
    };
    let source = format!("thoughtjack/{}", env!("CARGO_PKG_VERSION"));
    let verdict = evaluate_verdict(
        &loaded.document.attack,
        &trace_snapshot,
        &actor_infos,
        &eval_config,
        &source,
    );

    // 8. Build actor statuses from outcomes
    let actor_statuses = build_actor_statuses(&outcomes, actors);

    // 9. Resolve grace period
    let grace_applied = resolve_grace_period(
        args.grace_period.map(Into::into),
        loaded.document.attack.grace_period.as_deref(),
    );

    // 10. Build output
    let output = build_verdict_output(
        &loaded.document.attack,
        &verdict,
        actor_statuses,
        Some(grace_applied),
        trace_snapshot.len(),
        duration_ms,
    );

    // 11. Write JSON verdict if --output
    if let Some(ref path) = args.output {
        write_json_verdict(&output, path)?;
    }

    // 12. Print human summary
    if !quiet {
        print_human_summary(&output);
    }

    // 13. Exit code
    let code = verdict_exit_code(&verdict.result);
    if code != 0 {
        return Err(ThoughtJackError::Verdict {
            message: output.verdict.result,
            code,
        });
    }

    Ok(())
}

/// Single-actor shortcut: no orchestrator, no readiness gate.
///
/// Runs the actor directly and returns its outcome.
async fn run_single_actor(
    loaded: &LoadedDocument,
    config: &ActorConfig,
    events: &EventEmitter,
    cancel: CancellationToken,
) -> Result<(Vec<ActorOutcome>, SharedTrace), ThoughtJackError> {
    let actors = document_actors(&loaded.document);

    let actor_name = &actors[0].name;

    // Build per-actor await_extractors config
    let await_cfg: HashMap<usize, Vec<AwaitExtractor>> = loaded
        .await_extractors
        .iter()
        .filter(|((name, _), _)| name == actor_name)
        .map(|((_, phase_idx), specs)| (*phase_idx, specs.clone()))
        .collect();

    let trace = SharedTrace::new();
    let extractor_store = ExtractorStore::new();

    let result = run_actor(
        0,
        loaded.document.clone(),
        config,
        trace.clone(),
        extractor_store,
        await_cfg,
        cancel,
        None,
        None,
        events,
    )
    .await
    .map_err(|e| ThoughtJackError::Orchestration(e.to_string()))?;

    events.emit(ThoughtJackEvent::OrchestratorCompleted {
        summary: format!("single actor '{}' completed", result.actor_name),
    });

    Ok((vec![ActorOutcome::Success(result)], trace))
}

/// Converts actor outcomes into `ActorStatus` entries.
///
/// For successful actors, uses fields directly from `ActorResult`.
/// For failed/panicked actors, falls back to the actor definitions
/// for `total_phases`.
fn build_actor_statuses(outcomes: &[ActorOutcome], actors: &[oatf::Actor]) -> Vec<ActorStatus> {
    outcomes
        .iter()
        .map(|outcome| match outcome {
            ActorOutcome::Success(result) => ActorStatus {
                name: result.actor_name.clone(),
                status: termination_to_status(&result.termination),
                phases_completed: result.phases_completed,
                total_phases: result.total_phases,
                terminal_phase: result.final_phase.clone(),
                error: None,
            },
            ActorOutcome::Error { actor_name, error } => {
                let total_phases = actors
                    .iter()
                    .find(|a| &a.name == actor_name)
                    .map_or(0, |a| a.phases.len());
                ActorStatus {
                    name: actor_name.clone(),
                    status: "error".to_string(),
                    phases_completed: 0,
                    total_phases,
                    terminal_phase: None,
                    error: Some(error.clone()),
                }
            }
            ActorOutcome::Panic { actor_name } => {
                let total_phases = actors
                    .iter()
                    .find(|a| &a.name == actor_name)
                    .map_or(0, |a| a.phases.len());
                ActorStatus {
                    name: actor_name.clone(),
                    status: "error".to_string(),
                    phases_completed: 0,
                    total_phases,
                    terminal_phase: None,
                    error: Some("task panicked".to_string()),
                }
            }
        })
        .collect()
}
