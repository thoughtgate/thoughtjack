//! Version information display (TJ-SPEC-007)
//!
//! Prints version and build metadata in human or JSON format.

use crate::cli::args::{OutputFormat, VersionArgs};

/// Print version and build information.
pub fn run(args: &VersionArgs) {
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");

    match args.format {
        OutputFormat::Human => {
            println!("{name} {version}");
        }
        OutputFormat::Json => {
            println!(r#"{{"name":"{name}","version":"{version}"}}"#,);
        }
    }
}
