//! Mermaid diagram generation from `ThoughtJack` scenario configurations.
//!
//! Supports three diagram types:
//! - `stateDiagram-v2` for phased attack scenarios
//! - `sequenceDiagram` for conditional response scenarios
//! - `flowchart` for behavioral/DoS scenarios
//!
//! TJ-SPEC-011 F-002

pub mod escape;
pub mod flowchart;
pub mod sequence;
pub mod state;

use crate::error::DiagramError;
use thoughtjack_core::config::schema::ServerConfig;

/// Diagram type for Mermaid rendering.
///
/// Implements: TJ-SPEC-011 F-002
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramType {
    /// State diagram for phased scenarios.
    State,
    /// Sequence diagram for conditional response scenarios.
    Sequence,
    /// Flowchart for behavioral/DoS scenarios.
    Flowchart,
}

/// Renders a Mermaid diagram from a scenario configuration.
///
/// Implements: TJ-SPEC-011 F-002
pub trait DiagramRenderer {
    /// Render the diagram as a Mermaid markup string.
    ///
    /// # Errors
    ///
    /// Returns `DiagramError` if the configuration cannot be rendered.
    fn render(&self, config: &ServerConfig) -> Result<String, DiagramError>;

    /// Returns the diagram type this renderer produces.
    fn diagram_type(&self) -> DiagramType;
}

/// Auto-select the best diagram type for a scenario configuration.
///
/// Selection matrix:
/// - Phased server (`phases` present) → `State`
/// - Simple server with `match` blocks → `Sequence`
/// - DoS/behavioral only → `Flowchart` (TD)
/// - Simple static server → `Flowchart` (LR)
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn auto_select(config: &ServerConfig) -> DiagramType {
    if config.phases.is_some() {
        return DiagramType::State;
    }

    if has_conditional_responses(config) {
        return DiagramType::Sequence;
    }

    DiagramType::Flowchart
}

/// Create a renderer for the given diagram type.
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn create_renderer(diagram_type: DiagramType) -> Box<dyn DiagramRenderer> {
    match diagram_type {
        DiagramType::State => Box::new(state::StateDiagramRenderer),
        DiagramType::Sequence => Box::new(sequence::SequenceDiagramRenderer),
        DiagramType::Flowchart => Box::new(flowchart::FlowchartRenderer),
    }
}

/// Check if a config has conditional match blocks in any tool/resource/prompt response.
fn has_conditional_responses(config: &ServerConfig) -> bool {
    if let Some(tools) = &config.tools {
        if tools.iter().any(|t| t.response.match_block.is_some()) {
            return true;
        }
    }
    if let Some(resources) = &config.resources {
        if resources
            .iter()
            .any(|r| r.response.as_ref().is_some_and(|resp| resp.match_block.is_some()))
        {
            return true;
        }
    }
    if let Some(prompts) = &config.prompts {
        if prompts
            .iter()
            .any(|p| p.response.match_block.is_some())
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_select_phased() {
        let yaml = r#"
server:
  name: test
phases:
  - name: phase1
baseline:
  tools: []
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(auto_select(&config), DiagramType::State);
    }

    #[test]
    fn test_auto_select_simple() {
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
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(auto_select(&config), DiagramType::Flowchart);
    }

    #[test]
    fn test_auto_select_with_match() {
        let yaml = r#"
server:
  name: test
tools:
  - tool:
      name: search
      description: Search
      inputSchema:
        type: object
    response:
      match:
        - when:
            args.query: "secret"
          content:
            - type: text
              text: injected
        - default: true
          content:
            - type: text
              text: normal
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(auto_select(&config), DiagramType::Sequence);
    }
}
