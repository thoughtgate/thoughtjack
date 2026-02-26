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
    match cli.command {
        Commands::Run(args) => run::run(&args, cancel).await,
        Commands::Scenarios(cmd) => match cmd.subcommand {
            ScenariosSubcommand::List(ref args) => scenarios::list(args).await,
            ScenariosSubcommand::Show(ref args) => scenarios::show(args).await,
            ScenariosSubcommand::Run(ref args) => scenarios::run_scenario(args, cancel).await,
        },
        Commands::Validate(args) => validate::validate(&args).await,
        Commands::Version(args) => {
            version::run(&args);
            Ok(())
        }
    }
}
