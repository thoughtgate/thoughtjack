//! `ThoughtJack` — Adversarial MCP server for security testing

// tokio::select! macro expands to pub(crate) items inside private scope
#![allow(clippy::redundant_pub_crate)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use clap::{Parser, error::ErrorKind};
use tokio_util::sync::CancellationToken;

use thoughtjack::cli::args::{Cli, LogFormatChoice};
use thoughtjack::cli::commands;
use thoughtjack::error::ExitCode;
use thoughtjack::observability::{LogFormat, init_logging};

#[cfg(unix)]
fn register_unix_signals() -> Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)> {
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
    match (sigint, sigterm) {
        (Ok(sigint), Ok(sigterm)) => Some((sigint, sigterm)),
        (sigint_result, sigterm_result) => {
            if let Err(e) = sigint_result {
                eprintln!("warning: failed to register SIGINT handler: {e}");
            }
            if let Err(e) = sigterm_result {
                eprintln!("warning: failed to register SIGTERM handler: {e}");
            }
            None
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let kind = err.kind();
            let _ = err.print();
            match kind {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    std::process::exit(ExitCode::SUCCESS);
                }
                _ => std::process::exit(ExitCode::USAGE_ERROR),
            }
        }
    };

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
    let signal_exit_code = Arc::new(AtomicI32::new(0));
    let signal_exit_code_for_handler = Arc::clone(&signal_exit_code);
    #[cfg(unix)]
    let unix_signals = register_unix_signals();

    // Spawn signal handler for graceful shutdown
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Some((mut sigint, mut sigterm)) = unix_signals {
                tokio::select! {
                    _ = sigint.recv() => {
                        signal_exit_code_for_handler
                            .store(ExitCode::INTERRUPTED, Ordering::SeqCst);
                    }
                    _ = sigterm.recv() => {
                        signal_exit_code_for_handler
                            .store(ExitCode::TERMINATED, Ordering::SeqCst);
                    }
                }

                cancel_for_signal.cancel();
                eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");

                tokio::select! {
                    _ = sigint.recv() => std::process::exit(ExitCode::INTERRUPTED),
                    _ = sigterm.recv() => std::process::exit(ExitCode::TERMINATED),
                }
            } else if tokio::signal::ctrl_c().await.is_ok() {
                // Fallback path if Unix handlers could not be installed.
                signal_exit_code_for_handler.store(ExitCode::INTERRUPTED, Ordering::SeqCst);
                cancel_for_signal.cancel();
                eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");
                if tokio::signal::ctrl_c().await.is_ok() {
                    std::process::exit(ExitCode::INTERRUPTED);
                }
            }
        }

        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_err() {
                eprintln!("warning: failed to register signal handler");
                return;
            }
            signal_exit_code_for_handler.store(ExitCode::INTERRUPTED, Ordering::SeqCst);
            cancel_for_signal.cancel();
            eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");

            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(ExitCode::INTERRUPTED);
            }
        }
    });

    let result = commands::dispatch(cli, cancel).await;
    let signal_code = signal_exit_code.load(Ordering::SeqCst);
    if signal_code != 0 {
        std::process::exit(signal_code);
    }

    match result {
        Ok(()) => std::process::exit(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
