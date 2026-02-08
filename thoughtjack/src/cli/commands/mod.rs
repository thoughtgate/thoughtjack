//! CLI command dispatch and handlers (TJ-SPEC-007)
//!
//! Routes parsed CLI arguments to the appropriate command handler.

pub mod agent;
pub mod completions;
pub mod diagram;
pub mod docs;
pub mod scenarios;
pub mod server;
pub mod version;

use tokio_util::sync::CancellationToken;

use crate::cli::args::{Cli, Commands, DocsSubcommand, ScenariosSubcommand, ServerSubcommand};
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
        Commands::Server(cmd) => match cmd.subcommand {
            Some(ServerSubcommand::Run(args)) => server::run(&args, cancel).await,
            Some(ServerSubcommand::Validate(args)) => server::validate(&args).await,
            Some(ServerSubcommand::List(args)) => server::list(&args).await,
            None => server::run(&cmd.run_args, cancel).await,
        },
        Commands::Scenarios(cmd) => match cmd.subcommand {
            ScenariosSubcommand::List(ref args) => scenarios::list(args).await,
            ScenariosSubcommand::Show(ref args) => scenarios::show(args).await,
        },
        Commands::Diagram(args) => diagram::run(&args),
        Commands::Docs(cmd) => match cmd.subcommand {
            DocsSubcommand::Generate(ref args) => docs::generate(args),
            DocsSubcommand::Validate(ref args) => docs::validate_cmd(args),
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
