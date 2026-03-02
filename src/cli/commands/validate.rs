//! Validate command handler (TJ-SPEC-007 v2)
//!
//! Validates an OATF document without executing it. Optionally prints
//! the pre-processed (normalized) document.

use crate::cli::args::ValidateArgs;
use crate::error::ThoughtJackError;
use crate::loader;

/// Validate an OATF scenario document.
///
/// Loads and validates the OATF document via the SDK. If `--normalize`
/// is set, prints the pre-processed document YAML to stdout.
///
/// # Errors
///
/// Returns an error if the file cannot be read or if validation fails.
///
/// Implements: TJ-SPEC-007 F-004
#[allow(clippy::unused_async)]
pub async fn validate(args: &ValidateArgs, quiet: bool) -> Result<(), ThoughtJackError> {
    let yaml = std::fs::read_to_string(&args.path)?;

    let loaded = loader::load_document(&yaml)?;

    if args.normalize {
        let normalized = serde_yaml::to_string(&loaded.document)
            .map_err(|e| ThoughtJackError::Usage(e.to_string()))?;
        print!("{normalized}");
    } else if !quiet {
        eprintln!("Valid OATF document: {}", args.path.display());
    }

    Ok(())
}
