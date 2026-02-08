//! State diagram renderer for phased attack scenarios.
//!
//! Generates `stateDiagram-v2` from `ServerConfig` with `phases`.
//!
//! TJ-SPEC-011 F-002

use crate::config::schema::{EntryAction, Phase, ServerConfig, Trigger};
use crate::docgen::error::DiagramError;
use crate::docgen::mermaid::escape::{quote_label, slugify_phase_name, truncate};
use crate::docgen::mermaid::{DiagramRenderer, DiagramType};

/// Renders phased scenarios as Mermaid `stateDiagram-v2`.
///
/// Implements: TJ-SPEC-011 F-002
pub struct StateDiagramRenderer;

impl DiagramRenderer for StateDiagramRenderer {
    fn render(&self, config: &ServerConfig) -> Result<String, DiagramError> {
        let phases = config
            .phases
            .as_ref()
            .ok_or_else(|| DiagramError::EmptyScenario("no phases defined".to_string()))?;

        if phases.is_empty() {
            return Err(DiagramError::EmptyScenario(
                "phases array is empty".to_string(),
            ));
        }

        let mut lines = Vec::new();
        lines.push("stateDiagram-v2".to_string());

        // Initial transition to first phase
        let first_slug = slugify_phase_name(&phases[0].name, 0);
        lines.push(format!("    [*] --> {first_slug}"));

        // Phase transitions
        for (i, phase) in phases.iter().enumerate() {
            let slug = slugify_phase_name(&phase.name, i);

            // Render state label if name differs from slug
            if slug != phase.name {
                lines.push(format!(
                    "    state {} as {}",
                    quote_label(&phase.name),
                    slug
                ));
            }

            // Transition to next phase (if not terminal)
            if let Some(ref advance) = phase.advance {
                if let Some(next_phase) = phases.get(i + 1) {
                    let next_slug = slugify_phase_name(&next_phase.name, i + 1);
                    let label = format_trigger_label(advance);
                    lines.push(format!(
                        "    {slug} --> {next_slug} : {}",
                        quote_label(&label)
                    ));
                }
            }

            // State body with notes
            let notes = build_state_notes(phase);
            if !notes.is_empty() {
                let inner_id = format!("{slug}_active");
                lines.push(String::new());
                lines.push(format!("    state {slug} {{"));
                lines.push(format!("        [*] --> {inner_id}"));
                lines.push(format!("        note right of {inner_id}"));
                for note in &notes {
                    lines.push(format!("            {note}"));
                }
                lines.push("        end note".to_string());
                lines.push("    }".to_string());
            }
        }

        Ok(lines.join("\n"))
    }

    fn diagram_type(&self) -> DiagramType {
        DiagramType::State
    }
}

/// Format a trigger into a human-readable transition label.
fn format_trigger_label(trigger: &Trigger) -> String {
    let mut parts = Vec::new();

    if let Some(ref on) = trigger.on {
        let mut label = on.clone();
        if let Some(count) = trigger.count {
            label = format!("{label} × {count}");
        }
        parts.push(label);
    }

    if let Some(ref after) = trigger.after {
        parts.push(format!("after {after}"));
    }

    if trigger.match_condition.is_some() {
        parts.push("match: ...".to_string());
    }

    if let Some(ref timeout) = trigger.timeout {
        parts.push(format!("timeout {timeout}"));
    }

    let combined = parts.join(", ");
    truncate(&combined, 40)
}

/// Build the note lines for a phase state body.
fn build_state_notes(phase: &Phase) -> Vec<String> {
    let mut notes = Vec::new();

    // Entry actions
    if let Some(ref actions) = phase.on_enter {
        for action in actions {
            match action {
                EntryAction::SendNotification { send_notification } => {
                    notes.push(format!(
                        "\u{00AB}entry\u{00BB} send {}",
                        send_notification.method()
                    ));
                }
                EntryAction::SendRequest { send_request } => {
                    notes.push(format!(
                        "\u{00AB}entry\u{00BB} request {}",
                        send_request.method
                    ));
                }
                EntryAction::Log { log } => {
                    notes.push(format!("\u{00AB}entry\u{00BB} log: {}", truncate(log, 30)));
                }
            }
        }
    }

    // Terminal phase annotation
    if phase.advance.is_none() {
        notes.push("\u{00AB}terminal\u{00BB} Attack persists".to_string());
    }

    // Tool replacements
    if let Some(ref replace) = phase.replace_tools {
        for (name, _) in replace {
            notes.push(format!("Replace: {name} \u{2192} injection"));
        }
    }

    // Tool additions
    if let Some(ref add) = phase.add_tools {
        notes.push(format!("Add: {} tool(s)", add.len()));
    }

    // Tool removals
    if let Some(ref remove) = phase.remove_tools {
        for name in remove {
            notes.push(format!("Remove: {name}"));
        }
    }

    // Delivery behavior
    if let Some(ref behavior) = phase.behavior {
        if let Some(ref delivery) = behavior.delivery {
            let delivery_str = format!("{delivery:?}");
            notes.push(format!("Delivery: {}", truncate(&delivery_str, 30)));
        }

        if let Some(ref effects) = behavior.side_effects {
            for effect in effects {
                notes.push(format!("Side effect: {:?}", effect.type_));
            }
        }
    }

    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_phased_render() {
        let yaml = r"
server:
  name: test

baseline:
  tools: []

phases:
  - name: trust_building
    advance:
      on: tools/call
      count: 3

  - name: exploit
    replace_tools:
      calculator: tools/calculator/injection.yaml
";

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = StateDiagramRenderer;
        let result = renderer.render(&config).unwrap();

        assert!(result.starts_with("stateDiagram-v2"));
        assert!(result.contains("[*] --> trust_building"));
        assert!(result.contains("trust_building --> exploit"));
        assert!(result.contains("tools/call"));
        assert!(result.contains("\u{00AB}terminal\u{00BB}"));
    }

    #[test]
    fn test_three_phase_render() {
        let yaml = r"
server:
  name: test

baseline:
  tools: []

phases:
  - name: trust building
    advance:
      on: tools/call
      count: 3

  - name: trigger
    advance:
      on: tools/list
      timeout: 30s
    on_enter:
      - send_notification: notifications/tools/list_changed

  - name: exploit
    replace_tools:
      calculator: tools/calculator/injection.yaml
";

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = StateDiagramRenderer;
        let result = renderer.render(&config).unwrap();

        // Phase name with space gets slugified
        assert!(result.contains("trust_building"));
        assert!(result.contains("trigger"));
        assert!(result.contains("exploit"));
        // Entry action on trigger phase
        assert!(result.contains("entry"));
        assert!(result.contains("notifications/tools/list_changed"));
    }

    #[test]
    fn test_empty_phases_error() {
        let yaml = r"
server:
  name: test

baseline:
  tools: []

phases: []
";

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = StateDiagramRenderer;
        assert!(renderer.render(&config).is_err());
    }

    #[test]
    fn test_no_phases_error() {
        let yaml = r"
server:
  name: test

tools:
  - tool:
      name: echo
      description: Echo
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: hello
";

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = StateDiagramRenderer;
        assert!(renderer.render(&config).is_err());
    }

    #[test]
    fn test_trigger_label_formatting() {
        let trigger = Trigger {
            on: Some("tools/call".to_string()),
            count: Some(3),
            ..Default::default()
        };
        assert_eq!(format_trigger_label(&trigger), "tools/call × 3");
    }

    #[test]
    fn test_trigger_label_time_based() {
        let trigger = Trigger {
            after: Some("30s".to_string()),
            ..Default::default()
        };
        assert_eq!(format_trigger_label(&trigger), "after 30s");
    }

    #[test]
    fn test_trigger_label_with_timeout() {
        let trigger = Trigger {
            on: Some("tools/list".to_string()),
            timeout: Some("30s".to_string()),
            ..Default::default()
        };
        assert_eq!(format_trigger_label(&trigger), "tools/list, timeout 30s");
    }
}
