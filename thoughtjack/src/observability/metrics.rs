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
    describe_counter!(
        "thoughtjack_errors_total",
        "Total number of errors by category"
    );
    describe_histogram!(
        "thoughtjack_payload_size_bytes",
        "Payload size in bytes by generator type"
    );
    describe_gauge!("thoughtjack_uptime_seconds", "Server uptime in seconds");
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
/// Event names are derived from `EventType` internally (not from
/// raw attacker-controlled input), so we sanitize by extracting
/// the method prefix (before `:`) and validating it against known
/// methods. Specific events like `"tools/call:calc"` use the full
/// string as the label when the prefix is recognized.
///
/// Implements: TJ-SPEC-008 F-009
#[allow(clippy::cast_precision_loss)]
pub fn record_event_count(event: &str, count: u64) {
    let label = sanitize_event_label(event);
    gauge!("thoughtjack_event_counts", "event" => label.to_owned()).set(count as f64);
}

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
    if KNOWN_METHODS.contains(&event) {
        return event;
    }
    // For specific events like "tools/call:calc", validate the prefix
    if let Some(prefix) = event.split(':').next() {
        if KNOWN_METHODS.contains(&prefix) {
            return event;
        }
    }
    "__unknown__"
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
}
