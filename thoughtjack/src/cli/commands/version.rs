//! Version information display (TJ-SPEC-007)
//!
//! Prints version and build metadata in human or JSON format.

use crate::built_info;
use crate::cli::args::{OutputFormat, VersionArgs};

/// Print version and build information.
///
/// Displays package version along with build metadata including git commit,
/// build timestamp, Rust compiler version, and target triple.
///
/// Implements: TJ-SPEC-007 F-008
pub fn run(args: &VersionArgs) {
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");

    // Git info (may not be available in all builds)
    let git_hash = built_info::GIT_COMMIT_HASH_SHORT.unwrap_or("unknown");
    let git_dirty = built_info::GIT_DIRTY.unwrap_or(false);

    // Build info
    let build_time = built_info::BUILT_TIME_UTC;
    let rustc_version = built_info::RUSTC_VERSION;
    let target = built_info::TARGET;

    match args.format {
        OutputFormat::Human => {
            println!("{name} {version}");
            if git_dirty {
                println!("  commit:  {git_hash} (dirty)");
            } else {
                println!("  commit:  {git_hash}");
            }
            println!("  built:   {build_time}");
            println!("  rustc:   {rustc_version}");
            println!("  target:  {target}");
        }
        OutputFormat::Json => {
            println!(
                r#"{{"name":"{name}","version":"{version}","commit":"{git_hash}","dirty":{git_dirty},"built":"{build_time}","rustc":"{rustc_version}","target":"{target}"}}"#,
            );
        }
    }
}
