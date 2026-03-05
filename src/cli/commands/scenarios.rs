//! Scenarios command handlers (TJ-SPEC-010)
//!
//! Implements `scenarios list`, `scenarios show`, and `scenarios run`.

use std::fmt::Write as _;

use tokio_util::sync::CancellationToken;

use crate::cli::args::{OutputFormat, ScenariosListArgs, ScenariosRunArgs, ScenariosShowArgs};
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
pub async fn list(args: &ScenariosListArgs, quiet: bool) -> Result<(), ThoughtJackError> {
    let results = scenarios::list_scenarios(args.category, args.tag.as_deref());

    if quiet {
        return Ok(());
    }

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

            println!("Run a scenario: thoughtjack scenarios run <name>");
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
pub async fn show(args: &ScenariosShowArgs, quiet: bool) -> Result<(), ThoughtJackError> {
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

    if !quiet {
        print!("{}", scenario.yaml);
    }
    Ok(())
}

/// Run a built-in scenario by name.
///
/// Resolves the scenario YAML, then delegates to the `run` command handler.
///
/// # Errors
///
/// Returns a usage error if the scenario name is not found, or a runtime
/// error if execution fails.
///
/// Implements: TJ-SPEC-010 F-008
pub async fn run_scenario(
    args: &ScenariosRunArgs,
    quiet: bool,
    cancel: CancellationToken,
) -> Result<(), ThoughtJackError> {
    let scenario = scenarios::find_scenario(&args.name).ok_or_else(|| {
        let mut message = format!("Unknown scenario '{}'", args.name);

        if let Some(suggestion) = scenarios::suggest_scenario(&args.name) {
            let _ = write!(message, "\n\nDid you mean '{suggestion}'?");
        }

        message.push_str("\n\nUse 'thoughtjack scenarios list' to see available scenarios.");
        ThoughtJackError::Usage(message)
    })?;

    let run_args: crate::cli::args::RunArgs = (&args.run).into();
    super::run::run_from_yaml(scenario.yaml, &run_args, quiet, cancel).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cli::args::ScenariosRunOverrides;

    fn test_run_args() -> ScenariosRunOverrides {
        ScenariosRunOverrides {
            mcp_server: None,
            mcp_client_command: None,
            mcp_client_args: None,
            mcp_client_endpoint: None,
            agui_client_endpoint: None,
            a2a_server: None,
            a2a_client_endpoint: None,
            grace_period: None,
            max_session: humantime::Duration::from(Duration::from_secs(1)),
            readiness_timeout: humantime::Duration::from(Duration::from_secs(1)),
            output: None,
            header: vec![],
            no_semantic: false,
            raw_synthesize: false,
            metrics_port: None,
            events_file: None,
        }
    }

    #[tokio::test]
    async fn run_scenario_unknown_name_is_usage_error() {
        let args = ScenariosRunArgs {
            name: "not-a-real-scenario".to_string(),
            run: test_run_args(),
        };

        let err = run_scenario(&args, true, CancellationToken::new())
            .await
            .expect_err("unknown scenario should fail with usage error");

        match err {
            ThoughtJackError::Usage(msg) => {
                assert!(
                    msg.contains("Unknown scenario"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected usage error, got {other}"),
        }
    }
}
