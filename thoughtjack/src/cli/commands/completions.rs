//! Shell completion generation (TJ-SPEC-007)
//!
//! Generates shell completion scripts for supported shells.

use clap::CommandFactory;
use clap_complete::Shell as ClapShell;

use crate::cli::args::{Cli, CompletionsArgs, Shell};

/// Generate and print a shell completion script to stdout.
pub fn run(args: &CompletionsArgs) {
    let shell = match args.shell {
        Shell::Bash => ClapShell::Bash,
        Shell::Zsh => ClapShell::Zsh,
        Shell::Fish => ClapShell::Fish,
        Shell::PowerShell => ClapShell::PowerShell,
        Shell::Elvish => ClapShell::Elvish,
    };

    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "thoughtjack", &mut std::io::stdout());
}
