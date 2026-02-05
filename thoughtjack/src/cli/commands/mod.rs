//! CLI command dispatch and handlers (TJ-SPEC-007)
//!
//! Routes parsed CLI arguments to the appropriate command handler.

pub mod agent;
pub mod completions;
pub mod server;
pub mod version;

use crate::cli::args::{Cli, Commands, ServerSubcommand};
use crate::error::ThoughtJackError;

/// Dispatch a parsed CLI invocation to the appropriate command handler.
///
/// # Errors
///
/// Returns an error if the dispatched command handler fails.
///
/// Implements: TJ-SPEC-007 F-001
pub async fn dispatch(cli: Cli) -> Result<(), ThoughtJackError> {
    match cli.command {
        Commands::Server(cmd) => match cmd.subcommand {
            ServerSubcommand::Run(args) => server::run(&args).await,
            ServerSubcommand::Validate(args) => server::validate(&args).await,
            ServerSubcommand::List(args) => server::list(&args).await,
        },
        Commands::Agent(cmd) => agent::run(&cmd).await,
        Commands::Completions(args) => {
            completions::run(&args);
            Ok(())
        }
        Commands::Version(args) => {
            version::run(&args);
            Ok(())
        }
    }
}
