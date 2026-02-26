//! Validate command handler (TJ-SPEC-007 v2)
//!
//! Validates an OATF document without executing it.

use crate::cli::args::ValidateArgs;
use crate::error::ThoughtJackError;

/// Validate an OATF scenario document.
///
/// # Errors
///
/// Returns an error if validation fails.
///
/// Implements: TJ-SPEC-007 F-003
#[allow(clippy::unused_async)]
pub async fn validate(_args: &ValidateArgs) -> Result<(), ThoughtJackError> {
    todo!("validate command wired in engine prompt")
}
