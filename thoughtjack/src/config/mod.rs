//! Configuration module (TJ-SPEC-001, TJ-SPEC-006)
//!
//! Handles loading and validation of `ThoughtJack` configuration files,
//! including attack scenarios, tool definitions, and runtime settings.

pub mod loader;
pub mod schema;
pub mod validation;

pub use loader::{ConfigLimits, ConfigLoader, LoadResult, LoadWarning, LoaderOptions};
pub use schema::*;
pub use validation::{ValidationResult, Validator};
