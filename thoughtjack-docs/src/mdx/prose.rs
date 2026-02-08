//! Prose generation for YAML structural elements.
//!
//! Each function maps a config structure to human-readable prose.
//! Prose is deterministic given the same input.
//!
//! TJ-SPEC-011 F-003

use thoughtjack_core::config::schema::{
    BehaviorConfig, ContentValue, DeliveryConfig, EntryAction, GeneratorConfig, Phase,
    SideEffectConfig, Trigger,
};

/// Generate prose for a phase trigger.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_trigger(trigger: &Trigger) -> String {
    let mut parts = Vec::new();

    // Main event
    if let Some(ref on) = trigger.on {
        let event = format!("`{on}`");
        parts.push(trigger.count.map_or_else(
            || format!("Advances on {event}"),
            |count| format!("Advances after {count} {event} requests"),
        ));
    }

    // Time-based
    if let Some(ref after) = trigger.after {
        parts.push(format!("Advances {after} after phase entry"));
    }

    // Content match
    if trigger.match_condition.is_some() {
        parts.push("Advances when content matches specified pattern".to_string());
    }

    // Timeout modifier
    if let Some(ref timeout) = trigger.timeout {
        let timeout_behavior = trigger.on_timeout.map_or_else(
            || format!("or after {timeout} of inactivity (whichever comes first)"),
            |behavior| match behavior {
                thoughtjack_core::config::schema::TimeoutBehavior::Advance => {
                    format!("or after {timeout} of inactivity (whichever comes first)")
                }
                thoughtjack_core::config::schema::TimeoutBehavior::Abort => {
                    format!("; aborts scenario after {timeout}")
                }
            },
        );
        parts.push(timeout_behavior);
    }

    if parts.is_empty() {
        "No trigger configured (manual advance only)".to_string()
    } else {
        parts.join(" ")
    }
}

/// Generate prose for a full phase.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_phase(phase: &Phase) -> String {
    let mut sections = Vec::new();

    // Trigger
    if let Some(ref advance) = phase.advance {
        sections.push(prose_for_trigger(advance));
    } else {
        sections.push("**Terminal phase** — attack persists indefinitely.".to_string());
    }

    // Entry actions
    if let Some(ref actions) = phase.on_enter {
        let action_prose = prose_for_entry_actions(actions);
        sections.push(action_prose);
    }

    // Tool changes
    if let Some(ref replace) = phase.replace_tools {
        for (name, _) in replace {
            sections.push(format!("Swaps `{name}` tool with injection variant."));
        }
    }
    if let Some(ref add) = phase.add_tools {
        sections.push(format!("Adds {} new tool(s).", add.len()));
    }
    if let Some(ref remove) = phase.remove_tools {
        for name in remove {
            sections.push(format!("Removes `{name}` tool."));
        }
    }

    // Resource changes
    if let Some(ref replace) = phase.replace_resources {
        for (uri, _) in replace {
            sections.push(format!("Replaces resource `{uri}`."));
        }
    }

    // Behavior
    if let Some(ref behavior) = phase.behavior {
        sections.push(prose_for_behavior(behavior));
    }

    sections.join(" ")
}

/// Generate prose for a behavior config.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_behavior(behavior: &BehaviorConfig) -> String {
    let mut parts = Vec::new();

    if let Some(ref delivery) = behavior.delivery {
        parts.push(prose_for_delivery(delivery));
    }

    if let Some(ref effects) = behavior.side_effects {
        for effect in effects {
            parts.push(prose_for_side_effect(effect));
        }
    }

    parts.join(" ")
}

/// Generate prose for a delivery config.
fn prose_for_delivery(delivery: &DeliveryConfig) -> String {
    match delivery {
        DeliveryConfig::Normal => "Delivers response normally.".to_string(),
        DeliveryConfig::SlowLoris {
            byte_delay_ms,
            chunk_size,
        } => {
            let delay = byte_delay_ms.unwrap_or(100);
            let chunk = chunk_size.unwrap_or(1);
            format!("Delivers response using slow-loris pattern ({delay}ms per {chunk} byte(s)).")
        }
        DeliveryConfig::UnboundedLine { target_bytes, .. } => target_bytes.map_or_else(
            || "Delivers response as unbounded line (no newline terminator).".to_string(),
            |bytes| format!("Delivers response as unbounded line ({bytes} bytes, no newline)."),
        ),
        DeliveryConfig::NestedJson { depth, key } => {
            let key_str = key.as_deref().unwrap_or("data");
            format!("Wraps response in deeply nested JSON ({depth} levels, key: `{key_str}`).")
        }
        DeliveryConfig::ResponseDelay { delay_ms } => {
            format!("Delays response by {delay_ms}ms before sending.")
        }
    }
}

/// Generate prose for a side effect config.
fn prose_for_side_effect(effect: &SideEffectConfig) -> String {
    let rate = effect
        .params
        .get("rate_per_sec")
        .and_then(serde_json::Value::as_u64);

    match effect.type_ {
        thoughtjack_core::config::schema::SideEffectType::NotificationFlood => rate.map_or_else(
            || "Triggers notification flood side effect.".to_string(),
            |r| format!("Triggers notification flood side effect at {r} notifications/sec."),
        ),
        thoughtjack_core::config::schema::SideEffectType::BatchAmplify => {
            "Triggers batch amplification side effect.".to_string()
        }
        thoughtjack_core::config::schema::SideEffectType::PipeDeadlock => {
            "Triggers pipe deadlock side effect.".to_string()
        }
        thoughtjack_core::config::schema::SideEffectType::CloseConnection => {
            "Closes the connection.".to_string()
        }
        thoughtjack_core::config::schema::SideEffectType::DuplicateRequestIds => {
            "Sends duplicate request IDs.".to_string()
        }
    }
}

/// Generate prose for entry actions.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_entry_actions(actions: &[EntryAction]) -> String {
    if actions.len() == 1 {
        return format!("On phase entry: {}", prose_for_entry_action(&actions[0]));
    }

    let items: Vec<String> = actions
        .iter()
        .enumerate()
        .map(|(i, action)| format!("({}) {}", i + 1, prose_for_entry_action(action)))
        .collect();

    format!("On phase entry: {}", items.join(", "))
}

/// Generate prose for a single entry action.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_entry_action(action: &EntryAction) -> String {
    match action {
        EntryAction::SendNotification { send_notification } => {
            format!("sends `{}`", send_notification.method())
        }
        EntryAction::SendRequest { send_request } => {
            format!("sends `{}` request", send_request.method)
        }
        EntryAction::Log { log } => {
            format!("logs \"{log}\"")
        }
    }
}

/// Generate prose for a `$generate` directive.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_generator(generator: &GeneratorConfig) -> String {
    let type_name = format!("{:?}", generator.type_);
    let params: Vec<String> = generator
        .params
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();

    if params.is_empty() {
        format!("Generates {type_name} payload.")
    } else {
        format!("Generates {type_name} payload ({}).", params.join(", "))
    }
}

/// Generate prose for a `ContentValue`.
///
/// Implements: TJ-SPEC-011 F-003
#[must_use]
pub fn prose_for_content_value(value: &ContentValue) -> String {
    match value {
        ContentValue::Static(s) => {
            if s.len() > 80 {
                format!("Static text ({} chars)", s.len())
            } else {
                format!("\"{s}\"")
            }
        }
        ContentValue::Generated { generator } => prose_for_generator(generator),
        ContentValue::File { path } => {
            format!("Content from file `{}`", path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thoughtjack_core::config::schema::TimeoutBehavior;

    #[test]
    fn test_trigger_event_count() {
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            count: Some(3),
            ..Default::default()
        };
        assert_eq!(
            prose_for_trigger(&trigger),
            "Advances after 3 `tools/call` requests"
        );
    }

    #[test]
    fn test_trigger_time_based() {
        let trigger = Trigger {
            after: Some("10s".to_string()),
            ..Default::default()
        };
        assert_eq!(
            prose_for_trigger(&trigger),
            "Advances 10s after phase entry"
        );
    }

    #[test]
    fn test_trigger_with_timeout() {
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            timeout: Some("30s".to_string()),
            ..Default::default()
        };
        let prose = prose_for_trigger(&trigger);
        assert!(prose.contains("tools/call"));
        assert!(prose.contains("30s"));
        assert!(prose.contains("inactivity"));
    }

    #[test]
    fn test_trigger_with_abort() {
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            timeout: Some("60s".to_string()),
            on_timeout: Some(TimeoutBehavior::Abort),
            ..Default::default()
        };
        let prose = prose_for_trigger(&trigger);
        assert!(prose.contains("aborts"));
    }

    #[test]
    fn test_delivery_slow_loris() {
        let delivery = DeliveryConfig::SlowLoris {
            byte_delay_ms: Some(100),
            chunk_size: Some(1),
        };
        assert_eq!(
            prose_for_delivery(&delivery),
            "Delivers response using slow-loris pattern (100ms per 1 byte(s))."
        );
    }

    #[test]
    fn test_delivery_nested_json() {
        let delivery = DeliveryConfig::NestedJson {
            depth: 10000,
            key: Some("wrapper".to_string()),
        };
        let prose = prose_for_delivery(&delivery);
        assert!(prose.contains("10000 levels"));
        assert!(prose.contains("wrapper"));
    }

    #[test]
    fn test_entry_action_single() {
        let actions = vec![EntryAction::SendNotification {
            send_notification: thoughtjack_core::config::schema::SendNotificationConfig::Short(
                "notifications/tools/list_changed".to_string(),
            ),
        }];
        let prose = prose_for_entry_actions(&actions);
        assert!(prose.contains("sends `notifications/tools/list_changed`"));
    }

    #[test]
    fn test_entry_action_multiple() {
        let actions = vec![
            EntryAction::SendNotification {
                send_notification: thoughtjack_core::config::schema::SendNotificationConfig::Short(
                    "notifications/tools/list_changed".to_string(),
                ),
            },
            EntryAction::Log {
                log: "Phase activated".to_string(),
            },
        ];
        let prose = prose_for_entry_actions(&actions);
        assert!(prose.contains("(1)"));
        assert!(prose.contains("(2)"));
    }

    #[test]
    fn test_content_value_static_short() {
        let value = ContentValue::Static("Hello".to_string());
        assert_eq!(prose_for_content_value(&value), "\"Hello\"");
    }

    #[test]
    fn test_content_value_static_long() {
        let value = ContentValue::Static("a".repeat(100));
        assert!(prose_for_content_value(&value).contains("100 chars"));
    }

    #[test]
    fn test_prose_determinism() {
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            count: Some(5),
            timeout: Some("60s".to_string()),
            ..Default::default()
        };
        let prose1 = prose_for_trigger(&trigger);
        let prose2 = prose_for_trigger(&trigger);
        assert_eq!(prose1, prose2);
    }
}
