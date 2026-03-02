//! Entry action execution for phase transitions.
//!
//! When a phase is entered, its `on_enter` actions execute in order.
//! Actions include logging, sending notifications, and sending
//! elicitations. The `EntryActionSender` trait abstracts the
//! transport-dependent operations.
//!
//! See TJ-SPEC-013 §8.3 for the entry action specification.

use std::collections::HashMap;

use async_trait::async_trait;
use oatf::enums::{ElicitationMode, LogLevel};
use oatf::primitives::{interpolate_template, interpolate_value};

use crate::error::EngineError;

// ============================================================================
// EntryActionSender
// ============================================================================

/// Trait for sending protocol-level entry actions.
///
/// Protocol drivers provide transport-backed implementations of this
/// trait. When no sender is available (e.g., during testing or for
/// protocols that don't support certain actions), the `PhaseLoop`
/// logs a warning and continues.
///
/// # Errors
///
/// Implementations should return `EngineError::EntryAction` on failure.
///
/// Implements: TJ-SPEC-013 F-001
#[async_trait]
pub trait EntryActionSender: Send + Sync {
    /// Send a JSON-RPC notification to the agent.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::EntryAction` if the transport fails.
    async fn send_notification(
        &self,
        method: &str,
        params: Option<&serde_json::Value>,
    ) -> Result<(), EngineError>;

    /// Send an elicitation request to the agent.
    ///
    /// For url-mode elicitations, `elicitation_id` correlates with
    /// `notifications/elicitation/complete`. If `None` for url-mode,
    /// the caller should auto-generate a UUID before calling.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::EntryAction` if the transport fails.
    async fn send_elicitation(
        &self,
        message: &str,
        mode: Option<&ElicitationMode>,
        requested_schema: Option<&serde_json::Value>,
        url: Option<&str>,
        elicitation_id: Option<&str>,
    ) -> Result<(), EngineError>;
}

// ============================================================================
// execute_entry_actions
// ============================================================================

/// Execute a list of entry actions for a phase transition.
///
/// Processes actions in order. `SendNotification` and `SendElicitation`
/// are delegated to the `sender` if provided; `Log` actions use the
/// `tracing` crate directly. If no sender is provided, transport-dependent
/// actions log a warning and are skipped.
///
/// Template interpolation for action parameters uses the OATF SDK's
/// `interpolate_template()` and `interpolate_value()`.
///
/// Implements: TJ-SPEC-013 F-001
#[allow(clippy::implicit_hasher, clippy::cognitive_complexity)]
pub async fn execute_entry_actions(
    actions: &[oatf::Action],
    extractors: &HashMap<String, String>,
    sender: Option<&dyn EntryActionSender>,
) {
    for action in actions {
        match action {
            oatf::Action::SendNotification {
                method,
                params,
                extensions: _,
                non_ext_key_count: _,
            } => {
                let params = params
                    .as_ref()
                    .map(|p| interpolate_value(p, extractors, None, None).0);
                if let Some(sender) = sender {
                    if let Err(err) = sender.send_notification(method, params.as_ref()).await {
                        tracing::warn!(
                            method,
                            error = %err,
                            "failed to send notification entry action"
                        );
                    }
                } else {
                    tracing::warn!(
                        method,
                        "no entry action sender available — skipping send_notification"
                    );
                }
            }
            oatf::Action::SendElicitation {
                message,
                mode,
                requested_schema,
                url,
                extensions,
                non_ext_key_count: _,
            } => {
                let (interpolated_message, _) =
                    interpolate_template(message, extractors, None, None);
                // Extract elicitationId from extensions, or auto-generate
                // a UUID for url-mode if absent (§8.3).
                let is_url_mode = mode
                    .as_ref()
                    .is_some_and(|m| matches!(m, ElicitationMode::Url));
                let auto_id = extensions
                    .get("elicitationId")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| is_url_mode.then(|| uuid::Uuid::new_v4().to_string()));
                let elicitation_id = auto_id.as_deref();
                if let Some(sender) = sender {
                    if let Err(err) = sender
                        .send_elicitation(
                            &interpolated_message,
                            mode.as_ref(),
                            requested_schema.as_ref(),
                            url.as_deref(),
                            elicitation_id,
                        )
                        .await
                    {
                        tracing::warn!(
                            error = %err,
                            "failed to send elicitation entry action"
                        );
                    }
                } else {
                    tracing::warn!("no entry action sender available — skipping send_elicitation");
                }
            }
            oatf::Action::Log {
                message,
                level,
                extensions: _,
                non_ext_key_count: _,
            } => {
                let (interpolated_message, _) =
                    interpolate_template(message, extractors, None, None);
                match level {
                    Some(LogLevel::Error) => {
                        tracing::error!(source = "entry_action", "{}", interpolated_message);
                    }
                    Some(LogLevel::Warn) => {
                        tracing::warn!(source = "entry_action", "{}", interpolated_message);
                    }
                    Some(LogLevel::Info) | None => {
                        tracing::info!(source = "entry_action", "{}", interpolated_message);
                    }
                }
            }
            oatf::Action::BindingSpecific {
                key,
                value: _,
                extensions: _,
                non_ext_key_count: _,
            } => {
                tracing::debug!(
                    action = key,
                    "binding-specific entry action — not handled by core engine"
                );
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock sender that can be configured to succeed or fail.
    struct MockSender {
        fail_notifications: bool,
        fail_elicitations: bool,
        /// Tracks calls for assertion.
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl MockSender {
        fn succeeding() -> Self {
            Self {
                fail_notifications: false,
                fail_elicitations: false,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn failing_notifications() -> Self {
            Self {
                fail_notifications: true,
                fail_elicitations: false,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn failing_elicitations() -> Self {
            Self {
                fail_notifications: false,
                fail_elicitations: true,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn call_log(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl EntryActionSender for MockSender {
        async fn send_notification(
            &self,
            method: &str,
            _params: Option<&serde_json::Value>,
        ) -> Result<(), EngineError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("notification:{method}"));
            if self.fail_notifications {
                Err(EngineError::EntryAction("transport closed".to_string()))
            } else {
                Ok(())
            }
        }

        async fn send_elicitation(
            &self,
            message: &str,
            _mode: Option<&ElicitationMode>,
            _requested_schema: Option<&serde_json::Value>,
            _url: Option<&str>,
            elicitation_id: Option<&str>,
        ) -> Result<(), EngineError> {
            let id_info = elicitation_id.unwrap_or("none");
            self.calls
                .lock()
                .unwrap()
                .push(format!("elicitation:{message}:id={id_info}"));
            if self.fail_elicitations {
                Err(EngineError::EntryAction(
                    "elicitation send failed".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn log_action_executes_without_panic() {
        let actions = vec![oatf::Action::Log {
            message: "phase entered: {{phase_name}}".to_string(),
            level: Some(LogLevel::Info),
            extensions: HashMap::new(),
            non_ext_key_count: 2,
        }];

        let mut extractors = HashMap::new();
        extractors.insert("phase_name".to_string(), "exploit".to_string());

        // Should not panic even without a sender
        execute_entry_actions(&actions, &extractors, None).await;
    }

    #[tokio::test]
    async fn notification_without_sender_does_not_panic() {
        let actions = vec![oatf::Action::SendNotification {
            method: "notifications/tools/list_changed".to_string(),
            params: None,
            extensions: HashMap::new(),
            non_ext_key_count: 1,
        }];

        let extractors = HashMap::new();

        // Should log warning but not panic
        execute_entry_actions(&actions, &extractors, None).await;
    }

    #[tokio::test]
    async fn elicitation_without_sender_does_not_panic() {
        let actions = vec![oatf::Action::SendElicitation {
            message: "Enter your credentials".to_string(),
            mode: Some(ElicitationMode::Form),
            requested_schema: Some(serde_json::json!({"type": "object"})),
            url: None,
            extensions: HashMap::new(),
            non_ext_key_count: 3,
        }];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, None).await;
    }

    #[tokio::test]
    async fn binding_specific_action_logged() {
        let actions = vec![oatf::Action::BindingSpecific {
            key: "send_request".to_string(),
            value: serde_json::json!({"method": "custom"}),
            extensions: HashMap::new(),
            non_ext_key_count: 1,
        }];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, None).await;
    }

    #[tokio::test]
    async fn multiple_actions_execute_in_order() {
        let actions = vec![
            oatf::Action::Log {
                message: "first".to_string(),
                level: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
            oatf::Action::Log {
                message: "second".to_string(),
                level: Some(LogLevel::Warn),
                extensions: HashMap::new(),
                non_ext_key_count: 2,
            },
            oatf::Action::Log {
                message: "third".to_string(),
                level: Some(LogLevel::Error),
                extensions: HashMap::new(),
                non_ext_key_count: 2,
            },
        ];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, None).await;
    }

    // ---- Error path tests ----

    #[tokio::test]
    async fn notification_sender_error_continues_to_next_action() {
        // When send_notification fails, the error is logged but execution
        // continues to the next action in the list.
        let sender = MockSender::failing_notifications();
        let actions = vec![
            oatf::Action::SendNotification {
                method: "notifications/tools/list_changed".to_string(),
                params: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
            oatf::Action::Log {
                message: "after notification".to_string(),
                level: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
        ];

        let extractors = HashMap::new();
        // Should not panic despite send_notification returning Err
        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        // The notification was attempted (call recorded)
        let calls = sender.call_log();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "notification:notifications/tools/list_changed");
    }

    #[tokio::test]
    async fn elicitation_sender_error_continues_to_next_action() {
        let sender = MockSender::failing_elicitations();
        let actions = vec![
            oatf::Action::SendElicitation {
                message: "Enter credentials".to_string(),
                mode: Some(ElicitationMode::Form),
                requested_schema: None,
                url: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
            oatf::Action::Log {
                message: "after elicitation".to_string(),
                level: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
        ];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        let calls = sender.call_log();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("elicitation:Enter credentials"));
    }

    #[tokio::test]
    async fn notification_with_interpolated_params() {
        let sender = MockSender::succeeding();
        let actions = vec![oatf::Action::SendNotification {
            method: "notifications/resources/updated".to_string(),
            params: Some(serde_json::json!({"uri": "file:///{{path}}"})),
            extensions: HashMap::new(),
            non_ext_key_count: 2,
        }];

        let mut extractors = HashMap::new();
        extractors.insert("path".to_string(), "secret.txt".to_string());

        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        let calls = sender.call_log();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "notification:notifications/resources/updated");
    }

    #[tokio::test]
    async fn elicitation_url_mode_auto_generates_id() {
        let sender = MockSender::succeeding();
        let actions = vec![oatf::Action::SendElicitation {
            message: "Visit this URL".to_string(),
            mode: Some(ElicitationMode::Url),
            requested_schema: None,
            url: Some("https://example.com/auth".to_string()),
            extensions: HashMap::new(), // No elicitationId → auto-generate
            non_ext_key_count: 3,
        }];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        let calls = sender.call_log();
        assert_eq!(calls.len(), 1);
        // Should have auto-generated a UUID, not "none"
        assert!(
            !calls[0].ends_with("id=none"),
            "url-mode elicitation should auto-generate an elicitation_id, got: {}",
            calls[0]
        );
    }

    #[tokio::test]
    async fn elicitation_explicit_id_from_extensions() {
        let sender = MockSender::succeeding();
        let mut extensions = HashMap::new();
        extensions.insert(
            "elicitationId".to_string(),
            serde_json::json!("custom-id-123"),
        );

        let actions = vec![oatf::Action::SendElicitation {
            message: "Enter password".to_string(),
            mode: Some(ElicitationMode::Form),
            requested_schema: None,
            url: None,
            extensions,
            non_ext_key_count: 1,
        }];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        let calls = sender.call_log();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].contains("id=custom-id-123"),
            "should use explicit elicitationId from extensions, got: {}",
            calls[0]
        );
    }

    #[tokio::test]
    async fn all_sender_errors_logged_but_execution_completes() {
        // All three transport-dependent actions fail, but the function
        // processes all of them without panicking or short-circuiting.
        let sender = MockSender {
            fail_notifications: true,
            fail_elicitations: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };

        let actions = vec![
            oatf::Action::SendNotification {
                method: "notify1".to_string(),
                params: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
            oatf::Action::SendElicitation {
                message: "elicit1".to_string(),
                mode: None,
                requested_schema: None,
                url: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
            oatf::Action::SendNotification {
                method: "notify2".to_string(),
                params: None,
                extensions: HashMap::new(),
                non_ext_key_count: 1,
            },
        ];

        let extractors = HashMap::new();
        execute_entry_actions(&actions, &extractors, Some(&sender)).await;

        // All three were attempted despite errors
        let calls = sender.call_log();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], "notification:notify1");
        assert!(calls[1].starts_with("elicitation:elicit1"));
        assert_eq!(calls[2], "notification:notify2");
    }
}
