//! Diagram generation command handler.
//!
//! TJ-SPEC-011 F-006

use std::fs;

use crate::cli::args::{DiagramArgs, DiagramTypeChoice};
use crate::docgen::mermaid::{self, DiagramType};
use crate::error::ThoughtJackError;

/// Execute the diagram command.
///
/// Reads a scenario YAML file and generates a Mermaid diagram.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or diagram generation fails.
///
/// Implements: TJ-SPEC-011 F-006
pub fn run(args: &DiagramArgs) -> Result<(), ThoughtJackError> {
    let yaml = fs::read_to_string(&args.scenario).map_err(|e| {
        ThoughtJackError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read {}: {e}", args.scenario.display()),
        ))
    })?;

    let config: crate::config::schema::ServerConfig = serde_yaml::from_str(&yaml).map_err(|e| {
        ThoughtJackError::Config(crate::error::ConfigError::ParseError {
            path: args.scenario.clone(),
            line: None,
            message: e.to_string(),
        })
    })?;

    let diagram_type = match args.diagram_type {
        DiagramTypeChoice::Auto => mermaid::auto_select(&config),
        DiagramTypeChoice::State => DiagramType::State,
        DiagramTypeChoice::Sequence => DiagramType::Sequence,
        DiagramTypeChoice::Flowchart => DiagramType::Flowchart,
    };

    let renderer = mermaid::create_renderer(diagram_type);
    let diagram = renderer.render(&config).map_err(|e| {
        ThoughtJackError::Io(std::io::Error::other(format!(
            "diagram generation failed: {e}"
        )))
    })?;

    if let Some(ref output_path) = args.output {
        fs::write(output_path, &diagram).map_err(|e| {
            ThoughtJackError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to write {}: {e}", output_path.display()),
            ))
        })?;
        eprintln!("Wrote diagram to {}", output_path.display());
    } else {
        println!("{diagram}");
    }

    Ok(())
}
