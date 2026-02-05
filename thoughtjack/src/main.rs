//! `ThoughtJack` — Adversarial MCP server for security testing

use clap::Parser;
use tracing::Level;

use thoughtjack::cli::args::Cli;
use thoughtjack::cli::commands;
use thoughtjack::error::ExitCode;

fn setup_logging(verbose: u8, quiet: bool) {
    let level = if quiet {
        Level::ERROR
    } else {
        match verbose {
            0 => Level::WARN,
            1 => Level::INFO,
            2 => Level::DEBUG,
            _ => Level::TRACE,
        }
    };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(verbose >= 3)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    setup_logging(cli.verbose, cli.quiet);

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
