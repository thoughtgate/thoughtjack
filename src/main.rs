//! `ThoughtJack` — Adversarial MCP server for security testing

// tokio::select! macro expands to pub(crate) items inside private scope
#![allow(clippy::redundant_pub_crate)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use clap::{Parser, error::ErrorKind};
use tokio_util::sync::CancellationToken;

use thoughtjack::cli::args::{Cli, LogFormatChoice};
use thoughtjack::cli::commands;
use thoughtjack::error::{ExitCode, ThoughtJackError};
use thoughtjack::observability::{LogFormat, init_logging};

#[cfg(unix)]
struct UnixSignals {
    sigint: Option<tokio::signal::unix::Signal>,
    sigterm: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl UnixSignals {
    const fn is_empty(&self) -> bool {
        self.sigint.is_none() && self.sigterm.is_none()
    }
}

#[cfg(unix)]
fn register_unix_signals() -> UnixSignals {
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());

    let sigint = match sigint {
        Ok(signal) => Some(signal),
        Err(e) => {
            eprintln!("warning: failed to register SIGINT handler: {e}");
            None
        }
    };
    let sigterm = match sigterm {
        Ok(signal) => Some(signal),
        Err(e) => {
            eprintln!("warning: failed to register SIGTERM handler: {e}");
            None
        }
    };

    UnixSignals { sigint, sigterm }
}

#[cfg(unix)]
async fn recv_unix_signal(signals: &mut UnixSignals) -> Option<i32> {
    match (&mut signals.sigint, &mut signals.sigterm) {
        (Some(sigint), Some(sigterm)) => {
            tokio::select! {
                received = sigint.recv() => received.map(|()| ExitCode::INTERRUPTED),
                received = sigterm.recv() => received.map(|()| ExitCode::TERMINATED),
            }
        }
        (Some(sigint), None) => sigint.recv().await.map(|()| ExitCode::INTERRUPTED),
        (None, Some(sigterm)) => sigterm.recv().await.map(|()| ExitCode::TERMINATED),
        (None, None) => None,
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
            let mut unix_signals = unix_signals;
            if unix_signals.is_empty() {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_exit_code_for_handler.store(ExitCode::INTERRUPTED, Ordering::SeqCst);
                    cancel_for_signal.cancel();
                    eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");
                    if tokio::signal::ctrl_c().await.is_ok() {
                        std::process::exit(ExitCode::INTERRUPTED);
                    }
                }
            } else if let Some(exit_code) = recv_unix_signal(&mut unix_signals).await {
                signal_exit_code_for_handler.store(exit_code, Ordering::SeqCst);
                cancel_for_signal.cancel();
                eprintln!("\nShutting down gracefully... (send signal again to force)");
                if let Some(force_exit_code) = recv_unix_signal(&mut unix_signals).await {
                    std::process::exit(force_exit_code);
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
        Err(ThoughtJackError::Verdict { code, .. }) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
