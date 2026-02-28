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
    /// # Errors
    ///
    /// Returns `EngineError::EntryAction` if the transport fails.
    async fn send_elicitation(
        &self,
        message: &str,
        mode: Option<&ElicitationMode>,
        requested_schema: Option<&serde_json::Value>,
        url: Option<&str>,
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
                extensions: _,
                non_ext_key_count: _,
            } => {
                let (interpolated_message, _) =
                    interpolate_template(message, extractors, None, None);
                if let Some(sender) = sender {
                    if let Err(err) = sender
                        .send_elicitation(
                            &interpolated_message,
                            mode.as_ref(),
                            requested_schema.as_ref(),
                            url.as_deref(),
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
}
