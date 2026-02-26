//! Verdict output formatting: JSON file and human summary.
//!
//! Produces structured JSON verdict output (OATF §9.3) and a concise
//! human-readable summary on stderr.
//!
//! See TJ-SPEC-014 §5 for output format details.

use std::io::Write;
use std::path::Path;

use oatf::enums::{AttackResult, IndicatorResult};
use serde::Serialize;

use crate::engine::types::TerminationReason;
use crate::error::ExitCode;

// ============================================================================
// Verdict Output Structure
// ============================================================================

/// Full verdict output document combining attack metadata, verdict, and
/// execution summary.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct VerdictOutput {
    /// Attack metadata from the OATF document envelope.
    pub attack: AttackMetadata,
    /// The computed verdict.
    pub verdict: VerdictBlock,
    /// Execution summary with runtime metadata.
    pub execution_summary: ExecutionSummary,
}

/// Attack metadata extracted from the OATF document.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct AttackMetadata {
    /// Attack ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Attack name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Document version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// Severity information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<serde_json::Value>,
    /// Classification information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<serde_json::Value>,
}

/// The verdict block conforming to OATF §9.3.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct VerdictBlock {
    /// Overall attack result.
    pub result: String,
    /// Per-indicator verdicts.
    pub indicator_verdicts: Vec<IndicatorVerdictOutput>,
    /// Summary counts.
    pub evaluation_summary: EvaluationSummaryOutput,
    /// ISO 8601 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Tool identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Per-indicator verdict for JSON output.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct IndicatorVerdictOutput {
    /// Indicator ID.
    pub id: String,
    /// Result.
    pub result: String,
    /// Evidence string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Evaluation summary counts for JSON output.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct EvaluationSummaryOutput {
    /// Number matched.
    pub matched: i64,
    /// Number not matched.
    pub not_matched: i64,
    /// Number of errors.
    pub error: i64,
    /// Number skipped.
    pub skipped: i64,
}

/// Per-actor execution status for the output.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct ActorStatus {
    /// Actor name.
    pub name: String,
    /// Execution status.
    pub status: String,
    /// Phases completed.
    pub phases_completed: usize,
    /// Total phases.
    pub total_phases: usize,
    /// Terminal phase name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_phase: Option<String>,
    /// Error message (if status is "error").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Execution summary for JSON output.
///
/// Implements: TJ-SPEC-014 F-007
#[derive(Debug, Serialize)]
pub struct ExecutionSummary {
    /// Per-actor status array.
    pub actors: Vec<ActorStatus>,
    /// Grace period actually applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period_applied: Option<String>,
    /// Total messages in the protocol trace.
    pub trace_messages: usize,
    /// Total execution time in milliseconds.
    pub duration_ms: u64,
}

// ============================================================================
// Conversion from SDK types
// ============================================================================

/// Converts an `AttackResult` to its wire string representation.
///
/// Implements: TJ-SPEC-014 F-007
#[must_use]
pub fn attack_result_to_string(result: &AttackResult) -> String {
    match result {
        AttackResult::Exploited => "exploited".to_string(),
        AttackResult::NotExploited => "not_exploited".to_string(),
        AttackResult::Partial => "partial".to_string(),
        AttackResult::Error => "error".to_string(),
    }
}

/// Converts an `IndicatorResult` to its wire string representation.
#[must_use]
pub fn indicator_result_to_string(result: &IndicatorResult) -> String {
    match result {
        IndicatorResult::Matched => "matched".to_string(),
        IndicatorResult::NotMatched => "not_matched".to_string(),
        IndicatorResult::Error => "error".to_string(),
        IndicatorResult::Skipped => "skipped".to_string(),
    }
}

/// Converts a `TerminationReason` to a status string for the execution summary.
#[must_use]
pub fn termination_to_status(reason: &TerminationReason) -> String {
    match reason {
        TerminationReason::TerminalPhaseReached => "completed".to_string(),
        TerminationReason::Cancelled => "cancelled".to_string(),
        TerminationReason::MaxSessionExpired => "timeout".to_string(),
    }
}

/// Builds a `VerdictOutput` from SDK types and execution metadata.
///
/// Implements: TJ-SPEC-014 F-007
#[must_use]
pub fn build_verdict_output(
    attack: &oatf::Attack,
    verdict: &oatf::AttackVerdict,
    actors: Vec<ActorStatus>,
    grace_period_applied: Option<std::time::Duration>,
    trace_messages: usize,
    duration_ms: u64,
) -> VerdictOutput {
    let attack_metadata = AttackMetadata {
        id: attack.id.clone(),
        name: attack.name.clone(),
        version: attack.version,
        severity: attack
            .severity
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok()),
        classification: attack
            .classification
            .as_ref()
            .and_then(|c| serde_json::to_value(c).ok()),
    };

    let indicator_verdicts: Vec<IndicatorVerdictOutput> = verdict
        .indicator_verdicts
        .iter()
        .map(|iv| IndicatorVerdictOutput {
            id: iv.indicator_id.clone(),
            result: indicator_result_to_string(&iv.result),
            evidence: iv.evidence.clone(),
        })
        .collect();

    let verdict_block = VerdictBlock {
        result: attack_result_to_string(&verdict.result),
        indicator_verdicts,
        evaluation_summary: EvaluationSummaryOutput {
            matched: verdict.evaluation_summary.matched,
            not_matched: verdict.evaluation_summary.not_matched,
            error: verdict.evaluation_summary.error,
            skipped: verdict.evaluation_summary.skipped,
        },
        timestamp: verdict.timestamp.clone(),
        source: verdict.source.clone(),
    };

    let grace_str = grace_period_applied.map(|d| humantime::format_duration(d).to_string());

    let execution_summary = ExecutionSummary {
        actors,
        grace_period_applied: grace_str,
        trace_messages,
        duration_ms,
    };

    VerdictOutput {
        attack: attack_metadata,
        verdict: verdict_block,
        execution_summary,
    }
}

// ============================================================================
// JSON Output
// ============================================================================

/// Writes the verdict output as JSON to the specified path.
///
/// Use `"-"` for stdout.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or written.
///
/// Implements: TJ-SPEC-014 F-007
pub fn write_json_verdict(output: &VerdictOutput, path: &str) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(output).map_err(std::io::Error::other)?;

    if path == "-" {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle.write_all(json.as_bytes())?;
        handle.write_all(b"\n")?;
        handle.flush()?;
    } else {
        let path = Path::new(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, format!("{json}\n"))?;
    }

    Ok(())
}

// ============================================================================
// Human Summary (stderr)
// ============================================================================

/// Prints a human-readable verdict summary to stderr.
///
/// Implements: TJ-SPEC-014 F-008
pub fn print_human_summary(output: &VerdictOutput) {
    let stderr = std::io::stderr();
    let mut w = stderr.lock();

    // Header
    let attack_name = output.attack.name.as_deref().unwrap_or("(unnamed attack)");
    let attack_id = output.attack.id.as_deref().unwrap_or("");

    let _ = writeln!(w);
    if attack_id.is_empty() {
        let _ = writeln!(w, "  {attack_name}");
    } else {
        let _ = writeln!(w, "  {attack_id}: {attack_name}");
    }

    // Result line
    let result_str = &output.verdict.result;
    let symbol = match result_str.as_str() {
        "exploited" => "EXPLOITED",
        "not_exploited" => "NOT EXPLOITED",
        "partial" => "PARTIAL",
        "error" => "ERROR",
        _ => result_str.as_str(),
    };
    let _ = writeln!(w);
    let _ = writeln!(w, "  Result: {symbol}");

    // Indicators
    if !output.verdict.indicator_verdicts.is_empty() {
        let _ = writeln!(w);
        let _ = writeln!(w, "  Indicators:");
        for iv in &output.verdict.indicator_verdicts {
            let icon = match iv.result.as_str() {
                "matched" => "+",
                "not_matched" => "-",
                "skipped" => "o",
                "error" => "!",
                _ => "?",
            };
            let _ = writeln!(w, "    {icon} {} [{}]", iv.id, iv.result);
            if let Some(ref evidence) = iv.evidence {
                let _ = writeln!(w, "      {evidence}");
            }
        }
    }

    // Summary counts
    let s = &output.verdict.evaluation_summary;
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "  Summary: {} matched, {} not matched, {} errors, {} skipped",
        s.matched, s.not_matched, s.error, s.skipped
    );

    // Execution
    let duration_secs = output.execution_summary.duration_ms / 1000;
    let duration_tenths = (output.execution_summary.duration_ms % 1000) / 100;
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "  Execution: {} messages, {duration_secs}.{duration_tenths}s",
        output.execution_summary.trace_messages,
    );
    for actor in &output.execution_summary.actors {
        let icon = match actor.status.as_str() {
            "completed" => "+",
            "error" | "timeout" => "!",
            "cancelled" => "x",
            _ => "?",
        };
        let _ = writeln!(
            w,
            "    {icon} {}: {}/{} phases",
            actor.name, actor.phases_completed, actor.total_phases
        );
        if let Some(ref err) = actor.error {
            let _ = writeln!(w, "      {err}");
        }
    }

    let _ = writeln!(w);
}

// ============================================================================
// Exit Code Mapping
// ============================================================================

/// Maps an `AttackResult` to the corresponding process exit code.
///
/// - `NotExploited` → 0 (success)
/// - `Exploited` → 1 (failure)
/// - `Error` → 2 (evaluation error)
/// - `Partial` → 3 (partial)
///
/// Implements: TJ-SPEC-014 F-009
#[must_use]
pub const fn verdict_exit_code(result: &AttackResult) -> i32 {
    match result {
        AttackResult::NotExploited => ExitCode::NOT_EXPLOITED,
        AttackResult::Exploited => ExitCode::EXPLOITED,
        AttackResult::Error => ExitCode::ERROR,
        AttackResult::Partial => ExitCode::PARTIAL,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_verdict_output(result: &str) -> VerdictOutput {
        VerdictOutput {
            attack: AttackMetadata {
                id: Some("TJ-TEST-001".to_string()),
                name: Some("Test Attack".to_string()),
                version: Some(1),
                severity: None,
                classification: None,
            },
            verdict: VerdictBlock {
                result: result.to_string(),
                indicator_verdicts: vec![
                    IndicatorVerdictOutput {
                        id: "ind-1".to_string(),
                        result: "matched".to_string(),
                        evidence: Some("found malicious content".to_string()),
                    },
                    IndicatorVerdictOutput {
                        id: "ind-2".to_string(),
                        result: "skipped".to_string(),
                        evidence: Some("No inference engine configured".to_string()),
                    },
                ],
                evaluation_summary: EvaluationSummaryOutput {
                    matched: 1,
                    not_matched: 0,
                    error: 0,
                    skipped: 1,
                },
                timestamp: Some("2026-02-26T00:00:00Z".to_string()),
                source: Some("thoughtjack/0.5.0".to_string()),
            },
            execution_summary: ExecutionSummary {
                actors: vec![ActorStatus {
                    name: "mcp_poison".to_string(),
                    status: "completed".to_string(),
                    phases_completed: 2,
                    total_phases: 2,
                    terminal_phase: Some("exploit".to_string()),
                    error: None,
                }],
                grace_period_applied: Some("30s".to_string()),
                trace_messages: 12,
                duration_ms: 4520,
            },
        }
    }

    // ── Exit code mapping ───────────────────────────────────────────────

    #[test]
    fn exit_code_not_exploited() {
        assert_eq!(verdict_exit_code(&AttackResult::NotExploited), 0);
    }

    #[test]
    fn exit_code_exploited() {
        assert_eq!(verdict_exit_code(&AttackResult::Exploited), 1);
    }

    #[test]
    fn exit_code_error() {
        assert_eq!(verdict_exit_code(&AttackResult::Error), 2);
    }

    #[test]
    fn exit_code_partial() {
        assert_eq!(verdict_exit_code(&AttackResult::Partial), 3);
    }

    // ── Result string conversion ────────────────────────────────────────

    #[test]
    fn attack_result_strings() {
        assert_eq!(
            attack_result_to_string(&AttackResult::Exploited),
            "exploited"
        );
        assert_eq!(
            attack_result_to_string(&AttackResult::NotExploited),
            "not_exploited"
        );
        assert_eq!(attack_result_to_string(&AttackResult::Partial), "partial");
        assert_eq!(attack_result_to_string(&AttackResult::Error), "error");
    }

    #[test]
    fn indicator_result_strings() {
        assert_eq!(
            indicator_result_to_string(&IndicatorResult::Matched),
            "matched"
        );
        assert_eq!(
            indicator_result_to_string(&IndicatorResult::NotMatched),
            "not_matched"
        );
        assert_eq!(indicator_result_to_string(&IndicatorResult::Error), "error");
        assert_eq!(
            indicator_result_to_string(&IndicatorResult::Skipped),
            "skipped"
        );
    }

    // ── Termination status mapping ──────────────────────────────────────

    #[test]
    fn termination_status_strings() {
        assert_eq!(
            termination_to_status(&TerminationReason::TerminalPhaseReached),
            "completed"
        );
        assert_eq!(
            termination_to_status(&TerminationReason::Cancelled),
            "cancelled"
        );
        assert_eq!(
            termination_to_status(&TerminationReason::MaxSessionExpired),
            "timeout"
        );
    }

    // ── JSON output ─────────────────────────────────────────────────────

    #[test]
    fn json_output_serializes_correctly() {
        let output = make_verdict_output("exploited");
        let json = serde_json::to_string_pretty(&output).unwrap();

        // Parse back and verify key fields
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["attack"]["id"], "TJ-TEST-001");
        assert_eq!(parsed["verdict"]["result"], "exploited");
        assert_eq!(
            parsed["verdict"]["indicator_verdicts"][0]["result"],
            "matched"
        );
        assert_eq!(parsed["verdict"]["evaluation_summary"]["matched"], 1);
        assert_eq!(parsed["execution_summary"]["trace_messages"], 12);
        assert_eq!(parsed["execution_summary"]["duration_ms"], 4520);
    }

    #[test]
    fn json_output_write_to_temp_file() {
        let output = make_verdict_output("not_exploited");
        let dir = std::env::temp_dir().join("thoughtjack_test_output");
        let path = dir.join("verdict.json");

        // Clean up from previous runs
        let _ = std::fs::remove_dir_all(&dir);

        write_json_verdict(&output, path.to_str().unwrap()).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["verdict"]["result"], "not_exploited");

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── EC-VERDICT-011: Unicode in Evidence ─────────────────────────────

    #[test]
    fn ec_verdict_011_unicode_in_evidence() {
        let output = VerdictOutput {
            attack: AttackMetadata {
                id: Some("TJ-TEST-001".to_string()),
                name: Some("Unicode Test".to_string()),
                version: Some(1),
                severity: None,
                classification: None,
            },
            verdict: VerdictBlock {
                result: "exploited".to_string(),
                indicator_verdicts: vec![IndicatorVerdictOutput {
                    id: "ind-1".to_string(),
                    result: "matched".to_string(),
                    evidence: Some(
                        "Tool name: \u{4e2d}\u{6587}\u{5de5}\u{5177} matched".to_string(),
                    ),
                }],
                evaluation_summary: EvaluationSummaryOutput {
                    matched: 1,
                    not_matched: 0,
                    error: 0,
                    skipped: 0,
                },
                timestamp: None,
                source: None,
            },
            execution_summary: ExecutionSummary {
                actors: vec![],
                grace_period_applied: None,
                trace_messages: 1,
                duration_ms: 100,
            },
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\u{4e2d}\u{6587}\u{5de5}\u{5177}"));
    }

    // ── EC-VERDICT-012: Permission Denied ───────────────────────────────

    #[test]
    fn ec_verdict_012_permission_denied() {
        let output = make_verdict_output("exploited");
        let result = write_json_verdict(&output, "/nonexistent_root_path/verdict.json");
        assert!(result.is_err());
    }

    // ── Human summary ───────────────────────────────────────────────────

    #[test]
    fn human_summary_does_not_panic() {
        // Just verify it doesn't panic — output goes to stderr.
        let output = make_verdict_output("exploited");
        print_human_summary(&output);
    }

    #[test]
    fn human_summary_empty_indicators() {
        let output = VerdictOutput {
            attack: AttackMetadata {
                id: None,
                name: None,
                version: None,
                severity: None,
                classification: None,
            },
            verdict: VerdictBlock {
                result: "not_exploited".to_string(),
                indicator_verdicts: vec![],
                evaluation_summary: EvaluationSummaryOutput {
                    matched: 0,
                    not_matched: 0,
                    error: 0,
                    skipped: 0,
                },
                timestamp: None,
                source: None,
            },
            execution_summary: ExecutionSummary {
                actors: vec![],
                grace_period_applied: None,
                trace_messages: 0,
                duration_ms: 0,
            },
        };
        print_human_summary(&output);
    }

    // ── build_verdict_output ────────────────────────────────────────────

    #[test]
    fn build_verdict_output_populates_fields() {
        let attack = oatf::Attack {
            id: Some("ATK-001".to_string()),
            name: Some("Test".to_string()),
            version: Some(1),
            status: None,
            created: None,
            modified: None,
            author: None,
            description: None,
            grace_period: None,
            severity: None,
            impact: None,
            classification: None,
            references: None,
            execution: oatf::Execution {
                mode: None,
                state: None,
                phases: None,
                actors: None,
                extensions: std::collections::HashMap::new(),
            },
            indicators: None,
            correlation: None,
            extensions: std::collections::HashMap::new(),
        };

        let verdict = oatf::AttackVerdict {
            attack_id: Some("ATK-001".to_string()),
            result: AttackResult::NotExploited,
            indicator_verdicts: vec![],
            evaluation_summary: oatf::EvaluationSummary {
                matched: 0,
                not_matched: 0,
                error: 0,
                skipped: 0,
            },
            timestamp: Some("2026-01-01T00:00:00Z".to_string()),
            source: Some("thoughtjack/0.5.0".to_string()),
        };

        let actors = vec![ActorStatus {
            name: "actor1".to_string(),
            status: "completed".to_string(),
            phases_completed: 2,
            total_phases: 2,
            terminal_phase: Some("terminal".to_string()),
            error: None,
        }];

        let output = build_verdict_output(
            &attack,
            &verdict,
            actors,
            Some(std::time::Duration::from_secs(30)),
            10,
            1500,
        );

        assert_eq!(output.attack.id.as_deref(), Some("ATK-001"));
        assert_eq!(output.verdict.result, "not_exploited");
        assert_eq!(output.execution_summary.trace_messages, 10);
        assert_eq!(output.execution_summary.duration_ms, 1500);
        assert!(output.execution_summary.grace_period_applied.is_some());
    }
}
