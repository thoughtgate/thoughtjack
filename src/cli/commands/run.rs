//! Run command handler (TJ-SPEC-007 v2)
//!
//! Executes an OATF scenario against a target agent.

use tokio_util::sync::CancellationToken;

use crate::cli::args::RunArgs;
use crate::error::ThoughtJackError;

/// Execute an OATF scenario.
///
/// # Errors
///
/// Returns an error if scenario execution fails.
///
/// Implements: TJ-SPEC-007 F-002
#[allow(clippy::unused_async)]
pub async fn run(_args: &RunArgs, _cancel: CancellationToken) -> Result<(), ThoughtJackError> {
    todo!("run command wired in engine prompt")
}
