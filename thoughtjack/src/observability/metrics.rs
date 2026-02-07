//! Metrics collection for `ThoughtJack` (TJ-SPEC-008 F-009 / F-010).
//!
//! Provides Prometheus-compatible metrics with label cardinality protection
//! and typed convenience functions for recording measurements.

use std::time::Duration;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::error::ThoughtJackError;

/// Known MCP method names used for label cardinality protection.
///
/// Any method not in this list is bucketed as `"__unknown__"` to prevent
/// memory exhaustion from attacker-controlled label values
/// (EC-OBS-021, EC-OBS-022).
const KNOWN_METHODS: [&str; 12] = [
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
    "logging/setLevel",
    "completion/complete",
];

/// Sanitizes a method name for use as a metrics label.
///
/// Returns the original string when it matches a known MCP method,
/// or `"__unknown__"` otherwise.
///
/// Implements: TJ-SPEC-008 F-009, EC-OBS-021, EC-OBS-022
#[must_use]
pub fn sanitize_method_label(method: &str) -> &str {
    if KNOWN_METHODS.contains(&method) {
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
fn describe_metrics() {
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
}

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
pub fn record_response(method: &str, success: bool) {
    let label = sanitize_method_label(method);
    let status = if success { "success" } else { "error" };
    counter!("thoughtjack_responses_total", "method" => label.to_owned(), "status" => status)
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
/// Implements: TJ-SPEC-008 F-009
pub fn record_phase_transition(from: &str, to: &str) {
    counter!(
        "thoughtjack_phase_transitions_total",
        "from" => from.to_owned(),
        "to" => to.to_owned()
    )
    .increment(1);
}

/// Sets the currently active phase gauge.
///
/// Implements: TJ-SPEC-008 F-009
pub fn set_current_phase(phase_name: &str) {
    gauge!("thoughtjack_current_phase", "phase_name" => phase_name.to_owned()).set(1.0);
}

/// Sets the number of active connections.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn set_connections_active(count: u64) {
    gauge!("thoughtjack_connections_active").set(count as f64);
}

/// Records the current count for an event type.
///
/// The event name is sanitized via [`sanitize_method_label`] to prevent
/// label cardinality explosion from unknown event types.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_event_count(event: &str, count: u64) {
    let label = sanitize_method_label(event);
    gauge!("thoughtjack_event_counts", "event" => label.to_owned()).set(count as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_known_method_returns_original() {
        assert_eq!(sanitize_method_label("tools/call"), "tools/call");
    }

    #[test]
    fn sanitize_unknown_method_returns_unknown() {
        assert_eq!(sanitize_method_label("evil/method"), "__unknown__");
        assert_eq!(sanitize_method_label(""), "__unknown__");
    }

    #[test]
    fn sanitize_all_known_methods() {
        for method in &KNOWN_METHODS {
            assert_eq!(
                sanitize_method_label(method),
                *method,
                "expected {method} to be recognized"
            );
        }
    }

    #[test]
    fn very_long_method_returns_unknown() {
        // EC-OBS-022: very long method name should be bucketed as __unknown__
        let long_method = "x".repeat(10_000);
        assert_eq!(sanitize_method_label(&long_method), "__unknown__");
    }

    #[test]
    fn record_functions_do_not_panic_without_recorder() {
        // metrics macros silently no-op when no global recorder is installed
        record_request("tools/call");
        record_response("tools/call", true);
        record_request_duration("tools/call", Duration::from_millis(42));
        record_delivery_duration(Duration::from_secs(1));
        record_phase_transition("trust_building", "exploit");
        set_current_phase("exploit");
        set_connections_active(3);
        record_event_count("tools/call", 5);
    }
}
