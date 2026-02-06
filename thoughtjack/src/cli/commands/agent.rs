//! Agent command handler (TJ-SPEC-007)
//!
//! Placeholder — agent mode is not yet implemented.

use crate::cli::args::AgentCommand;
use crate::error::{PhaseError, ThoughtJackError};

/// Run `ThoughtJack` in agent (MCP client) mode.
///
/// # Errors
///
/// Always returns a phase error indicating this command is not yet implemented.
///
/// Implements: TJ-SPEC-007 F-006
#[allow(clippy::unused_async)] // will use async when agent mode is implemented
pub async fn run(_cmd: &AgentCommand) -> Result<(), ThoughtJackError> {
    eprintln!("agent mode is coming soon");
    Err(ThoughtJackError::Phase(PhaseError::NotFound(
        "agent command not yet implemented".into(),
    )))
}
