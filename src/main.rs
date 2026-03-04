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

    // Spawn signal handler for graceful shutdown
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());

            match sigterm {
                Ok(mut sigterm) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
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
                        _ = tokio::signal::ctrl_c() => std::process::exit(ExitCode::INTERRUPTED),
                        _ = sigterm.recv() => std::process::exit(ExitCode::TERMINATED),
                    }
                }
                Err(e) => {
                    eprintln!("warning: failed to register SIGTERM handler: {e}");
                    // Fall back to Ctrl+C only
                    if tokio::signal::ctrl_c().await.is_ok() {
                        signal_exit_code_for_handler.store(ExitCode::INTERRUPTED, Ordering::SeqCst);
                        cancel_for_signal.cancel();
                        eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");
                        if tokio::signal::ctrl_c().await.is_ok() {
                            std::process::exit(ExitCode::INTERRUPTED);
                        }
                    }
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
