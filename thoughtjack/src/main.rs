//! `ThoughtJack` — Adversarial MCP server for security testing

// tokio::select! macro expands to pub(crate) items inside private scope
#![allow(clippy::redundant_pub_crate)]

use clap::Parser;
use tokio_util::sync::CancellationToken;

use thoughtjack::cli::args::{Cli, LogFormatChoice};
use thoughtjack::cli::commands;
use thoughtjack::error::ExitCode;
use thoughtjack::observability::{LogFormat, init_logging};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if !cli.quiet {
        let format = match cli.log_format {
            LogFormatChoice::Human => LogFormat::Human,
            LogFormatChoice::Json => LogFormat::Json,
        };
        init_logging(format, cli.verbose, cli.color);
    }

    // Create a single cancellation token shared across the entire process
    let cancel = CancellationToken::new();
    let cancel_for_signal = cancel.clone();

    // Spawn signal handler for graceful shutdown
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }

        cancel_for_signal.cancel();
        eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => std::process::exit(ExitCode::INTERRUPTED),
            _ = sigterm.recv() => std::process::exit(ExitCode::TERMINATED),
        }
    });

    let result = commands::dispatch(cli, cancel).await;

    match result {
        Ok(()) => std::process::exit(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
