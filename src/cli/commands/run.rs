//! Run command handler (TJ-SPEC-007 v2)
//!
//! Executes an OATF scenario against a target agent. Loads the document,
//! runs orchestration (or single-actor shortcut), evaluates the verdict,
//! and outputs results.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::cli::args::RunArgs;
use crate::engine::trace::SharedTrace;
use crate::engine::types::{ActorResult, AwaitExtractor, TerminationReason};
use crate::error::{EngineError, ThoughtJackError};
use crate::loader::{self, LoadedDocument, document_actors};
use crate::observability::events::{EventEmitter, ThoughtJackEvent};
use crate::observability::init_metrics;
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
    let config = args
        .config
        .as_ref()
        .ok_or_else(|| ThoughtJackError::Usage("--config <path> is required for `run`".into()))?;
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

    // 1. Initialize metrics (idempotent no-op if already initialized).
    init_metrics(args.metrics_port)?;

    // 2. Load document
    let loaded = loader::load_document(yaml)?;

    // 3. Build ActorConfig from RunArgs
    let config = build_actor_config(args).map_err(|e| match e {
        EngineError::Driver(msg) => ThoughtJackError::Usage(msg),
        other => ThoughtJackError::Usage(other.to_string()),
    })?;

    // 4. Set up EventEmitter
    let events: Arc<EventEmitter> = match &args.events_file {
        Some(path) => Arc::new(EventEmitter::from_file(path)?),
        None => Arc::new(EventEmitter::noop()),
    };

    // 5. Get actors
    let actors = loaded
        .document
        .attack
        .execution
        .actors
        .as_ref()
        .ok_or_else(|| ThoughtJackError::Usage("no actors in document".into()))?;

    // 6. Execute: single-actor bypass or multi-actor orchestrate
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

    // 7. Build ActorInfo list for verdict
    let actor_infos: Vec<ActorInfo> = actors
        .iter()
        .map(|a| ActorInfo {
            name: a.name.clone(),
            mode: a.mode.clone(),
        })
        .collect();

    // 8. Evaluate verdict
    let trace_snapshot = trace.snapshot();
    let cel = oatf::evaluate::default_cel_evaluator();
    let eval_config = EvaluationConfig {
        cel_evaluator: Some(&cel),
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

    // 9. Build actor statuses from outcomes
    let actor_statuses = build_actor_statuses(&outcomes, actors);

    // 10. Resolve grace period
    let grace_applied = resolve_grace_period(
        args.grace_period.map(Into::into),
        loaded.document.attack.grace_period.as_deref(),
    );

    // 11. Build output
    let output = build_verdict_output(
        &loaded.document.attack,
        &verdict,
        actor_statuses,
        Some(grace_applied),
        trace_snapshot.len(),
        duration_ms,
    );

    // 12. Write JSON verdict if --output
    if let Some(ref path) = args.output {
        write_json_verdict(&output, path)?;
    }

    // 13. Print human summary
    if !quiet {
        print_human_summary(&output);
    }

    // 14. Runtime actor failures are treated as orchestration errors.
    let actor_failures = summarize_actor_failures(&outcomes);
    if !actor_failures.is_empty() {
        return Err(ThoughtJackError::Orchestration(format!(
            "actor execution failed: {}",
            actor_failures.join("; ")
        )));
    }

    // 15. Exit code
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
    events: &Arc<EventEmitter>,
    cancel: CancellationToken,
) -> Result<(Vec<ActorOutcome>, SharedTrace), ThoughtJackError> {
    let actors = document_actors(&loaded.document);

    let actor_name = actors[0].name.clone();
    let total_phases = actors[0].phases.len();

    // Build per-actor await_extractors config
    let await_cfg: HashMap<usize, Vec<AwaitExtractor>> = loaded
        .await_extractors
        .iter()
        .filter(|((name, _), _)| name == &actor_name)
        .map(|((_, phase_idx), specs)| (*phase_idx, specs.clone()))
        .collect();

    let trace = SharedTrace::new();
    let extractor_store = ExtractorStore::new();

    let actor_cancel = cancel.child_token();
    let cfg = config.clone();
    let task_actor_cancel = actor_cancel.clone();
    let task_events = Arc::clone(events);
    let document = loaded.document.clone();
    let trace_for_actor = trace.clone();
    let mut actor_handle = tokio::spawn(async move {
        run_actor(
            0,
            document,
            &cfg,
            trace_for_actor,
            extractor_store,
            await_cfg,
            task_actor_cancel,
            None,
            None,
            &task_events,
        )
        .await
    });

    let outcome = tokio::select! {
        result = &mut actor_handle => {
            unpack_single_actor_join(result, &actor_name, total_phases)
        }
        () = tokio::time::sleep(config.max_session) => {
            actor_cancel.cancel();
            mark_timeout_outcome(
                wait_for_single_actor_shutdown(&mut actor_handle, &actor_name, total_phases).await,
            )
        }
        () = cancel.cancelled() => {
            actor_cancel.cancel();
            wait_for_single_actor_shutdown(&mut actor_handle, &actor_name, total_phases).await
        }
    };

    let summary = match &outcome {
        ActorOutcome::Success(result) => {
            format!(
                "single actor '{}' completed ({})",
                result.actor_name, result.termination
            )
        }
        ActorOutcome::Error { actor_name, error } => {
            format!("single actor '{actor_name}' failed ({error})")
        }
        ActorOutcome::Panic { actor_name } => {
            format!("single actor '{actor_name}' panicked")
        }
    };
    events.emit(ThoughtJackEvent::OrchestratorCompleted { summary });

    Ok((vec![outcome], trace))
}

fn unpack_single_actor_join(
    join_result: Result<Result<ActorResult, crate::error::EngineError>, tokio::task::JoinError>,
    actor_name: &str,
    total_phases: usize,
) -> ActorOutcome {
    match join_result {
        Ok(Ok(result)) => ActorOutcome::Success(result),
        Ok(Err(err)) => ActorOutcome::Error {
            actor_name: actor_name.to_string(),
            error: err.to_string(),
        },
        Err(join_err) if join_err.is_cancelled() => {
            ActorOutcome::Success(cancelled_actor_result(actor_name, total_phases))
        }
        Err(_join_err) => ActorOutcome::Panic {
            actor_name: actor_name.to_string(),
        },
    }
}

async fn wait_for_single_actor_shutdown(
    handle: &mut tokio::task::JoinHandle<Result<ActorResult, crate::error::EngineError>>,
    actor_name: &str,
    total_phases: usize,
) -> ActorOutcome {
    const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
    match tokio::time::timeout(SHUTDOWN_WAIT, &mut *handle).await {
        Ok(join_result) => unpack_single_actor_join(join_result, actor_name, total_phases),
        Err(_elapsed) => {
            handle.abort();
            ActorOutcome::Success(cancelled_actor_result(actor_name, total_phases))
        }
    }
}

fn mark_timeout_outcome(outcome: ActorOutcome) -> ActorOutcome {
    match outcome {
        ActorOutcome::Success(mut result) => {
            if result.termination == TerminationReason::Cancelled {
                result.termination = TerminationReason::MaxSessionExpired;
            }
            ActorOutcome::Success(result)
        }
        other => other,
    }
}

fn cancelled_actor_result(actor_name: &str, total_phases: usize) -> ActorResult {
    ActorResult {
        actor_name: actor_name.to_string(),
        termination: TerminationReason::Cancelled,
        phases_completed: 0,
        total_phases,
        final_phase: None,
    }
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

/// Summarizes runtime actor failures from orchestration outcomes.
fn summarize_actor_failures(outcomes: &[ActorOutcome]) -> Vec<String> {
    let mut failures: Vec<String> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            ActorOutcome::Error { actor_name, error } => Some(format!("{actor_name}: {error}")),
            ActorOutcome::Panic { actor_name } => Some(format!("{actor_name}: task panicked")),
            ActorOutcome::Success(_) => None,
        })
        .collect();

    if failures.is_empty() {
        let successful: Vec<&ActorResult> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                ActorOutcome::Success(result) => Some(result),
                ActorOutcome::Error { .. } | ActorOutcome::Panic { .. } => None,
            })
            .collect();

        let any_completed = successful.iter().any(|result| {
            !matches!(
                result.termination,
                TerminationReason::Cancelled | TerminationReason::MaxSessionExpired
            )
        });

        if !successful.is_empty() && !any_completed {
            let cancelled = successful
                .iter()
                .filter(|result| result.termination == TerminationReason::Cancelled)
                .count();
            let timed_out = successful
                .iter()
                .filter(|result| result.termination == TerminationReason::MaxSessionExpired)
                .count();
            failures.push(format!(
                "all actors terminated by cancellation/timeout before completion (cancelled: {cancelled}, timeout: {timed_out})"
            ));
        }
    }

    failures
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn test_run_args(max_session: Duration) -> RunArgs {
        RunArgs {
            config: None,
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(max_session),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(5)),
            output: None,
            header: vec![],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        }
    }

    #[test]
    fn summarize_actor_failures_ignores_success() {
        let outcomes = vec![ActorOutcome::Success(crate::engine::types::ActorResult {
            actor_name: "ok".to_string(),
            termination: crate::engine::types::TerminationReason::TerminalPhaseReached,
            phases_completed: 1,
            total_phases: 1,
            final_phase: Some("done".to_string()),
        })];
        assert!(summarize_actor_failures(&outcomes).is_empty());
    }

    #[test]
    fn summarize_actor_failures_all_cancelled_returns_failure() {
        let outcomes = vec![ActorOutcome::Success(crate::engine::types::ActorResult {
            actor_name: "server".to_string(),
            termination: crate::engine::types::TerminationReason::Cancelled,
            phases_completed: 0,
            total_phases: 2,
            final_phase: None,
        })];
        let failures = summarize_actor_failures(&outcomes);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("cancellation/timeout"));
    }

    #[test]
    fn summarize_actor_failures_mixed_completion_is_not_failure() {
        let outcomes = vec![
            ActorOutcome::Success(crate::engine::types::ActorResult {
                actor_name: "server".to_string(),
                termination: crate::engine::types::TerminationReason::Cancelled,
                phases_completed: 0,
                total_phases: 2,
                final_phase: None,
            }),
            ActorOutcome::Success(crate::engine::types::ActorResult {
                actor_name: "client".to_string(),
                termination: crate::engine::types::TerminationReason::TerminalPhaseReached,
                phases_completed: 1,
                total_phases: 1,
                final_phase: Some("done".to_string()),
            }),
        ];
        assert!(summarize_actor_failures(&outcomes).is_empty());
    }

    #[tokio::test]
    async fn run_from_yaml_returns_error_when_actor_fails() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: actor-runtime-failure
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: client
        mode: ag_ui_client
        phases:
          - name: probe
            state:
              run_agent_input:
                messages:
                  - role: user
                    content: "hello"
"#;

        let mut args = test_run_args(Duration::from_millis(500));
        args.mcp_server = Some("127.0.0.1:0".to_string());

        let err = run_from_yaml(yaml, &args, true, CancellationToken::new())
            .await
            .expect_err("actor runtime failure should bubble up as orchestration error");

        match err {
            ThoughtJackError::Orchestration(msg) => {
                assert!(msg.contains("actor execution failed"));
                assert!(msg.contains("client"));
            }
            other => panic!("expected orchestration error, got {other}"),
        }
    }

    #[tokio::test]
    async fn run_from_yaml_single_actor_respects_max_session() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: single-max-session
  execution:
    mode: mcp_server
    phases:
      - name: long_running
        state:
          tools:
            - name: test_tool
              description: "test"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 999
      - name: terminal
"#;

        let args = test_run_args(Duration::from_millis(250));

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_from_yaml(yaml, &args, true, CancellationToken::new()),
        )
        .await;
        assert!(result.is_ok(), "single-actor run exceeded timeout window");
        let err = result
            .unwrap()
            .expect_err("single-actor timeout should be reported as a runtime failure");
        match err {
            ThoughtJackError::Orchestration(msg) => {
                assert!(
                    msg.contains("cancellation/timeout"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected orchestration error, got {other}"),
        }
    }

    #[tokio::test]
    async fn run_from_yaml_invalid_header_returns_usage_error() {
        let yaml = r#"
oatf: "0.1"
attack:
  name: invalid-header-test
  execution:
    mode: mcp_server
    state:
      tools:
        - name: test_tool
          description: "test"
          inputSchema:
            type: object
"#;

        let mut args = test_run_args(Duration::from_secs(1));
        args.header = vec!["MissingColon".to_string()];

        let err = run_from_yaml(yaml, &args, true, CancellationToken::new())
            .await
            .expect_err("invalid --header should fail with usage error");

        match err {
            ThoughtJackError::Usage(msg) => {
                assert!(
                    msg.contains("expected KEY:VALUE"),
                    "unexpected usage error: {msg}"
                );
            }
            other => panic!("expected usage error, got {other}"),
        }
    }
}
