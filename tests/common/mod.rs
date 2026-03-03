//! Shared integration-test helpers for `ThoughtJack`.
//!
//! Provides utility functions for v0.5 integration tests.

#![allow(dead_code)]

pub mod mock_server;

use std::path::PathBuf;

/// Returns the path to a test fixture.
#[must_use]
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
