//! Flowchart renderer for behavioral and simple scenarios.
//!
//! Generates `flowchart TD` (DoS/behavioral) or `flowchart LR` (simple static)
//! from `ServerConfig`.
//!
//! TJ-SPEC-011 F-002

use crate::error::DiagramError;
use crate::mermaid::escape::quote_label;
use crate::mermaid::{DiagramRenderer, DiagramType};
use thoughtjack_core::config::schema::ServerConfig;

/// Renders behavioral or simple scenarios as Mermaid flowcharts.
///
/// Implements: TJ-SPEC-011 F-002
pub struct FlowchartRenderer;

impl DiagramRenderer for FlowchartRenderer {
    fn render(&self, config: &ServerConfig) -> Result<String, DiagramError> {
        let has_side_effects = config
            .behavior
            .as_ref()
            .is_some_and(|b| b.side_effects.is_some());

        let direction = if has_side_effects { "TD" } else { "LR" };

        let mut lines = Vec::new();
        lines.push(format!("flowchart {direction}"));

        // Entry node
        lines.push("    req([Incoming Request])".to_string());

        // Tools
        if let Some(ref tools) = config.tools {
            for (i, tool) in tools.iter().enumerate() {
                let id = format!("tool{i}");
                lines.push(format!(
                    "    {id}[{}]",
                    quote_label(&tool.tool.name)
                ));
                lines.push(format!("    req --> {id}"));
            }
        }

        // Resources
        if let Some(ref resources) = config.resources {
            for (i, resource) in resources.iter().enumerate() {
                let id = format!("res{i}");
                lines.push(format!(
                    "    {id}[{}]",
                    quote_label(&resource.resource.name)
                ));
                lines.push(format!("    req --> {id}"));
            }
        }

        // Delivery behavior
        if let Some(ref behavior) = config.behavior {
            if let Some(ref delivery) = behavior.delivery {
                let delivery_label = format!("{delivery:?}");
                lines.push(format!(
                    "    delivery{{{{{}}}}}", // double braces for diamond
                    quote_label(&delivery_label)
                ));
                lines.push("    req --> delivery".to_string());
            }

            // Side effects
            if let Some(ref effects) = behavior.side_effects {
                for (i, effect) in effects.iter().enumerate() {
                    let id = format!("fx{i}");
                    let label = format!("{:?} ({:?})", effect.type_, effect.trigger);
                    lines.push(format!("    {id}>{{{}}}", quote_label(&label)));
                    lines.push(format!("    req --> {id}"));
                }
            }
        }

        // Response node
        lines.push("    resp([Response])".to_string());
        if let Some(ref tools) = config.tools {
            for i in 0..tools.len() {
                lines.push(format!("    tool{i} --> resp"));
            }
        }
        if let Some(ref resources) = config.resources {
            for i in 0..resources.len() {
                lines.push(format!("    res{i} --> resp"));
            }
        }

        Ok(lines.join("\n"))
    }

    fn diagram_type(&self) -> DiagramType {
        DiagramType::Flowchart
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_flowchart() {
        let yaml = r#"
server:
  name: test
tools:
  - tool:
      name: echo
      description: Echo tool
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: hello
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = FlowchartRenderer;
        let result = renderer.render(&config).unwrap();

        assert!(result.starts_with("flowchart LR"));
        assert!(result.contains("echo"));
        assert!(result.contains("req"));
        assert!(result.contains("resp"));
    }

    #[test]
    fn test_behavioral_flowchart() {
        let yaml = r#"
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
behavior:
  delivery:
    type: slow_loris
    byte_delay_ms: 100
  side_effects:
    - type: notification_flood
      trigger: on_connect
      rate_per_sec: 100
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = FlowchartRenderer;
        let result = renderer.render(&config).unwrap();

        assert!(result.starts_with("flowchart TD"));
        assert!(result.contains("SlowLoris"));
        assert!(result.contains("NotificationFlood"));
    }

    #[test]
    fn test_empty_flowchart() {
        let yaml = r#"
server:
  name: test
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = FlowchartRenderer;
        let result = renderer.render(&config).unwrap();
        assert!(result.starts_with("flowchart LR"));
    }
}
