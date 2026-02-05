//! Trigger evaluation (TJ-SPEC-003 F-004, F-005, F-008)
//!
//! Evaluates transition triggers against current phase state,
//! supporting event matching, count thresholds, content matching,
//! and time-based triggers.

use std::time::Duration;

use regex::Regex;

use crate::config::schema::{FieldMatcher, MatchPredicate, Trigger};
use crate::error::PhaseError;

use super::state::{EventType, PhaseState};

/// Result of evaluating a trigger.
///
/// Implements: TJ-SPEC-003 F-004
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerResult {
    /// Trigger fired with a human-readable reason
    Fired(String),
    /// Trigger conditions not met
    NotMet,
}

/// Evaluates an event-based trigger against current state.
///
/// Checks in order: event name match, count threshold (>=), content match.
/// All conditions must be true for the trigger to fire.
///
/// Uses `>=` for threshold comparison to prevent skipped triggers
/// under high concurrency.
///
/// Implements: TJ-SPEC-003 F-004
pub fn evaluate(
    trigger: &Trigger,
    state: &PhaseState,
    event: &EventType,
    params: Option<&serde_json::Value>,
) -> TriggerResult {
    // Event-based triggers require `on` field
    let Some(on) = &trigger.on else {
        return TriggerResult::NotMet;
    };

    // Check event name match
    if !event_matches(&event.0, on) {
        return TriggerResult::NotMet;
    }

    // Check count threshold (>= to prevent skipped triggers under concurrency)
    let threshold = trigger.count.unwrap_or(1);
    let count = state.event_count(event);
    if count < threshold {
        return TriggerResult::NotMet;
    }

    // Check content match if specified
    if let Some(match_condition) = &trigger.match_condition {
        if let Some(params) = params {
            if !matches_content(match_condition, params) {
                return TriggerResult::NotMet;
            }
        } else {
            // No params to match against — match fails
            return TriggerResult::NotMet;
        }
    }

    let reason = trigger.count.map_or_else(
        || format!("event '{on}' occurred"),
        |count| format!("event '{on}' reached count {count}"),
    );

    TriggerResult::Fired(reason)
}

/// Evaluates a time-based trigger against current state.
///
/// Checks the `after` duration against time since phase entry.
///
/// Implements: TJ-SPEC-003 F-008
pub fn evaluate_time_trigger(trigger: &Trigger, state: &PhaseState) -> TriggerResult {
    let Some(after_str) = &trigger.after else {
        return TriggerResult::NotMet;
    };

    let Ok(duration) = parse_duration(after_str) else {
        return TriggerResult::NotMet;
    };

    let elapsed = state.phase_entered_at().elapsed();
    if elapsed >= duration {
        TriggerResult::Fired(format!("time trigger after {after_str} elapsed"))
    } else {
        TriggerResult::NotMet
    }
}

/// Evaluates a timeout trigger for event-based triggers.
///
/// Returns `Fired` if the timeout duration has elapsed since phase entry.
///
/// Implements: TJ-SPEC-003 F-009
pub fn evaluate_timeout(trigger: &Trigger, state: &PhaseState) -> TriggerResult {
    let Some(timeout_str) = &trigger.timeout else {
        return TriggerResult::NotMet;
    };

    // Timeout only valid when `on` is also specified
    if trigger.on.is_none() {
        return TriggerResult::NotMet;
    }

    let Ok(duration) = parse_duration(timeout_str) else {
        return TriggerResult::NotMet;
    };

    let elapsed = state.phase_entered_at().elapsed();
    if elapsed >= duration {
        TriggerResult::Fired(format!("timeout after {timeout_str} elapsed"))
    } else {
        TriggerResult::NotMet
    }
}

/// Checks if an event name matches a trigger pattern.
///
/// Supports both generic (`"tools/call"`) and specific (`"tools/call:calculator"`) patterns.
fn event_matches(event_name: &str, pattern: &str) -> bool {
    if event_name == pattern {
        return true;
    }
    // A specific event "tools/call:calc" matches generic trigger "tools/call"
    // But a generic event "tools/call" does NOT match specific trigger "tools/call:calc"
    if let Some(prefix) = pattern.strip_suffix("") {
        // If the pattern has no colon, match events that start with pattern + ":"
        if !pattern.contains(':')
            && event_name.starts_with(pattern)
            && event_name[pattern.len()..].starts_with(':')
        {
            return true;
        }
        let _ = prefix; // suppress unused variable
    }
    false
}

/// Checks if request parameters match a content predicate.
///
/// All conditions in the predicate must match (AND semantics).
/// Uses dot-notation field paths for nested access.
///
/// Implements: TJ-SPEC-003 F-005
#[must_use]
pub fn matches_content(predicate: &MatchPredicate, params: &serde_json::Value) -> bool {
    for (path, matcher) in &predicate.conditions {
        let value = json_path_get(params, path);
        match value {
            Some(v) => {
                if !field_matches(matcher, v) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Navigates a JSON value using dot-notation path.
fn json_path_get<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Matches a single field value against a `FieldMatcher`.
fn field_matches(matcher: &FieldMatcher, value: &serde_json::Value) -> bool {
    match matcher {
        FieldMatcher::Exact(expected) => {
            // Compare the JSON value's string representation to the expected string
            match value {
                serde_json::Value::String(s) => s == expected,
                _ => false,
            }
        }
        FieldMatcher::Pattern {
            contains,
            prefix,
            suffix,
            regex,
        } => {
            let s = match value {
                serde_json::Value::String(s) => s.as_str(),
                _ => return false,
            };

            if let Some(needle) = contains {
                if !s.contains(needle.as_str()) {
                    return false;
                }
            }

            if let Some(pfx) = prefix {
                if !s.starts_with(pfx.as_str()) {
                    return false;
                }
            }

            if let Some(sfx) = suffix {
                if !s.ends_with(sfx.as_str()) {
                    return false;
                }
            }

            if let Some(pattern) = regex {
                match Regex::new(pattern) {
                    Ok(re) => {
                        if !re.is_match(s) {
                            return false;
                        }
                    }
                    Err(_) => return false,
                }
            }

            true
        }
    }
}

/// Parses a duration string like "30s", "5m", "100ms", "1h".
///
/// # Errors
///
/// Returns `PhaseError::TriggerError` if the format is invalid.
///
/// Implements: TJ-SPEC-003 F-008
pub fn parse_duration(s: &str) -> Result<Duration, PhaseError> {
    let s = s.trim();

    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms
            .trim()
            .parse()
            .map_err(|_| PhaseError::TriggerError(format!("invalid duration: '{s}'")))?;
        return Ok(Duration::from_millis(n));
    }

    if let Some(hours) = s.strip_suffix('h') {
        let n: u64 = hours
            .trim()
            .parse()
            .map_err(|_| PhaseError::TriggerError(format!("invalid duration: '{s}'")))?;
        return Ok(Duration::from_secs(n * 3600));
    }

    if let Some(mins) = s.strip_suffix('m') {
        let n: u64 = mins
            .trim()
            .parse()
            .map_err(|_| PhaseError::TriggerError(format!("invalid duration: '{s}'")))?;
        return Ok(Duration::from_secs(n * 60));
    }

    if let Some(secs) = s.strip_suffix('s') {
        let n: u64 = secs
            .trim()
            .parse()
            .map_err(|_| PhaseError::TriggerError(format!("invalid duration: '{s}'")))?;
        return Ok(Duration::from_secs(n));
    }

    Err(PhaseError::TriggerError(format!(
        "invalid duration format: '{s}' (expected suffix: ms, s, m, h)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use serde_json::json;

    // ---- Duration Parsing ----

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn test_parse_duration_milliseconds() {
        assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100));
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn test_parse_duration_invalid_format() {
        assert!(parse_duration("30x").is_err());
    }

    #[test]
    fn test_parse_duration_invalid_number() {
        assert!(parse_duration("abcs").is_err());
    }

    #[test]
    fn test_parse_duration_empty() {
        assert!(parse_duration("").is_err());
    }

    // ---- Event Matching ----

    #[test]
    fn test_event_matches_exact() {
        assert!(event_matches("tools/call", "tools/call"));
    }

    #[test]
    fn test_event_matches_specific_to_generic() {
        // Specific event matches generic trigger
        assert!(event_matches("tools/call:calculator", "tools/call"));
    }

    #[test]
    fn test_event_no_match_generic_to_specific() {
        // Generic event does NOT match specific trigger
        assert!(!event_matches("tools/call", "tools/call:calculator"));
    }

    #[test]
    fn test_event_no_match_different() {
        assert!(!event_matches("tools/call", "tools/list"));
    }

    #[test]
    fn test_event_matches_specific_exact() {
        assert!(event_matches(
            "tools/call:calculator",
            "tools/call:calculator"
        ));
    }

    // ---- Content Matching ----

    #[test]
    fn test_matches_content_exact() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"path": "/etc/passwd"});
        assert!(matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_exact_no_match() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"path": "/etc/shadow"});
        assert!(!matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_missing_field() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"other": "value"});
        assert!(!matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_contains() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "query".to_string(),
            FieldMatcher::Pattern {
                contains: Some("password".to_string()),
                prefix: None,
                suffix: None,
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"query": "find password here"});
        assert!(matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_prefix() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Pattern {
                contains: None,
                prefix: Some("/etc/".to_string()),
                suffix: None,
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"path": "/etc/passwd"});
        assert!(matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_suffix() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "file".to_string(),
            FieldMatcher::Pattern {
                contains: None,
                prefix: None,
                suffix: Some(".env".to_string()),
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"file": "app.env"});
        assert!(matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_regex() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Pattern {
                contains: None,
                prefix: None,
                suffix: None,
                regex: Some(r"\.(env|pem|key)$".to_string()),
            },
        );
        let predicate = MatchPredicate { conditions };

        assert!(matches_content(&predicate, &json!({"path": "secret.pem"})));
        assert!(!matches_content(&predicate, &json!({"path": "readme.md"})));
    }

    #[test]
    fn test_matches_content_nested_path() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "args.options.path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let predicate = MatchPredicate { conditions };

        let params = json!({"args": {"options": {"path": "/etc/passwd"}}});
        assert!(matches_content(&predicate, &params));
    }

    #[test]
    fn test_matches_content_multiple_conditions_and() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "operation".to_string(),
            FieldMatcher::Exact("read".to_string()),
        );
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Pattern {
                contains: None,
                prefix: Some("/etc/".to_string()),
                suffix: None,
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        // Both match
        assert!(matches_content(
            &predicate,
            &json!({"operation": "read", "path": "/etc/passwd"})
        ));
        // One fails
        assert!(!matches_content(
            &predicate,
            &json!({"operation": "write", "path": "/etc/passwd"})
        ));
    }

    #[test]
    fn test_matches_content_non_string_fails() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "count".to_string(),
            FieldMatcher::Pattern {
                contains: Some("5".to_string()),
                prefix: None,
                suffix: None,
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        // Number value doesn't match string operations
        assert!(!matches_content(&predicate, &json!({"count": 50})));
    }

    #[test]
    fn test_matches_content_combined_pattern() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Pattern {
                contains: Some("secret".to_string()),
                prefix: Some("/home/".to_string()),
                suffix: Some(".key".to_string()),
                regex: None,
            },
        );
        let predicate = MatchPredicate { conditions };

        assert!(matches_content(
            &predicate,
            &json!({"path": "/home/user/secret.key"})
        ));
        assert!(!matches_content(
            &predicate,
            &json!({"path": "/home/user/secret.pem"})
        ));
    }

    // ---- Trigger Evaluation ----

    #[test]
    fn test_evaluate_event_trigger() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            count: Some(3),
            ..Default::default()
        };

        // Increment to 2 — not yet
        state.increment_event(&event);
        state.increment_event(&event);
        assert_eq!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::NotMet
        );

        // Increment to 3 — fires
        state.increment_event(&event);
        match evaluate(&trigger, &state, &event, None) {
            TriggerResult::Fired(reason) => assert!(reason.contains("count 3")),
            TriggerResult::NotMet => panic!("Expected Fired"),
        }
    }

    #[test]
    fn test_evaluate_threshold_gte() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            count: Some(3),
            ..Default::default()
        };

        // Increment past threshold (simulating concurrent race)
        for _ in 0..5 {
            state.increment_event(&event);
        }

        // Should still fire (>= not ==)
        assert!(matches!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::Fired(_)
        ));
    }

    #[test]
    fn test_evaluate_default_count_1() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/list");
        let trigger = Trigger {
            on: Some("tools/list".to_string()),
            count: None, // defaults to 1
            ..Default::default()
        };

        state.increment_event(&event);
        assert!(matches!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::Fired(_)
        ));
    }

    #[test]
    fn test_evaluate_wrong_event() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/list");
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            ..Default::default()
        };

        state.increment_event(&event);
        assert_eq!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::NotMet
        );
    }

    #[test]
    fn test_evaluate_with_content_match() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");

        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            match_condition: Some(MatchPredicate { conditions }),
            ..Default::default()
        };

        state.increment_event(&event);

        // With matching params
        let params = json!({"path": "/etc/passwd"});
        assert!(matches!(
            evaluate(&trigger, &state, &event, Some(&params)),
            TriggerResult::Fired(_)
        ));

        // With non-matching params
        let params = json!({"path": "/etc/shadow"});
        assert_eq!(
            evaluate(&trigger, &state, &event, Some(&params)),
            TriggerResult::NotMet
        );
    }

    #[test]
    fn test_evaluate_content_match_no_params() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");

        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Exact("/etc/passwd".to_string()),
        );
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            match_condition: Some(MatchPredicate { conditions }),
            ..Default::default()
        };

        state.increment_event(&event);
        assert_eq!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::NotMet
        );
    }

    #[test]
    fn test_evaluate_no_on_field() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");
        let trigger = Trigger {
            after: Some("30s".to_string()),
            ..Default::default()
        };

        assert_eq!(
            evaluate(&trigger, &state, &event, None),
            TriggerResult::NotMet
        );
    }

    // ---- Time Trigger ----

    #[test]
    fn test_evaluate_time_trigger_not_elapsed() {
        let state = PhaseState::new(3);
        let trigger = Trigger {
            after: Some("1h".to_string()),
            ..Default::default()
        };

        assert_eq!(
            evaluate_time_trigger(&trigger, &state),
            TriggerResult::NotMet
        );
    }

    #[test]
    fn test_evaluate_time_trigger_elapsed() {
        let state = PhaseState::new(3);
        let trigger = Trigger {
            after: Some("0s".to_string()),
            ..Default::default()
        };

        // 0s should fire immediately
        assert!(matches!(
            evaluate_time_trigger(&trigger, &state),
            TriggerResult::Fired(_)
        ));
    }

    #[test]
    fn test_evaluate_time_trigger_no_after() {
        let state = PhaseState::new(3);
        let trigger = Trigger::default();

        assert_eq!(
            evaluate_time_trigger(&trigger, &state),
            TriggerResult::NotMet
        );
    }

    // ---- Timeout ----

    #[test]
    fn test_evaluate_timeout_fires() {
        let state = PhaseState::new(3);
        let trigger = Trigger {
            on: Some("tools/list".to_string()),
            timeout: Some("0s".to_string()),
            ..Default::default()
        };

        assert!(matches!(
            evaluate_timeout(&trigger, &state),
            TriggerResult::Fired(_)
        ));
    }

    #[test]
    fn test_evaluate_timeout_without_on() {
        let state = PhaseState::new(3);
        let trigger = Trigger {
            timeout: Some("0s".to_string()),
            ..Default::default()
        };

        // Timeout only valid with `on`
        assert_eq!(evaluate_timeout(&trigger, &state), TriggerResult::NotMet);
    }

    #[test]
    fn test_evaluate_timeout_not_elapsed() {
        let state = PhaseState::new(3);
        let trigger = Trigger {
            on: Some("tools/list".to_string()),
            timeout: Some("1h".to_string()),
            ..Default::default()
        };

        assert_eq!(evaluate_timeout(&trigger, &state), TriggerResult::NotMet);
    }

    // ---- Regex Edge Cases ----

    #[test]
    fn test_invalid_regex_no_match() {
        let mut conditions = IndexMap::new();
        conditions.insert(
            "path".to_string(),
            FieldMatcher::Pattern {
                contains: None,
                prefix: None,
                suffix: None,
                regex: Some("[invalid".to_string()),
            },
        );
        let predicate = MatchPredicate { conditions };

        assert!(!matches_content(&predicate, &json!({"path": "anything"})));
    }
}
