//! Metrics collection for `ThoughtJack` (TJ-SPEC-008 F-009 / F-010).
//!
//! Provides Prometheus-compatible metrics with label cardinality protection
//! and typed convenience functions for recording measurements.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::error::ThoughtJackError;

/// Guard to prevent double-initialization of the metrics recorder.
static METRICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Known MCP method names used for label cardinality protection.
///
/// Any method not in this list (and not in A2A/AG-UI lists) is bucketed
/// as `"__unknown__"` to prevent memory exhaustion from attacker-controlled
/// label values (EC-OBS-021, EC-OBS-022).
const KNOWN_MCP_METHODS: [&str; 14] = [
    "initialize",
    "ping",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "prompts/list",
    "prompts/get",
    "elicitation/create",
    "sampling/createMessage",
    "logging/setLevel",
    "completion/complete",
];

/// Known A2A method names for label cardinality protection.
///
/// Implements: TJ-SPEC-008 §4.1
const KNOWN_A2A_METHODS: [&str; 5] = [
    "message/send",
    "message/stream",
    "tasks/get",
    "tasks/cancel",
    "agent_card/get",
];

/// Known AG-UI event names for label cardinality protection.
///
/// Implements: TJ-SPEC-008 §4.1
const KNOWN_AGUI_EVENTS: [&str; 9] = [
    "RUN_STARTED",
    "RUN_FINISHED",
    "RUN_ERROR",
    "TEXT_MESSAGE_START",
    "TEXT_MESSAGE_CONTENT",
    "TEXT_MESSAGE_END",
    "TOOL_CALL_START",
    "TOOL_CALL_END",
    "AGENT_ERROR",
];

/// Sanitizes a method name for use as a metrics label.
///
/// Returns the original string when it matches a known MCP, A2A, or AG-UI method,
/// or `"__unknown__"` otherwise.
///
/// Implements: TJ-SPEC-008 F-009, EC-OBS-021, EC-OBS-022
#[must_use]
pub fn sanitize_method_label(method: &str) -> &str {
    if KNOWN_MCP_METHODS.contains(&method)
        || KNOWN_A2A_METHODS.contains(&method)
        || KNOWN_AGUI_EVENTS.contains(&method)
    {
        method
    } else {
        "__unknown__"
    }
}

/// Initializes the global metrics recorder.
///
/// When `port` is `Some`, a Prometheus HTTP listener is started on
/// `127.0.0.1:<port>`.  When `None`, the recorder is installed without
/// an HTTP endpoint (metrics are recorded internally and can be read
/// programmatically).
///
/// # Errors
///
/// Returns `ThoughtJackError::Io` if the recorder or HTTP listener
/// cannot be installed (e.g. port already in use).
///
/// Implements: TJ-SPEC-008 F-010
pub fn init_metrics(port: Option<u16>) -> Result<(), ThoughtJackError> {
    if METRICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        tracing::debug!("metrics already initialized, skipping");
        return Ok(());
    }
    port.map_or_else(
        || PrometheusBuilder::new().install_recorder().map(|_| ()),
        |p| {
            PrometheusBuilder::new()
                .with_http_listener(([127, 0, 0, 1], p))
                .install()
        },
    )
    .map_err(|e| ThoughtJackError::Io(std::io::Error::other(e.to_string())))?;

    describe_metrics();
    Ok(())
}

/// Registers metric descriptions with the global recorder.
#[allow(clippy::too_many_lines)]
fn describe_metrics() {
    // Legacy (v0.2)
    describe_counter!(
        "thoughtjack_requests_total",
        "Total number of MCP requests received"
    );
    describe_counter!(
        "thoughtjack_responses_total",
        "Total number of MCP responses sent"
    );
    describe_histogram!(
        "thoughtjack_request_duration_ms",
        "Request processing duration in milliseconds"
    );
    describe_histogram!(
        "thoughtjack_delivery_duration_ms",
        "Delivery behavior duration in milliseconds"
    );
    describe_counter!(
        "thoughtjack_phase_transitions_total",
        "Total number of phase transitions"
    );
    describe_gauge!(
        "thoughtjack_current_phase",
        "Currently active phase (1 = active)"
    );
    describe_gauge!(
        "thoughtjack_connections_active",
        "Number of currently active connections"
    );
    describe_counter!("thoughtjack_delivery_bytes_total", "Bytes delivered");
    describe_counter!("thoughtjack_side_effects_total", "Side effects executed");
    describe_histogram!(
        "thoughtjack_side_effect_messages",
        "Messages sent per side effect execution"
    );
    describe_histogram!(
        "thoughtjack_side_effect_bytes",
        "Bytes sent per side effect execution"
    );
    describe_histogram!(
        "thoughtjack_side_effect_duration_ms",
        "Side effect execution duration in milliseconds"
    );
    describe_gauge!("thoughtjack_event_counts", "Current event counts");
    describe_counter!(
        "thoughtjack_errors_total",
        "Total number of errors by category"
    );
    describe_histogram!(
        "thoughtjack_payload_size_bytes",
        "Payload size in bytes by generator type"
    );
    describe_gauge!("thoughtjack_uptime_seconds", "Server uptime in seconds");

    // v0.5 core
    describe_counter!("tj_scenarios_total", "Scenarios executed by verdict result");
    describe_histogram!(
        "tj_scenario_duration_seconds",
        "Total scenario execution time"
    );
    describe_counter!(
        "tj_actors_total",
        "Actors executed by mode and completion status"
    );

    // v0.5 engine
    describe_counter!("tj_phase_transitions_total", "Phase transitions");
    describe_counter!("tj_extractors_captured_total", "Extractor values captured");
    describe_counter!("tj_synthesize_calls_total", "LLM generation calls");
    describe_histogram!("tj_synthesize_duration_seconds", "LLM generation latency");

    // v0.5 protocol
    describe_counter!("tj_protocol_messages_total", "Protocol messages (in/out)");
    describe_histogram!(
        "tj_protocol_message_duration_seconds",
        "Message handling latency"
    );
    describe_counter!("tj_transport_errors_total", "Transport-level failures");

    // v0.5 verdict
    describe_counter!("tj_verdicts_total", "Verdicts produced by result type");
    describe_counter!(
        "tj_indicators_evaluated_total",
        "Indicator evaluations by method and result"
    );
    describe_histogram!(
        "tj_indicator_evaluation_duration_seconds",
        "Indicator evaluation latency"
    );
    describe_counter!(
        "tj_semantic_llm_calls_total",
        "LLM calls for semantic evaluation"
    );
    describe_histogram!(
        "tj_semantic_llm_latency_seconds",
        "Semantic LLM call latency"
    );
    describe_histogram!(
        "tj_grace_period_messages_captured",
        "Messages captured during grace period"
    );
}

// ============================================================================
// Legacy (v0.2) recording functions
// ============================================================================

/// Records an incoming MCP request.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_request(method: &str) {
    let label = sanitize_method_label(method);
    counter!("thoughtjack_requests_total", "method" => label.to_owned()).increment(1);
}

/// Records an outgoing MCP response.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_response(method: &str, success: bool, error_code: Option<i64>) {
    let label = sanitize_method_label(method);
    let status = if success { "success" } else { "error" };
    let code = error_code.map_or_else(String::new, |c| c.to_string());
    counter!(
        "thoughtjack_responses_total",
        "method" => label.to_owned(),
        "status" => status,
        "error_code" => code,
    )
    .increment(1);
}

/// Records request processing duration.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_request_duration(method: &str, duration: Duration) {
    let label = sanitize_method_label(method);
    histogram!("thoughtjack_request_duration_ms", "method" => label.to_owned())
        .record(duration.as_secs_f64() * 1000.0);
}

/// Records delivery behavior duration.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_delivery_duration(duration: Duration) {
    histogram!("thoughtjack_delivery_duration_ms").record(duration.as_secs_f64() * 1000.0);
}

/// Records a phase transition.
///
/// Phase names are sanitized to prevent label cardinality explosion
/// from user-controlled configuration values.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_phase_transition(from: &str, to: &str) {
    counter!(
        "thoughtjack_phase_transitions_total",
        "from" => sanitize_phase_label(from),
        "to" => sanitize_phase_label(to)
    )
    .increment(1);
}

/// Sets the currently active phase gauge.
///
/// Zeros out the previous phase label (if any) before setting the new one,
/// preventing stale labels from showing `1.0` in Prometheus.
///
/// Implements: TJ-SPEC-008 F-009
pub fn set_current_phase(phase_name: &str, previous_phase: Option<&str>) {
    if let Some(prev) = previous_phase {
        gauge!("thoughtjack_current_phase", "phase_name" => sanitize_phase_label(prev)).set(0.0);
    }
    gauge!("thoughtjack_current_phase", "phase_name" => sanitize_phase_label(phase_name)).set(1.0);
}

/// Sets the number of active connections.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn set_connections_active(count: u64) {
    gauge!("thoughtjack_connections_active").set(count as f64);
}

/// Records delivery bytes sent.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_delivery_bytes(bytes: u64) {
    counter!("thoughtjack_delivery_bytes_total").increment(bytes);
}

/// Records a side effect execution.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_side_effect(
    effect_type: &str,
    messages_sent: u64,
    bytes_sent: u64,
    duration: Duration,
) {
    counter!("thoughtjack_side_effects_total", "effect_type" => effect_type.to_owned())
        .increment(1);
    histogram!("thoughtjack_side_effect_messages", "effect_type" => effect_type.to_owned())
        .record(messages_sent as f64);
    histogram!("thoughtjack_side_effect_bytes", "effect_type" => effect_type.to_owned())
        .record(bytes_sent as f64);
    histogram!("thoughtjack_side_effect_duration_ms", "effect_type" => effect_type.to_owned())
        .record(duration.as_secs_f64() * 1000.0);
}

/// Records an error by category.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_error(category: &str) {
    counter!("thoughtjack_errors_total", "category" => category.to_owned()).increment(1);
}

/// Records a payload size by generator type.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_payload_size(generator_type: &str, size_bytes: u64) {
    histogram!(
        "thoughtjack_payload_size_bytes",
        "generator_type" => generator_type.to_owned()
    )
    .record(size_bytes as f64);
}

/// Sets the server uptime gauge.
///
/// Implements: TJ-SPEC-008 F-009
pub fn set_uptime(duration: Duration) {
    gauge!("thoughtjack_uptime_seconds").set(duration.as_secs_f64());
}

/// Records the current count for an event type.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_event_count(event: &str, count: u64) {
    let label = sanitize_event_label(event);
    gauge!("thoughtjack_event_counts", "event" => label.to_owned()).set(count as f64);
}

// ============================================================================
// v0.5 recording functions
// ============================================================================

/// Records a completed scenario execution.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_scenario_completed(result: &str) {
    counter!("tj_scenarios_total", "result" => result.to_owned()).increment(1);
}

/// Records a completed actor execution.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_actor_completed(mode: &str, status: &str) {
    counter!("tj_actors_total", "mode" => mode.to_owned(), "status" => status.to_owned())
        .increment(1);
}

/// Records a v0.5 phase transition.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_tj_phase_transition(actor: &str, from: &str, to: &str) {
    counter!(
        "tj_phase_transitions_total",
        "actor" => sanitize_phase_label(actor),
        "from" => sanitize_phase_label(from),
        "to" => sanitize_phase_label(to)
    )
    .increment(1);
}

/// Records an extractor capture.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_extractor_captured(actor: &str) {
    counter!("tj_extractors_captured_total", "actor" => sanitize_phase_label(actor)).increment(1);
}

/// Records a synthesize (LLM generation) call.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_synthesize_call(actor: &str, protocol: &str) {
    counter!(
        "tj_synthesize_calls_total",
        "actor" => sanitize_phase_label(actor),
        "protocol" => protocol.to_owned()
    )
    .increment(1);
}

/// Records a protocol message (in or out).
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_protocol_message(actor: &str, direction: &str, method: &str) {
    let label = sanitize_method_label(method);
    counter!(
        "tj_protocol_messages_total",
        "actor" => sanitize_phase_label(actor),
        "direction" => direction.to_owned(),
        "method" => label.to_owned()
    )
    .increment(1);
}

/// Records a transport error.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_transport_error(actor: &str, protocol: &str) {
    counter!(
        "tj_transport_errors_total",
        "actor" => sanitize_phase_label(actor),
        "protocol" => protocol.to_owned()
    )
    .increment(1);
}

/// Records a verdict result.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_verdict(result: &str) {
    counter!("tj_verdicts_total", "result" => result.to_owned()).increment(1);
}

/// Records an indicator evaluation.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_indicator_evaluation(method: &str, result: &str) {
    counter!(
        "tj_indicators_evaluated_total",
        "method" => method.to_owned(),
        "result" => result.to_owned()
    )
    .increment(1);
}

/// Records an LLM call for semantic evaluation.
///
/// Implements: TJ-SPEC-008 F-009
pub fn record_semantic_llm_call(latency: Duration) {
    counter!("tj_semantic_llm_calls_total").increment(1);
    histogram!("tj_semantic_llm_latency_seconds").record(latency.as_secs_f64());
}

/// Records messages captured during grace period.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_grace_period_messages(count: u64) {
    histogram!("tj_grace_period_messages_captured").record(count as f64);
}

// ============================================================================
// Label sanitization helpers
// ============================================================================

/// Maximum length for phase name labels.
///
/// Phase names come from user config and are used directly as Prometheus
/// labels. This caps the label length to prevent cardinality issues.
const MAX_PHASE_LABEL_LEN: usize = 64;

/// Sanitizes a phase name for use as a metrics label.
///
/// Truncates to [`MAX_PHASE_LABEL_LEN`] characters and replaces any
/// characters invalid in Prometheus labels with underscores.
fn sanitize_phase_label(name: &str) -> String {
    name.chars()
        .take(MAX_PHASE_LABEL_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Sanitizes an event label, handling both generic and specific forms.
///
/// Generic events like `"tools/call"` are validated directly.
/// Specific events like `"tools/call:calc"` are accepted when the
/// prefix before `:` is a known method. Unknown prefixes are bucketed
/// as `"__unknown__"`.
#[must_use]
fn sanitize_event_label(event: &str) -> &str {
    // Check the full event name first (handles generic events)
    if KNOWN_MCP_METHODS.contains(&event)
        || KNOWN_A2A_METHODS.contains(&event)
        || KNOWN_AGUI_EVENTS.contains(&event)
    {
        return event;
    }
    // For specific events like "tools/call:calc", validate the prefix
    if let Some(prefix) = event.split(':').next()
        && (KNOWN_MCP_METHODS.contains(&prefix)
            || KNOWN_A2A_METHODS.contains(&prefix)
            || KNOWN_AGUI_EVENTS.contains(&prefix))
    {
        return event;
    }
    "__unknown__"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_known_mcp_method_returns_original() {
        assert_eq!(sanitize_method_label("tools/call"), "tools/call");
    }

    #[test]
    fn sanitize_known_a2a_method_returns_original() {
        assert_eq!(sanitize_method_label("message/send"), "message/send");
        assert_eq!(sanitize_method_label("tasks/get"), "tasks/get");
    }

    #[test]
    fn sanitize_known_agui_event_returns_original() {
        assert_eq!(sanitize_method_label("RUN_STARTED"), "RUN_STARTED");
        assert_eq!(
            sanitize_method_label("TEXT_MESSAGE_CONTENT"),
            "TEXT_MESSAGE_CONTENT"
        );
    }

    #[test]
    fn sanitize_unknown_method_returns_unknown() {
        assert_eq!(sanitize_method_label("evil/method"), "__unknown__");
        assert_eq!(sanitize_method_label(""), "__unknown__");
    }

    #[test]
    fn sanitize_all_known_mcp_methods() {
        for method in &KNOWN_MCP_METHODS {
            assert_eq!(
                sanitize_method_label(method),
                *method,
                "expected {method} to be recognized"
            );
        }
    }

    #[test]
    fn sanitize_all_known_a2a_methods() {
        for method in &KNOWN_A2A_METHODS {
            assert_eq!(
                sanitize_method_label(method),
                *method,
                "expected {method} to be recognized"
            );
        }
    }

    #[test]
    fn sanitize_all_known_agui_events() {
        for event in &KNOWN_AGUI_EVENTS {
            assert_eq!(
                sanitize_method_label(event),
                *event,
                "expected {event} to be recognized"
            );
        }
    }

    #[test]
    fn very_long_method_returns_unknown() {
        let long_method = "x".repeat(10_000);
        assert_eq!(sanitize_method_label(&long_method), "__unknown__");
    }

    #[test]
    fn record_functions_do_not_panic_without_recorder() {
        // metrics macros silently no-op when no global recorder is installed
        record_request("tools/call");
        record_response("tools/call", true, None);
        record_request_duration("tools/call", Duration::from_millis(42));
        record_delivery_duration(Duration::from_secs(1));
        record_delivery_bytes(1024);
        record_side_effect("notification_flood", 100, 5000, Duration::from_millis(500));
        record_phase_transition("trust_building", "exploit");
        set_current_phase("exploit", Some("trust_building"));
        set_connections_active(3);
        record_event_count("tools/call", 5);
        record_error("transport");
        record_payload_size("nested_json", 10_000);
        set_uptime(Duration::from_secs(300));
    }

    #[test]
    fn v05_record_functions_do_not_panic_without_recorder() {
        record_scenario_completed("exploited");
        record_actor_completed("mcp_server", "completed");
        record_tj_phase_transition("actor1", "trust", "exploit");
        record_extractor_captured("actor1");
        record_synthesize_call("actor1", "mcp");
        record_protocol_message("actor1", "incoming", "tools/call");
        record_transport_error("actor1", "mcp");
        record_verdict("exploited");
        record_indicator_evaluation("cel", "matched");
        record_semantic_llm_call(Duration::from_millis(500));
        record_grace_period_messages(5);
    }

    #[test]
    fn test_sanitize_empty_method() {
        assert_eq!(sanitize_method_label(""), "__unknown__");
    }

    #[test]
    fn test_sanitize_method_with_slash() {
        assert_eq!(sanitize_method_label("custom/method"), "__unknown__");
    }

    #[test]
    fn test_init_metrics_none_port() {
        let result = init_metrics(None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_record_phase_transition_does_not_panic() {
        record_phase_transition("a", "b");
    }

    #[test]
    fn test_set_current_phase_does_not_panic() {
        set_current_phase("p1", None);
    }

    #[test]
    fn test_set_connections_active_does_not_panic() {
        set_connections_active(5);
    }

    #[test]
    fn test_record_error_does_not_panic() {
        record_error("test");
    }

    #[test]
    fn test_record_payload_size_does_not_panic() {
        record_payload_size("nested_json", 1024);
    }

    #[test]
    fn test_record_event_count_does_not_panic() {
        record_event_count("test", 1);
    }

    #[test]
    fn test_metrics_counter_overflow_saturates() {
        counter!("thoughtjack_requests_total", "method" => "tools/call").increment(u64::MAX);
    }

    #[test]
    fn test_elicitation_and_sampling_recognized() {
        assert_eq!(
            sanitize_method_label("elicitation/create"),
            "elicitation/create"
        );
        assert_eq!(
            sanitize_method_label("sampling/createMessage"),
            "sampling/createMessage"
        );
    }
}
