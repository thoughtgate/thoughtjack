//! CLI command dispatch and handlers (TJ-SPEC-007 v2)
//!
//! Routes parsed CLI arguments to the appropriate command handler.

pub mod run;
pub mod scenarios;
pub mod validate;
pub mod version;

use tokio_util::sync::CancellationToken;

use crate::cli::args::{Cli, Commands, ScenariosSubcommand};
use crate::error::ThoughtJackError;

/// Dispatch a parsed CLI invocation to the appropriate command handler.
///
/// # Errors
///
/// Returns an error if the dispatched command handler fails.
///
/// Implements: TJ-SPEC-007 F-001, F-005
pub async fn dispatch(cli: Cli, cancel: CancellationToken) -> Result<(), ThoughtJackError> {
    let quiet = cli.quiet;
    let color = cli.color;
    match cli.command {
        Commands::Run(args) => run::run(&args, quiet, color, cancel).await,
        Commands::Scenarios(cmd) => match cmd.subcommand {
            ScenariosSubcommand::List(ref args) => scenarios::list(args, quiet).await,
            ScenariosSubcommand::Show(ref args) => scenarios::show(args, quiet).await,
            ScenariosSubcommand::Run(ref args) => {
                scenarios::run_scenario(args, quiet, color, cancel).await
            }
        },
        Commands::Validate(args) => validate::validate(&args, quiet).await,
        Commands::Version(args) => {
            version::run(&args, quiet);
            Ok(())
        }
    }
}
