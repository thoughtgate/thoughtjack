//! Server command handlers (TJ-SPEC-007)
//!
//! Stub implementations for `server run`, `server validate`, and `server list`.

use crate::cli::args::{ServerListArgs, ServerRunArgs, ServerValidateArgs};
use crate::error::ThoughtJackError;

/// Start the adversarial MCP server.
///
/// # Errors
///
/// Returns a usage error if neither `--config` nor `--tool` is provided,
/// or a transport/phase error once the server runtime is wired in.
#[allow(clippy::unused_async)] // will use async when server runtime is wired in
pub async fn run(args: &ServerRunArgs) -> Result<(), ThoughtJackError> {
    // EC-CLI-003: require at least one source
    if args.config.is_none() && args.tool.is_none() {
        return Err(ThoughtJackError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "either --config or --tool is required",
        )));
    }

    tracing::info!("server starting...");

    if let Some(ref path) = args.config {
        tracing::info!(config = %path.display(), "loading configuration");
    }
    if let Some(ref path) = args.tool {
        tracing::info!(tool = %path.display(), "loading single tool definition");
    }

    // TODO: wire up config loader → phase engine → transport loop
    Ok(())
}

/// Validate configuration files without starting the server.
///
/// # Errors
///
/// Returns an I/O error if any file does not exist, or a config error
/// once the full validation pipeline is wired in.
#[allow(clippy::unused_async)] // will use async when config loader is wired in
pub async fn validate(args: &ServerValidateArgs) -> Result<(), ThoughtJackError> {
    for path in &args.files {
        if !path.exists() {
            return Err(ThoughtJackError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("file not found: {}", path.display()),
            )));
        }
        tracing::info!(file = %path.display(), "validating configuration");
    }

    // TODO: wire up config loader + validation
    Ok(())
}

/// List available attack patterns from the library.
///
/// # Errors
///
/// Returns an I/O error if the library directory is inaccessible.
#[allow(clippy::unused_async)] // will use async when library scanning is wired in
pub async fn list(args: &ServerListArgs) -> Result<(), ThoughtJackError> {
    tracing::info!(category = ?args.category, "listing library items");

    // TODO: scan library directory and render output
    Ok(())
}
