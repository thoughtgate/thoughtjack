//! Scenarios command handlers (TJ-SPEC-010)
//!
//! Implements `scenarios list` and `scenarios show`.

use std::fmt::Write as _;

use crate::cli::args::{OutputFormat, ScenariosListArgs, ScenariosShowArgs};
use crate::error::ThoughtJackError;
use crate::scenarios::{self, ScenarioCategory};

/// List available built-in scenarios.
///
/// Displays scenarios grouped by category (human) or as a JSON array.
///
/// # Errors
///
/// Returns an I/O error if output serialization fails.
///
/// Implements: TJ-SPEC-010 F-004
#[allow(clippy::unused_async)]
pub async fn list(args: &ScenariosListArgs) -> Result<(), ThoughtJackError> {
    let results = scenarios::list_scenarios(args.category, args.tag.as_deref());

    match args.format {
        OutputFormat::Json => {
            let json_entries: Vec<serde_json::Value> = results
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "description": s.description,
                        "category": s.category.to_string(),
                        "taxonomy": s.taxonomy,
                        "tags": s.tags,
                        "features": s.features,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json_entries)
                    .map_err(|e| ThoughtJackError::Io(std::io::Error::other(e.to_string())))?
            );
        }
        OutputFormat::Human => {
            if results.is_empty() {
                println!("No scenarios match the given filters.");
                return Ok(());
            }

            let total = results.len();
            println!("Built-in Scenarios ({total} available)\n");

            // Group by category in display order
            for cat in ScenarioCategory::all() {
                let in_cat: Vec<_> = results.iter().filter(|s| s.category == *cat).collect();
                if in_cat.is_empty() {
                    continue;
                }

                println!("  {}", cat.label());
                for s in in_cat {
                    let taxonomy = format!("[{}]", s.taxonomy.join(", "));
                    println!("    {:<24}{:<56}{taxonomy}", s.name, s.description);
                }
                println!();
            }

            println!("Run a scenario: thoughtjack server run --scenario <name>");
            println!("View YAML:      thoughtjack scenarios show <name>");
        }
    }

    Ok(())
}

/// Display the YAML configuration for a built-in scenario.
///
/// Prints raw YAML to stdout, suitable for piping.
///
/// # Errors
///
/// Returns a usage error if the scenario name is not found.
///
/// Implements: TJ-SPEC-010 F-005
#[allow(clippy::unused_async)]
pub async fn show(args: &ScenariosShowArgs) -> Result<(), ThoughtJackError> {
    let scenario = scenarios::find_scenario(&args.name).ok_or_else(|| {
        let mut message = format!("Unknown scenario '{}'", args.name);

        if let Some(suggestion) = scenarios::suggest_scenario(&args.name) {
            let _ = write!(message, "\n\nDid you mean '{suggestion}'?");
        }

        message.push_str("\n\nAvailable scenarios:");
        for name in scenarios::list_scenario_names() {
            if let Some(s) = scenarios::find_scenario(name) {
                let _ = write!(message, "\n  {:<24}{}", s.name, s.description);
            }
        }

        message.push_str("\n\nUse 'thoughtjack scenarios list' for full details.");
        ThoughtJackError::Usage(message)
    })?;

    print!("{}", scenario.yaml);
    Ok(())
}
