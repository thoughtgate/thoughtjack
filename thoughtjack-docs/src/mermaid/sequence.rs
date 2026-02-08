//! Sequence diagram renderer for conditional response scenarios.
//!
//! Generates `sequenceDiagram` from `ServerConfig` with `match` blocks.
//!
//! TJ-SPEC-011 F-002

use crate::error::DiagramError;
use crate::mermaid::escape::truncate;
use crate::mermaid::{DiagramRenderer, DiagramType};
use thoughtjack_core::config::schema::{
    ContentItem, ContentValue, MatchBranchConfig, ServerConfig, ToolPattern,
};

/// Renders conditional-response scenarios as Mermaid `sequenceDiagram`.
///
/// Implements: TJ-SPEC-011 F-002
pub struct SequenceDiagramRenderer;

impl DiagramRenderer for SequenceDiagramRenderer {
    fn render(&self, config: &ServerConfig) -> Result<String, DiagramError> {
        let mut lines = Vec::new();
        lines.push("sequenceDiagram".to_string());
        lines.push("    participant C as Client".to_string());
        lines.push("    participant TJ as ThoughtJack".to_string());

        let tools = config.tools.as_ref().ok_or_else(|| {
            DiagramError::EmptyScenario("no tools defined for sequence diagram".to_string())
        })?;

        for tool in tools {
            render_tool_sequence(&mut lines, tool);
        }

        if lines.len() <= 3 {
            return Err(DiagramError::EmptyScenario(
                "no match blocks found for sequence diagram".to_string(),
            ));
        }

        Ok(lines.join("\n"))
    }

    fn diagram_type(&self) -> DiagramType {
        DiagramType::Sequence
    }
}

/// Render a single tool's sequence interactions.
fn render_tool_sequence(lines: &mut Vec<String>, tool: &ToolPattern) {
    let tool_name = &tool.tool.name;

    lines.push(String::new());
    lines.push(format!("    C->>TJ: tools/call \"{tool_name}\""));

    if let Some(ref match_block) = tool.response.match_block {
        let mut first = true;
        for branch in match_block {
            match branch {
                MatchBranchConfig::When {
                    when,
                    content,
                    messages: _,
                    ..
                } => {
                    let conditions: Vec<String> =
                        when.iter().map(|(field, _cond)| field.clone()).collect();
                    let condition_str = truncate(&conditions.join(" & "), 50);

                    if first {
                        lines.push(format!("    alt {condition_str} matches"));
                        first = false;
                    } else {
                        lines.push(format!("    else {condition_str} matches"));
                    }

                    let response_summary = summarize_content(content);
                    lines.push(format!("        TJ->>C: {response_summary}"));
                }
                MatchBranchConfig::Default { content, .. } => {
                    lines.push("    else default".to_string());
                    let response_summary = summarize_content(content);
                    lines.push(format!("        TJ->>C: {response_summary}"));
                }
            }
        }
        lines.push("    end".to_string());
    } else {
        // Non-conditional tool: simple request/response
        let response_summary = summarize_content(&tool.response.content);
        lines.push(format!("    TJ->>C: {response_summary}"));
    }
}

/// Summarize response content into a short description.
fn summarize_content(content: &[ContentItem]) -> String {
    if content.is_empty() {
        return "(empty response)".to_string();
    }

    match &content[0] {
        ContentItem::Text { text } => match text {
            ContentValue::Static(s) => truncate(s, 40),
            ContentValue::Generated { generator } => {
                format!("$generate({:?})", generator.type_)
            }
            ContentValue::File { path } => {
                format!("$file({})", path.display())
            }
        },
        ContentItem::Image { mime_type, .. } => format!("[image: {mime_type}]"),
        ContentItem::Resource { resource } => {
            format!("[resource: {}]", resource.uri)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_with_match() {
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
              text: Injected credentials
        - default: true
          content:
            - type: text
              text: Normal results
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = SequenceDiagramRenderer;
        let result = renderer.render(&config).unwrap();

        assert!(result.starts_with("sequenceDiagram"));
        assert!(result.contains("participant C as Client"));
        assert!(result.contains("alt"));
        assert!(result.contains("else default"));
        assert!(result.contains("end"));
        assert!(result.contains("search"));
    }

    #[test]
    fn test_sequence_no_tools_error() {
        let yaml = r"
server:
  name: test
";

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        let renderer = SequenceDiagramRenderer;
        assert!(renderer.render(&config).is_err());
    }

    #[test]
    fn test_summarize_static_content() {
        let content = vec![ContentItem::Text {
            text: ContentValue::Static("Hello world".to_string()),
        }];
        assert_eq!(summarize_content(&content), "Hello world");
    }

    #[test]
    fn test_summarize_empty_content() {
        assert_eq!(summarize_content(&[]), "(empty response)");
    }
}
