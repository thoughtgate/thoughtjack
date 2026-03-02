//! End-to-end verdict pipeline integration tests.
//!
//! In-process only — constructs traces and indicators, calls
//! `evaluate_verdict()`, verifies results.

use chrono::Utc;
use oatf::enums::{AttackResult, CorrelationLogic};
use serde_json::json;

use thoughtjack::engine::trace::TraceEntry;
use thoughtjack::engine::types::Direction;
use thoughtjack::verdict::{ActorInfo, EvaluationConfig, evaluate_verdict, extract_protocol};

// ============================================================================
// Helpers
// ============================================================================

fn make_trace_entry(
    actor: &str,
    method: &str,
    content: serde_json::Value,
    direction: Direction,
) -> TraceEntry {
    TraceEntry {
        seq: 0,
        timestamp: Utc::now(),
        actor: actor.to_string(),
        phase: "test".to_string(),
        direction,
        method: method.to_string(),
        content,
    }
}

fn make_actor(name: &str, mode: &str) -> ActorInfo {
    ActorInfo {
        name: name.to_string(),
        mode: mode.to_string(),
    }
}

fn make_attack(indicators: Option<Vec<oatf::Indicator>>) -> oatf::Attack {
    oatf::Attack {
        id: Some("test-attack".to_string()),
        name: Some("Test Attack".to_string()),
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
            mode: Some("mcp_server".to_string()),
            state: None,
            phases: None,
            actors: None,
            extensions: std::collections::HashMap::new(),
        },
        indicators,
        correlation: None,
        extensions: std::collections::HashMap::new(),
    }
}

fn make_pattern_indicator(id: &str, contains: &str, protocol: Option<&str>) -> oatf::Indicator {
    oatf::Indicator {
        id: Some(id.to_string()),
        protocol: protocol.map(String::from),
        surface: "tool_description".to_string(),
        description: None,
        pattern: Some(oatf::PatternMatch {
            target: Some("description".to_string()),
            condition: Some(oatf::Condition::Operators(oatf::MatchCondition {
                contains: Some(contains.to_string()),
                starts_with: None,
                ends_with: None,
                regex: None,
                any_of: None,
                gt: None,
                lt: None,
                gte: None,
                lte: None,
                exists: None,
            })),
            contains: None,
            starts_with: None,
            ends_with: None,
            regex: None,
            any_of: None,
            gt: None,
            lt: None,
            gte: None,
            lte: None,
        }),
        expression: None,
        semantic: None,
        confidence: None,
        severity: None,
        false_positives: None,
        extensions: std::collections::HashMap::new(),
    }
}

fn default_config() -> EvaluationConfig<'static> {
    EvaluationConfig {
        cel_evaluator: None,
        semantic_evaluator: None,
        no_semantic: false,
    }
}

// ============================================================================
// 1. Pattern exploited — /etc/passwd in tool call (EC-VERDICT-003)
// ============================================================================

#[test]
fn verdict_pattern_exploited() {
    let indicator = make_pattern_indicator("ind-1", "/etc/passwd", None);
    let attack = make_attack(Some(vec![indicator]));

    let trace = vec![make_trace_entry(
        "actor1",
        "tools/call",
        json!({"description": "Read file /etc/passwd"}),
        Direction::Incoming,
    )];
    let actors = vec![make_actor("actor1", "mcp_server")];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");
    assert_eq!(verdict.result, AttackResult::Exploited);
    assert_eq!(verdict.evaluation_summary.matched, 1);
}

// ============================================================================
// 2. Empty trace — all not_matched → not_exploited (EC-VERDICT-008)
// ============================================================================

#[test]
fn verdict_empty_trace() {
    let ind1 = make_pattern_indicator("ind-1", "malicious", None);
    let ind2 = make_pattern_indicator("ind-2", "exfiltrate", None);
    let attack = make_attack(Some(vec![ind1, ind2]));

    let trace: Vec<TraceEntry> = vec![];
    let actors = vec![make_actor("actor1", "mcp_server")];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");
    assert_eq!(verdict.result, AttackResult::NotExploited);
    assert_eq!(verdict.evaluation_summary.not_matched, 2);
}

// ============================================================================
// 3. `any` correlation — one match → exploited (EC-VERDICT-006)
// ============================================================================

#[test]
fn verdict_any_one_match() {
    let ind1 = make_pattern_indicator("ind-1", "malicious", None);
    let ind2 = make_pattern_indicator("ind-2", "nonexistent", None);
    let ind3 = make_pattern_indicator("ind-3", "also_nonexistent", None);
    let attack = make_attack(Some(vec![ind1, ind2, ind3]));

    let trace = vec![make_trace_entry(
        "actor1",
        "tools/call",
        json!({"description": "malicious payload"}),
        Direction::Incoming,
    )];
    let actors = vec![make_actor("actor1", "mcp_server")];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");
    assert_eq!(verdict.result, AttackResult::Exploited);
    assert_eq!(verdict.evaluation_summary.matched, 1);
    assert_eq!(verdict.evaluation_summary.not_matched, 2);
}

// ============================================================================
// 4. `all` correlation — mixed results → partial (EC-VERDICT-005)
// ============================================================================

#[test]
fn verdict_all_mixed() {
    let ind1 = make_pattern_indicator("ind-1", "malicious", None);
    let ind2 = make_pattern_indicator("ind-2", "exfiltrated", None);
    let ind3 = make_pattern_indicator("ind-3", "nonexistent_pattern", None);

    let mut attack = make_attack(Some(vec![ind1, ind2, ind3]));
    attack.correlation = Some(oatf::Correlation {
        logic: Some(CorrelationLogic::All),
    });

    let trace = vec![make_trace_entry(
        "actor1",
        "tools/call",
        json!({"description": "malicious exfiltrated payload"}),
        Direction::Incoming,
    )];
    let actors = vec![make_actor("actor1", "mcp_server")];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");
    // 2 matched + 1 not_matched under `all` → partial
    assert_eq!(verdict.result, AttackResult::Partial);
    assert_eq!(verdict.evaluation_summary.matched, 2);
    assert_eq!(verdict.evaluation_summary.not_matched, 1);
}

// ============================================================================
// 5. Protocol filter — multi-protocol trace (EC-VERDICT-009)
// ============================================================================

#[test]
fn verdict_protocol_filter() {
    // MCP indicator should only evaluate MCP actor entries
    let mcp_indicator = make_pattern_indicator("ind-mcp", "secret_data", Some("mcp"));

    // AG-UI indicator should only evaluate AG-UI actor entries
    let agui_indicator = make_pattern_indicator("ind-agui", "hidden_payload", Some("ag_ui"));

    let attack = make_attack(Some(vec![mcp_indicator, agui_indicator]));

    let trace = vec![
        // MCP entries — should be evaluated by mcp indicator
        make_trace_entry(
            "mcp_actor",
            "tools/call",
            json!({"description": "contains secret_data"}),
            Direction::Incoming,
        ),
        make_trace_entry(
            "mcp_actor",
            "tools/list",
            json!({"description": "safe content"}),
            Direction::Incoming,
        ),
        // AG-UI entries — should be evaluated by agui indicator
        make_trace_entry(
            "agui_actor",
            "run_finished",
            json!({"description": "normal response"}),
            Direction::Incoming,
        ),
    ];

    let actors = vec![
        make_actor("mcp_actor", "mcp_server"),
        make_actor("agui_actor", "ag_ui_client"),
    ];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");

    // MCP indicator should match (secret_data found)
    assert_eq!(verdict.evaluation_summary.matched, 1);
    // AG-UI indicator should not match (no hidden_payload in agui entries)
    assert_eq!(verdict.evaluation_summary.not_matched, 1);
    // Overall: any correlation with one match → exploited
    assert_eq!(verdict.result, AttackResult::Exploited);

    // Verify extract_protocol mapping works correctly
    assert_eq!(extract_protocol("mcp_server"), "mcp");
    assert_eq!(extract_protocol("mcp_client"), "mcp");
    assert_eq!(extract_protocol("ag_ui_client"), "ag_ui");
    assert_eq!(extract_protocol("a2a_server"), "a2a");
}

// ============================================================================
// 6. Grace period entries included — match in grace data (EC-VERDICT-007)
// ============================================================================

#[test]
fn verdict_grace_entries_included() {
    let indicator = make_pattern_indicator("ind-1", "exfiltrated", None);
    let attack = make_attack(Some(vec![indicator]));

    let trace = vec![
        // Pre-grace phase entry — no match
        make_trace_entry(
            "actor1",
            "tools/call",
            json!({"description": "safe content during normal execution"}),
            Direction::Incoming,
        ),
        // Grace period entry — matches (appended to trace during grace)
        make_trace_entry(
            "actor1",
            "tools/call",
            json!({"description": "exfiltrated data to external server"}),
            Direction::Incoming,
        ),
    ];
    let actors = vec![make_actor("actor1", "mcp_server")];

    let verdict = evaluate_verdict(&attack, &trace, &actors, &default_config(), "test/1.0");
    assert_eq!(verdict.result, AttackResult::Exploited);
    assert_eq!(verdict.evaluation_summary.matched, 1);
}
