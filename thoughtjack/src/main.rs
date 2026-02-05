//! `ThoughtJack` — Adversarial MCP server for security testing

use clap::Parser;

use thoughtjack::cli::args::Cli;
use thoughtjack::cli::commands;
use thoughtjack::error::ExitCode;
use thoughtjack::observability::{LogFormat, init_logging};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if !cli.quiet {
        init_logging(LogFormat::Human, cli.verbose);
    }

    // Spawn signal handler for graceful shutdown
    tokio::spawn(async {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }

        eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => std::process::exit(ExitCode::INTERRUPTED),
            _ = sigterm.recv() => std::process::exit(ExitCode::TERMINATED),
        }
    });

    let result = commands::dispatch(cli).await;

    match result {
        Ok(()) => std::process::exit(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
