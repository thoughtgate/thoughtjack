//! Validate command handler (TJ-SPEC-007 v2)
//!
//! Validates an OATF document without executing it. Optionally prints
//! the pre-processed (normalized) document.

use crate::cli::args::ValidateArgs;
use crate::error::ThoughtJackError;
use crate::loader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateOutputMode {
    Silent,
    Normalized,
    Status,
}

const fn output_mode(normalize: bool, quiet: bool) -> ValidateOutputMode {
    if quiet {
        ValidateOutputMode::Silent
    } else if normalize {
        ValidateOutputMode::Normalized
    } else {
        ValidateOutputMode::Status
    }
}

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

    match output_mode(args.normalize, quiet) {
        ValidateOutputMode::Silent => {}
        ValidateOutputMode::Normalized => {
            let normalized = serde_yaml::to_string(&loaded.document)
                .map_err(|e| ThoughtJackError::Usage(e.to_string()))?;
            print!("{normalized}");
        }
        ValidateOutputMode::Status => {
            eprintln!("Valid OATF document: {}", args.path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_honors_quiet() {
        assert_eq!(output_mode(false, true), ValidateOutputMode::Silent);
        assert_eq!(output_mode(true, true), ValidateOutputMode::Silent);
    }

    #[test]
    fn output_mode_normalized_when_not_quiet() {
        assert_eq!(output_mode(true, false), ValidateOutputMode::Normalized);
    }

    #[test]
    fn output_mode_status_when_not_quiet() {
        assert_eq!(output_mode(false, false), ValidateOutputMode::Status);
    }
}
